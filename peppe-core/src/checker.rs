//! Verificador semântico — primeira passada: tabela de símbolos
//! e resolução de declarações de nível superior (`const`/`tipo`/`var`/
//! `procedimento`/`função`).
//!
//! ## Arquitetura
//!
//! O verificador roda em duas sub-passadas sobre [`Programa::declaracoes`]:
//!
//! 1. [`Verificador::coletar_tipos`] — percorre **todas** as declarações
//!    `tipo NOME = ...`, em qualquer nível (incluindo dentro de
//!    sub-rotinas), e monta uma tabela `nome em minúsculas -> (grafia
//!    original, definição)`. Isso permite que tipos sejam referenciados
//!    antes de sua declaração textual (ex.: `CAD_ALUNO` usando `BIMESTRE`
//!    definido depois) e detecta nomes de tipo duplicados.
//!
//!    Todos os `tipo` do programa entram em uma única tabela "global",
//!    mesmo os declarados dentro de uma sub-rotina — os exemplos do livro
//!    não declaram tipos locais, então essa simplificação não causa
//!    ambiguidade na prática.
//!
//! 2. [`Verificador::processar_declaracoes`] — percorre as declarações na
//!    ordem do código, desta vez construindo a [`TabelaSimbolos`]
//!    (escopos aninhados, *case-insensitive* ): `const`/`var`
//!    têm seu tipo resolvido e são declarados no escopo atual; `tipo` é
//!    declarado (para detecção de colisão de nomes); `procedimento`/
//!    `função` têm a assinatura resolvida, são declarados no escopo atual e
//!    abrem um novo escopo para seus parâmetros e declarações locais
//!    (recursivamente , sub-rotinas aninhadas). Ao final de cada
//!    sub-rotina (e do programa principal), [`Verificador::verificar_bloco`]
//!    verifica os comandos do `corpo`/`bloco_principal` com o escopo já
//!    montado.
//!
//! A verificação de comandos/expressões ([`Verificador::verificar_comando`]
//! / [`Verificador::tipo_de_expr`]) cobre: existência e categoria correta
//! de identificadores usados, compatibilidade de tipos em atribuições e
//! argumentos de chamada (via `tipos::compatibilidade`), tipo resultante de
//! operadores (via `tipos::tipo_resultado_binario`/`tipo_resultado_unario`),
//! acesso a campo/índice válido para o tipo correspondente, condições de
//! `se`/laços do tipo `lógico`, `interrompa`/`saia_caso` apenas dentro de
//! um laço, e `ir_para`/rótulos: cada `corpo` de sub-rotina e o
//! `bloco_principal` têm seus rótulos coletados primeiro (escopo de
//! rótulo = a sub-rotina/programa inteiro, não o bloco aninhado onde o
//! rótulo aparece — mesmo modelo de BASIC/Pascal clássicos), com detecção
//! de rótulo duplicado; `ir_para` então verifica que o rótulo referenciado
//! existe nesse mesmo conjunto (não pode saltar para dentro de outra
//! sub-rotina).

use crate::ast::*;
use crate::tipos::{
    compatibilidade, compatibilidade_com_heranca, e_subclasse_de, resolver_tipo,
    tipo_resultado_binario, tipo_resultado_unario, Compatibilidade, ErroResolucaoTipo,
    TipoResolvido,
};
use std::collections::HashMap;

// =====================================================================================
// Erros semânticos
// =====================================================================================

/// Um erro semântico — sem coluna (a AST guarda apenas `linha`).
/// O verificador acumula **todos** os erros encontrados, em vez de parar no
/// primeiro ("reporta todos os erros estáticos de uma vez").
#[derive(Debug, Clone, PartialEq)]
pub struct ErroSemantico {
    pub linha: usize,
    pub mensagem: String,
}

impl std::fmt::Display for ErroSemantico {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Erro semântico, linha {}: {}", self.linha, self.mensagem)
    }
}

// =====================================================================================
// Tabela de símbolos
// =====================================================================================

/// Assinatura de um `procedimento`/`função`, com tipos já resolvidos.
#[derive(Debug, Clone, PartialEq)]
pub struct AssinaturaSubRotina {
    pub categoria: CategoriaSubRotina,
    pub parametros: Vec<ParametroResolvido>,
    /// `Some(tipo)` para `função`, `None` para `procedimento`.
    pub tipo_retorno: Option<TipoResolvido>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParametroResolvido {
    pub nomes: Vec<String>,
    pub tipo: TipoResolvido,
    pub por_referencia: bool,
}

/// O que um identificador representa, com seu tipo (já resolvido).
#[derive(Debug, Clone, PartialEq)]
pub enum CategoriaSimbolo {
    Const(TipoResolvido),
    /// Apenas para detecção de colisão de nomes — o tipo em si vive na
    /// tabela de tipos ([`Verificador::tabela_tipos`]), consultada via
    /// [`resolver_tipo`].
    Tipo(TipoResolvido),
    Var(TipoResolvido),
    /// Uma ou mais assinaturas para o mesmo nome (sobrecarga
    /// ad-hoc): `CALCULAR(X:inteiro)`, `CALCULAR(R,H:real)` e
    /// `CALCULAR(X,Y,Z:inteiro)` compartilham uma única entrada na tabela
    /// de símbolos, com três elementos neste vetor. O caso comum (nome
    /// declarado uma única vez) é só um vetor de tamanho 1 — todo o
    /// código que assumia uma assinatura única precisa, a partir daqui,
    /// escolher a candidata certa (ver
    /// [`Verificador::resolver_sobrecarga`]).
    SubRotina(Vec<AssinaturaSubRotina>),
}

impl CategoriaSimbolo {
    /// Descrição em português, para mensagens de erro. Para
    /// `SubRotina` com múltiplas assinaturas (sobrecarga),
    /// usa a categoria da primeira — todas as sobrecargas de um mesmo
    /// nome compartilham a mesma categoria (procedimento ou função; ver
    /// [`Verificador::pode_sobrecarregar`], que impede misturar as duas).
    pub fn descricao(&self) -> &'static str {
        match self {
            CategoriaSimbolo::Const(_) => "uma constante",
            CategoriaSimbolo::Tipo(_) => "um tipo",
            CategoriaSimbolo::Var(_) => "uma variável",
            CategoriaSimbolo::SubRotina(assinaturas) => {
                match assinaturas.first().map(|a| a.categoria) {
                    Some(CategoriaSubRotina::Procedimento) | None => "um procedimento",
                    Some(CategoriaSubRotina::Funcao) => "uma função",
                }
            }
        }
    }
}

/// Um identificador declarado: categoria + onde foi declarado, com a grafia
/// original (para mensagens de erro  preserva a grafia da
/// primeira declaração).
#[derive(Debug, Clone, PartialEq)]
pub struct Simbolo {
    pub nome_original: String,
    pub categoria: CategoriaSimbolo,
    pub linha_declaracao: usize,
}

/// Pilha de escopos *case-insensitive*: índice 0 é o escopo
/// global; o último é o escopo atual. A busca de identificadores percorre
/// do escopo atual até o global (lexical scoping — sub-rotinas aninhadas
/// veem o escopo de quem as contém).
#[derive(Debug, Clone)]
pub struct TabelaSimbolos {
    escopos: Vec<HashMap<String, Simbolo>>,
}

impl TabelaSimbolos {
    pub fn novo() -> Self {
        TabelaSimbolos { escopos: vec![HashMap::new()] }
    }

    pub fn entrar_escopo(&mut self) {
        self.escopos.push(HashMap::new());
    }

    pub fn sair_escopo(&mut self) {
        self.escopos.pop();
        debug_assert!(!self.escopos.is_empty(), "o escopo global nunca deve ser removido");
    }

    /// Declara `nome_original` no escopo **atual**. Erro se outro
    /// identificador com o mesmo nome (ignorando maiúsculas/minúsculas)
    /// já existir nesse mesmo escopo; *shadowing* de um escopo
    /// externo é permitido (é o comportamento normal de variáveis locais).
    pub fn declarar(
        &mut self,
        nome_original: &str,
        categoria: CategoriaSimbolo,
        linha: usize,
    ) -> Result<(), ErroSemantico> {
        let chave = nome_original.to_lowercase();
        let escopo = self.escopos.last_mut().expect("sempre há ao menos o escopo global");

        if let Some(existente) = escopo.get(&chave) {
            let nota_case = if existente.nome_original != nome_original {
                format!(
                    " Note que '{}' e '{}' são o mesmo identificador em PEPPE \
                     (maiúsculas/minúsculas não importam ).",
                    existente.nome_original, nome_original
                )
            } else {
                String::new()
            };
            return Err(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{}' já foi declarado como {} na linha {}.{}",
                    nome_original,
                    existente.categoria.descricao(),
                    existente.linha_declaracao,
                    nota_case
                ),
            });
        }

        escopo.insert(
            chave,
            Simbolo { nome_original: nome_original.to_string(), categoria, linha_declaracao: linha },
        );
        Ok(())
    }

    /// Busca `nome` do escopo atual até o global (case-insensitive).
    pub fn buscar(&self, nome: &str) -> Option<&Simbolo> {
        let chave = nome.to_lowercase();
        self.escopos.iter().rev().find_map(|escopo| escopo.get(&chave))
    }

    /// Como [`Self::buscar`], mas só no escopo **atual** (não sobe para
    /// escopos pais) — necessário para sobrecarga: uma
    /// segunda sub-rotina com o mesmo nome só se acumula às anteriores
    /// quando ambas estão no mesmo escopo; se a primeira estiver num
    /// escopo pai, a nova é uma declaração independente que apenas
    /// sombreia a de fora (mesma regra de qualquer outro identificador).
    pub fn buscar_no_escopo_atual(&self, nome: &str) -> Option<&Simbolo> {
        let chave = nome.to_lowercase();
        self.escopos.last().expect("sempre há ao menos o escopo global").get(&chave)
    }

    /// Busca `nome` ignorando o escopo mais interno (o atual) — usado
    /// para chamada recursiva de função pelo próprio nome: dentro do
    /// corpo de `FATORIAL`, o escopo atual declara `FATORIAL` como `Var`
    /// (variável de retorno); nos escopos externos está a
    /// `SubRotina` em si. Pulando o escopo atual, encontramos a
    /// sub-rotina e permitimos a chamada recursiva (estilo Pascal).
    pub fn buscar_em_escopos_externos(&self, nome: &str) -> Option<&Simbolo> {
        let chave = nome.to_lowercase();
        // `iter().rev()` percorre do mais interno ao mais externo;
        // `.skip(1)` pula o escopo atual.
        self.escopos.iter().rev().skip(1).find_map(|escopo| escopo.get(&chave))
    }

    /// Substitui a categoria do símbolo `nome_original` já existente no
    /// escopo **atual** — usado exclusivamente para acrescentar uma nova
    /// assinatura a um `CategoriaSimbolo::SubRotina` já declarado
    /// (sobrecarga). Não altera `nome_original`/
    /// `linha_declaracao` (a mensagem de erro de qualquer problema
    /// continua citando a primeira declaração). Painc se `nome` não
    /// existir no escopo atual — só deve ser chamado depois de confirmar
    /// via [`Self::buscar_no_escopo_atual`].
    pub fn substituir_categoria_no_escopo_atual(&mut self, nome: &str, categoria: CategoriaSimbolo) {
        let chave = nome.to_lowercase();
        let escopo = self.escopos.last_mut().expect("sempre há ao menos o escopo global");
        let simbolo = escopo
            .get_mut(&chave)
            .expect("chamador deve garantir que o símbolo já existe no escopo atual");
        simbolo.categoria = categoria;
    }

    /// Número de escopos abertos (1 = apenas o global).
    pub fn profundidade(&self) -> usize {
        self.escopos.len()
    }
}

impl Default for TabelaSimbolos {
    fn default() -> Self {
        Self::novo()
    }
}

// =====================================================================================
// Verificador
// =====================================================================================

/// Resultado da verificação semântica: a tabela de símbolos global (após
/// processar todas as declarações de nível superior — todos os escopos de
/// sub-rotinas já foram fechados) e a lista de erros encontrados.
///
/// `erros.is_empty()` significa que o programa passou a verificação.
pub struct ResultadoVerificacao {
    pub tabela_global: TabelaSimbolos,
    pub erros: Vec<ErroSemantico>,
}

/// Executa o verificador semântico sobre `programa`: declarações de nível
/// superior (tabela de símbolos) e, em seguida, os comandos do
/// `bloco_principal`.
pub fn verificar(programa: &Programa) -> ResultadoVerificacao {
    let mut v = Verificador {
        tabela: TabelaSimbolos::novo(),
        tabela_tipos: HashMap::new(),
        erros: Vec::new(),
        profundidade_laco: 0,
        rotulos_validos: std::collections::HashSet::new(),
        info_classes: HashMap::new(),
        tabela_heranca: HashMap::new(),
        classe_atual: None,
    };
    v.coletar_tipos(&programa.declaracoes);
    v.coletar_classes(&programa.declaracoes);
    v.validar_sobrecargas_de_metodo();
    v.validar_overrides();
    v.validar_implementacao_de_metodos(&programa.declaracoes);
    v.processar_declaracoes(&programa.declaracoes);
    v.verificar_bloco_de_subrotina(&programa.bloco_principal, None);
    debug_assert_eq!(
        v.tabela.profundidade(),
        1,
        "todos os escopos de sub-rotinas devem ter sido fechados"
    );
    ResultadoVerificacao { tabela_global: v.tabela, erros: v.erros }
}

// =====================================================================================
// Classes — informação coletada do verificador semântico
// =====================================================================================

/// Resultado de resolver um nome de campo/método através da árvore de
/// herança (múltiplas bases diretas por classe, sem herança virtual).
/// Generaliza o que seria um simples `Option` quando só
/// havia uma cadeia linear de herança possível.
enum ResolucaoMembro<T> {
    /// Achado numa única classe (`String` = nome dela, em minúsculas) —
    /// inclui o caso "a própria classe consultada declara o nome
    /// diretamente" (que sempre tem prioridade sobre qualquer base,
    /// mesmo que uma base também declare o mesmo nome — análogo a um
    /// campo redefinido na derivada "escondendo" o da base em C++, sem
    /// gerar ambiguidade).
    Encontrado(T, String),
    /// O nome existe em mais de uma base direta (ou foi herdado por
    /// mais de um caminho da árvore) sem que a própria classe consultada
    /// o declare diretamente — `Vec<String>` lista as classes onde foi
    /// encontrado, em minúsculas, para a mensagem de erro sugerir a
    /// qualificação `CLS_BASE..NOME`.
    Ambiguo(Vec<String>),
    NaoEncontrado,
}

/// Um campo de instância, já com tipo resolvido e visibilidade (seção
/// 10.1/10.4).
#[derive(Debug, Clone, PartialEq)]
struct InfoCampo {
    nome: String,
    tipo: TipoResolvido,
    visibilidade: Visibilidade,
}

/// Um método de classe, já com assinatura resolvida e visibilidade
///. `implementado` é preenchido por
/// [`Verificador::validar_implementacao_de_metodos`], depois que toda
/// declaração de nível superior (incluindo `MetodoExterno`) já foi vista —
/// método sem implementação em nenhum lugar é erro semântico.
#[derive(Debug, Clone, PartialEq)]
struct InfoMetodo {
    nome: String,
    assinatura: AssinaturaSubRotina,
    visibilidade: Visibilidade,
    /// `virtual`/`sobrepor`/nenhum — usado para validar a
    /// relação entre um método `sobrepor` e o `virtual` correspondente
    /// na cadeia de herança, e para decidir dispatch dinâmico vs.
    /// estático em tempo de execução.
    modificador: Modificador,
    linha_assinatura: usize,
    implementado: bool,
}

/// Informação completa de uma classe, montada na passada de coleta
/// (`coletar_classes`) a partir de `tipo NOME = classe ... fim_classe`
///.
#[derive(Debug, Clone, PartialEq)]
struct InfoClasse {
    nome: String,
    /// Nomes das classes-base **diretas** (`classe herança de X[, de
    /// Y, ...]`) — vazio se não houver, um elemento para
    /// herança simples, dois ou mais para herança múltipla.
    heranca: Vec<String>,
    campos: Vec<InfoCampo>,
    metodos: Vec<InfoMetodo>,
    linha: usize,
}

impl InfoClasse {
    fn campo(&self, nome: &str) -> Option<&InfoCampo> {
        self.campos.iter().find(|c| c.nome.eq_ignore_ascii_case(nome))
    }

    /// **Todas** as sobrecargas de `nome` na classe — uma
    /// chamada de método precisa ver todas as candidatas para escolher a
    /// certa pelos tipos dos argumentos, não só a primeira declarada.
    fn metodos_por_nome(&self, nome: &str) -> Vec<&InfoMetodo> {
        self.metodos.iter().filter(|m| m.nome.eq_ignore_ascii_case(nome)).collect()
    }
}

struct Verificador {
    tabela: TabelaSimbolos,
    /// nome em minúsculas -> (grafia original, definição AST). Ver nota de
    /// simplificação no doc do módulo.
    tabela_tipos: HashMap<String, (String, Tipo)>,
    erros: Vec<ErroSemantico>,
    /// Quantos laços (`enquanto`/`até_seja`/`repita`/`execute`/`laço`/`para`)
    /// envolvem o comando atual — usado para validar que `interrompa` e
    /// `saia_caso` só aparecem dentro de um laço.
    profundidade_laco: usize,
    /// Conjunto de rótulos (nome em minúsculas) declarados em **toda** a
    /// sub-rotina/programa atualmente sendo verificado — coletado uma vez,
    /// recursivamente em qualquer nível de bloco aninhado (`se`/laços/
    /// `caso`), por [`Verificador::coletar_rotulos`]. `ir_para`
    /// pode saltar para qualquer rótulo deste conjunto: rótulos não
    /// atravessam fronteiras de sub-rotina (cada `função`/`procedimento`
    /// tem seu próprio espaço de rótulos), mas podem atravessar blocos
    /// `se`/laços dentro da mesma sub-rotina — é assim que o material de
    /// origem usa `ir_para` (ex.: simular saída antecipada de uma
    /// estrutura aninhada), e casa com a implementação em
    /// `interpreter.rs`.
    rotulos_validos: std::collections::HashSet<String>,
    /// nome de classe em minúsculas -> informação completa (campos,
    /// métodos, herança), montada por [`Verificador::coletar_classes`]
    ///.
    info_classes: HashMap<String, InfoClasse>,
    /// nome de classe em minúsculas -> nomes das classes-base diretas
    /// (vazio se não houver). Derivado de `info_classes`, mantido
    /// separado por já estar no formato que [`compatibilidade_com_heranca`]
    /// espera.
    tabela_heranca: HashMap<String, Vec<String>>,
    /// Nome (em minúsculas) da classe cujo corpo de método está sendo
    /// verificado agora, ou `None` fora de qualquer método (seção
    /// 10.4.1) — usado para decidir se um acesso a campo/método
    /// `seção_privada`/`seção_protegida` é permitido. `None` enquanto
    /// processamos o bloco principal do programa ou uma sub-rotina
    /// solta (não-método).
    classe_atual: Option<String>,
}

impl Verificador {
    // -- Passada 1: coleta de `tipo NOME = ...` ------------------------------------

    fn coletar_tipos(&mut self, declaracoes: &[DeclaracaoTopo]) {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::Tipo(t) => {
                    let chave = t.nome.to_lowercase();
                    if let Some((nome_existente, _)) = self.tabela_tipos.get(&chave) {
                        self.erros.push(ErroSemantico {
                            linha: t.linha,
                            mensagem: format!(
                                "o tipo '{}' já foi definido anteriormente (como '{}'). \
                                 Cada nome de tipo só pode ser definido uma vez com 'tipo' \
                                 (PEPPE é case-insensitive ).",
                                t.nome, nome_existente
                            ),
                        });
                    } else {
                        self.tabela_tipos.insert(chave, (t.nome.clone(), t.definicao.clone()));
                    }
                }
                DeclaracaoTopo::SubRotina(s) => self.coletar_tipos(&s.declaracoes_locais),
                DeclaracaoTopo::Const(_) | DeclaracaoTopo::Var(_) => {}
                DeclaracaoTopo::MetodoExterno { .. } => {}
            }
        }
    }

    /// Percorre todas as `tipo NOME = classe ... fim_classe` (qualquer
    /// nível, mesma simplificação de `coletar_tipos`) e monta
    /// `info_classes`/`tabela_heranca`. Roda depois de
    /// `coletar_tipos` (precisa de `tabela_tipos` completa, para resolver
    /// o tipo de cada campo) e antes de `validar_implementacao_de_metodos`
    /// e `processar_declaracoes`.
    fn coletar_classes(&mut self, declaracoes: &[DeclaracaoTopo]) {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::Tipo(t) => {
                    if let Tipo::Classe { heranca, membros } = &t.definicao {
                        let mut campos = Vec::new();
                        let mut metodos = Vec::new();
                        for membro in membros {
                            self.coletar_membro_classe(membro, &mut campos, &mut metodos, t.linha);
                        }
                        let chave = t.nome.to_lowercase();
                        self.tabela_heranca.insert(chave.clone(), heranca.clone());
                        self.info_classes.insert(
                            chave,
                            InfoClasse {
                                nome: t.nome.clone(),
                                heranca: heranca.clone(),
                                campos,
                                metodos,
                                linha: t.linha,
                            },
                        );
                    }
                }
                DeclaracaoTopo::SubRotina(s) => self.coletar_classes(&s.declaracoes_locais),
                DeclaracaoTopo::Const(_) | DeclaracaoTopo::Var(_) => {}
                DeclaracaoTopo::MetodoExterno { .. } => {}
            }
        }
    }

    /// Processa um único [`MembroClasse`] durante `coletar_classes`,
    /// adicionando a `campos` ou `metodos` conforme o caso. `linha_classe`
    /// é usada como fallback de linha para mensagens de erro de resolução
    /// de tipo de campo (a própria declaração de campo não carrega
    /// `linha` — segue a forma de [`DeclaracaoVar`] dentro de `registro`,
    /// que tem o mesmo formato).
    fn coletar_membro_classe(
        &mut self,
        membro: &MembroClasse,
        campos: &mut Vec<InfoCampo>,
        metodos: &mut Vec<InfoMetodo>,
        linha_classe: usize,
    ) {
        match &membro.item {
            ItemClasse::Campo(decl_var) => match resolver_tipo(&decl_var.tipo, &self.tabela_tipos) {
                Ok(tipo_resolvido) => {
                    for nome in &decl_var.nomes {
                        campos.push(InfoCampo {
                            nome: nome.clone(),
                            tipo: tipo_resolvido.clone(),
                            visibilidade: membro.visibilidade,
                        });
                    }
                }
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(&decl_var.tipo).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, decl_var.linha));
                }
            },
            ItemClasse::AssinaturaMetodo { categoria, nome, parametros, tipo_retorno, modificador, linha } => {
                let assinatura = self.resolver_assinatura(*categoria, parametros, tipo_retorno);
                metodos.push(InfoMetodo {
                    nome: nome.clone(),
                    assinatura,
                    visibilidade: membro.visibilidade,
                    modificador: *modificador,
                    linha_assinatura: *linha,
                    implementado: false,
                });
            }
            ItemClasse::MetodoInterno(sub, modificador) => {
                let assinatura = self.resolver_assinatura(sub.categoria, &sub.parametros, &sub.tipo_retorno);
                metodos.push(InfoMetodo {
                    nome: sub.nome.clone(),
                    assinatura,
                    visibilidade: membro.visibilidade,
                    modificador: *modificador,
                    linha_assinatura: sub.linha,
                    implementado: true,
                });
            }
        }
        let _ = linha_classe; // reservado para uso futuro (mensagens mais ricas)
    }

    /// Resolve os tipos de uma assinatura de método (categoria, parâmetros,
    /// tipo de retorno) para [`AssinaturaSubRotina`] — mesma lógica usada
    /// para sub-rotinas de nível superior, reaproveitada aqui.
    /// Erros de tipo não resolvido em parâmetro/retorno são reportados
    /// (com `TipoResolvido::Generico` como valor de fallback, para não
    /// interromper a coleta).
    fn resolver_assinatura(
        &mut self,
        categoria: CategoriaSubRotina,
        parametros: &[Parametro],
        tipo_retorno: &Option<Tipo>,
    ) -> AssinaturaSubRotina {
        let mut parametros_resolvidos = Vec::new();
        for p in parametros {
            match resolver_tipo(&p.tipo, &self.tabela_tipos) {
                Ok(tipo) => parametros_resolvidos.push(ParametroResolvido {
                    nomes: p.nomes.clone(),
                    tipo,
                    por_referencia: p.por_referencia,
                }),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(&p.tipo).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, 0));
                    parametros_resolvidos.push(ParametroResolvido {
                        nomes: p.nomes.clone(),
                        tipo: TipoResolvido::Generico,
                        por_referencia: p.por_referencia,
                    });
                }
            }
        }
        let tipo_retorno_resolvido = match tipo_retorno {
            Some(t) => match resolver_tipo(t, &self.tabela_tipos) {
                Ok(tipo) => Some(tipo),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(t).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, 0));
                    Some(TipoResolvido::Generico)
                }
            },
            None => None,
        };
        AssinaturaSubRotina {
            categoria,
            parametros: parametros_resolvidos,
            tipo_retorno: tipo_retorno_resolvido,
        }
    }

    /// Valida sobrecarga ad-hoc entre métodos da **mesma**
    /// classe: dois métodos com o mesmo nome só podem coexistir se (a)
    /// tiverem a mesma categoria (`procedimento`/`função` não se
    /// misturam sob o mesmo nome — mesma regra de sub-rotinas soltas,
    /// ver [`Verificador::declarar_subrotina_com_sobrecarga`]) e (b)
    /// listas de tipos de parâmetro diferentes entre si. Roda junto de
    /// `validar_overrides`, logo depois de `coletar_classes` — assim
    /// uma colisão de sobrecarga é reportada mesmo que o corpo dos
    /// métodos tenha outros problemas.
    ///
    /// Nota: esta validação é agnóstica ao [`Modificador`] de cada
    /// método — um `Sobrepor`/`Virtual` participando de uma sobrecarga
    /// (outro método de mesmo nome, assinatura diferente, na mesma
    /// classe) é permitido; a obrigação de assinatura **idêntica** ao
    /// `virtual` da base é checada separadamente em
    /// `validar_overrides`, e não impede que outras sobrecargas do
    /// mesmo nome existam na classe.
    fn validar_sobrecargas_de_metodo(&mut self) {
        for info in self.info_classes.values() {
            let mut vistos: Vec<&InfoMetodo> = Vec::new();
            for metodo in &info.metodos {
                for anterior in &vistos {
                    if !anterior.nome.eq_ignore_ascii_case(&metodo.nome) {
                        continue;
                    }
                    if anterior.assinatura.categoria != metodo.assinatura.categoria {
                        self.erros.push(ErroSemantico {
                            linha: metodo.linha_assinatura,
                            mensagem: format!(
                                "'{}' já foi declarado como {} em '{}' (linha {}) — uma \
                                 sobrecarga  precisa manter a mesma categoria \
                                 ('procedimento' ou 'função') em todas as versões.",
                                metodo.nome,
                                if anterior.assinatura.categoria == CategoriaSubRotina::Funcao {
                                    "função"
                                } else {
                                    "procedimento"
                                },
                                info.nome,
                                anterior.linha_assinatura
                            ),
                        });
                    } else if mesma_lista_de_tipos(&anterior.assinatura, &metodo.assinatura) {
                        self.erros.push(ErroSemantico {
                            linha: metodo.linha_assinatura,
                            mensagem: format!(
                                "'{}' já foi declarado com a mesma quantidade e tipos de \
                                 parâmetro em '{}' (linha {}) — duas sobrecargas  \
                                 precisam ter aridade ou tipos de parâmetro diferentes.",
                                metodo.nome, info.nome, anterior.linha_assinatura
                            ),
                        });
                    }
                }
                vistos.push(metodo);
            }
        }
    }

    /// Valida o uso de `virtual`/`sobrepor` em todas as
    /// classes já coletadas (`info_classes`). Roda logo depois de
    /// `coletar_classes`, antes de qualquer outra verificação que
    /// dependa de métodos — assim os erros de override aparecem mesmo
    /// que o corpo dos métodos tenha outros problemas.
    ///
    /// Regras:
    /// 1. Um método `Modificador::Sobrepor` precisa ter, na classe-base
    ///    **direta**, um método de mesmo nome com `Modificador::Virtual`
    ///    ou `Modificador::Sobrepor` e assinatura idêntica (mesma
    ///    categoria, mesma aridade e tipos de parâmetro — nomes de
    ///    parâmetro não importam —, mesmo tipo de retorno). Qualquer
    ///    divergência é erro semântico didático.
    /// 2. Um método com `Modificador::Nenhum` cujo nome e assinatura
    ///    coincidem com um `virtual`/`sobrepor` da base é erro — força o
    ///    programador a escrever `sobrepor` explicitamente quando a
    ///    intenção é redefinir, em vez de criar uma sobrecarga acidental
    ///    com o mesmo nome.
    fn validar_overrides(&mut self) {
        let nomes_classes: Vec<String> = self.info_classes.keys().cloned().collect();
        for chave in nomes_classes {
            let info = self.info_classes[&chave].clone();
            if info.heranca.is_empty() {
                continue;
            }
            for metodo in &info.metodos {
                // Busca o correspondente em CADA base direta e agrega —
                // mesma lógica de ambiguidade de 'resolver_em_bases',
                // mas partindo das bases (não da própria 'info', que é
                // quem declara 'metodo' e não deveria se encontrar).
                let mut encontrados: Vec<(InfoMetodo, String)> = Vec::new();
                let mut ambiguo_em: Vec<String> = Vec::new();
                for base in &info.heranca {
                    match self.buscar_metodo_com_heranca(base, &metodo.nome) {
                        ResolucaoMembro::Encontrado(candidatos, doadora) => {
                            // Sobrepor não participa de sobrecarga
                            // (exige assinatura idêntica) —
                            // qualquer candidata de mesmo nome na base
                            // já é o suficiente para localizar "o"
                            // virtual correspondente; se houver mais de
                            // uma sobrecarga com esse nome na base, a
                            // verificação de assinatura abaixo vai
                            // rejeitar a que não bater.
                            for c in candidatos {
                                encontrados.push((c, doadora.clone()));
                            }
                        }
                        ResolucaoMembro::Ambiguo(mut doadoras) => ambiguo_em.append(&mut doadoras),
                        ResolucaoMembro::NaoEncontrado => {}
                    }
                }
                // Entre as encontradas, prioriza uma com assinatura
                // idêntica (o caso comum, herança simples ou múltipla
                // sem colisão de nome); só reporta ambiguidade de fato
                // se nenhuma bater E houver mais de uma base com o
                // nome — caso contrário, a mensagem de "assinatura
                // diferente"/"não é virtual" abaixo já é suficiente.
                let compativel = encontrados
                    .iter()
                    .find(|(m, _)| assinaturas_compativeis_para_override(&metodo.assinatura, &m.assinatura));
                let correspondente_na_base = compativel.or(encontrados.first());
                let nomes_doadoras: Vec<String> =
                    encontrados.iter().map(|(_, d)| d.clone()).chain(ambiguo_em).collect();
                let mut nomes_doadoras_unicos = nomes_doadoras.clone();
                nomes_doadoras_unicos.sort();
                nomes_doadoras_unicos.dedup();

                if metodo.modificador == Modificador::Sobrepor
                    && correspondente_na_base.is_none()
                    && nomes_doadoras_unicos.len() > 1
                {
                    self.erros.push(ErroSemantico {
                        linha: metodo.linha_assinatura,
                        mensagem: format!(
                            "'{}' usa 'sobrepor', mas '{}' existe em mais de uma classe-base \
                             ({}) com assinaturas diferentes entre si — não há um único \
                             'virtual' correspondente para sobrepor (Fase 6 — herança \
                             múltipla).",
                            metodo.nome,
                            metodo.nome,
                            nomes_doadoras_unicos.join(", ")
                        ),
                    });
                    continue;
                }

                match metodo.modificador {
                    Modificador::Sobrepor => match correspondente_na_base {
                        None => {
                            self.erros.push(ErroSemantico {
                                linha: metodo.linha_assinatura,
                                mensagem: format!(
                                    "'{}' usa 'sobrepor', mas nenhuma classe-base declara \
                                     um método chamado '{}' — 'sobrepor' só pode redefinir \
                                     um método 'virtual' já existente na base .",
                                    metodo.nome, metodo.nome
                                ),
                            });
                        }
                        Some((base_metodo, doadora))
                            if !matches!(
                                base_metodo.modificador,
                                Modificador::Virtual | Modificador::Sobrepor
                            ) =>
                        {
                            self.erros.push(ErroSemantico {
                                linha: metodo.linha_assinatura,
                                mensagem: format!(
                                    "'{}' usa 'sobrepor', mas o método '{}' em '{}' não foi \
                                     declarado 'virtual' — adicione 'virtual' à declaração na \
                                     classe-base se a intenção é permitir redefinição (seção \
                                     10.6).",
                                    metodo.nome, base_metodo.nome, doadora
                                ),
                            });
                        }
                        Some((base_metodo, doadora))
                            if !assinaturas_compativeis_para_override(
                                &metodo.assinatura,
                                &base_metodo.assinatura,
                            ) =>
                        {
                            self.erros.push(ErroSemantico {
                                linha: metodo.linha_assinatura,
                                mensagem: format!(
                                    "'{}' usa 'sobrepor', mas sua assinatura não é idêntica à \
                                     do método 'virtual' correspondente em '{}' (mesma \
                                     quantidade e tipos de parâmetro, mesmo tipo de retorno) — \
                                     uma assinatura diferente é uma nova sobrecarga, não um \
                                     override, e não deve usar 'sobrepor' .",
                                    metodo.nome, doadora
                                ),
                            });
                        }
                        Some(_) => {} // tudo certo
                    },
                    Modificador::Nenhum => {
                        if let Some((base_metodo, doadora)) = correspondente_na_base {
                            if matches!(
                                base_metodo.modificador,
                                Modificador::Virtual | Modificador::Sobrepor
                            ) && assinaturas_compativeis_para_override(
                                &metodo.assinatura,
                                &base_metodo.assinatura,
                            ) {
                                self.erros.push(ErroSemantico {
                                    linha: metodo.linha_assinatura,
                                    mensagem: format!(
                                        "'{}' redefine o método 'virtual' de mesmo nome e \
                                         assinatura declarado em '{}', mas não usa 'sobrepor' \
                                         — adicione 'sobrepor' para deixar explícito que esta é \
                                         uma redefinição intencional .",
                                        metodo.nome, doadora
                                    ),
                                });
                            }
                        }
                    }
                    Modificador::Virtual => {} // 'virtual' na derivada não tem regra adicional aqui
                }
            }
        }
    }

    /// Marca `implementado = true` em cada [`InfoMetodo`] cuja
    /// implementação aparece como `MetodoInterno` (já coberto em
    /// `coletar_classes`) ou como [`DeclaracaoTopo::MetodoExterno`] em
    /// qualquer lugar do programa — e reporta erro semântico didático
    /// para toda assinatura que continuar sem implementação depois disso
    ///. Roda depois de `coletar_classes`.
    fn validar_implementacao_de_metodos(&mut self, declaracoes: &[DeclaracaoTopo]) {
        self.marcar_metodos_externos_implementados(declaracoes);
        let nomes_classes: Vec<String> = self.info_classes.keys().cloned().collect();
        for chave in nomes_classes {
            let info = &self.info_classes[&chave];
            for metodo in &info.metodos {
                if !metodo.implementado {
                    self.erros.push(ErroSemantico {
                        linha: metodo.linha_assinatura,
                        mensagem: format!(
                            "o método '{}' foi declarado na classe '{}', mas nunca foi \
                             implementado — nem internamente (corpo dentro de 'classe ... \
                             fim_classe') nem externamente ('função {}..{}(...) ... fim' \
                             ou 'procedimento {}..{}(...) ... fim'). Toda assinatura de \
                             método precisa de uma implementação .",
                            metodo.nome, info.nome, info.nome, metodo.nome, info.nome, metodo.nome
                        ),
                    });
                }
            }
        }
    }

    /// Marca `implementado = true` em cada [`InfoMetodo`] cuja
    /// implementação externa foi encontrada — casando por **assinatura
    /// completa** (nome + lista de tipos de parâmetro), não só por
    /// nome, para que sobrecarga funcione: duas assinaturas
    /// `AssinaturaMetodo` de mesmo nome (`CALCULAR(X:inteiro)` e
    /// `CALCULAR(R,H:real)`) precisam casar, cada uma, com a
    /// implementação externa de mesmos tipos de parâmetro — nunca as
    /// duas com a primeira implementação externa que aparecer.
    fn marcar_metodos_externos_implementados(&mut self, declaracoes: &[DeclaracaoTopo]) {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::MetodoExterno { classe, metodo } => {
                    let chave = classe.to_lowercase();
                    let assinatura_impl =
                        self.resolver_assinatura(metodo.categoria, &metodo.parametros, &metodo.tipo_retorno);
                    if let Some(info) = self.info_classes.get_mut(&chave) {
                        let candidato = info.metodos.iter_mut().find(|m| {
                            m.nome.eq_ignore_ascii_case(&metodo.nome)
                                && mesma_lista_de_tipos(&m.assinatura, &assinatura_impl)
                        });
                        if let Some(m) = candidato {
                            m.implementado = true;
                        } else if info.metodos.iter().any(|m| m.nome.eq_ignore_ascii_case(&metodo.nome)) {
                            self.erros.push(ErroSemantico {
                                linha: metodo.linha,
                                mensagem: format!(
                                    "'{}..{}' implementa um método externo, mas nenhuma das \
                                     sobrecargas  de '{}' na classe '{}' tem essa \
                                     mesma quantidade e tipos de parâmetro — confira se a \
                                     implementação bate com alguma das assinaturas \
                                     declaradas.",
                                    classe, metodo.nome, metodo.nome, classe
                                ),
                            });
                        } else {
                            self.erros.push(ErroSemantico {
                                linha: metodo.linha,
                                mensagem: format!(
                                    "'{}..{}' implementa um método externo, mas a classe \
                                     '{}' não declara nenhum método chamado '{}' (seção \
                                     10.3) — confira se o nome bate com a assinatura na \
                                     declaração da classe.",
                                    classe, metodo.nome, classe, metodo.nome
                                ),
                            });
                        }
                    } else {
                        self.erros.push(ErroSemantico {
                            linha: metodo.linha,
                            mensagem: format!(
                                "'{}..{}' implementa um método externo, mas '{}' não é \
                                 uma classe declarada .",
                                classe, metodo.nome, classe
                            ),
                        });
                    }
                }
                DeclaracaoTopo::SubRotina(s) => self.marcar_metodos_externos_implementados(&s.declaracoes_locais),
                DeclaracaoTopo::Const(_) | DeclaracaoTopo::Tipo(_) | DeclaracaoTopo::Var(_) => {}
            }
        }
    }

    // -- Passada 2: símbolos (escopos, tipos resolvidos) ----------------------------

    fn processar_declaracoes(&mut self, declaracoes: &[DeclaracaoTopo]) {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::Const(c) => self.processar_const(c),
                DeclaracaoTopo::Tipo(t) => self.processar_tipo(t),
                DeclaracaoTopo::Var(v) => self.processar_var(v),
                DeclaracaoTopo::SubRotina(s) => self.processar_subrotina(s),
                DeclaracaoTopo::MetodoExterno { classe, metodo } => {
                    self.processar_metodo_externo(classe, metodo)
                }
            }
        }
    }

    /// `const NOME = <literal>` — o tipo vem diretamente da
    /// variante de [`Expr`] (garantido pelo parser: `parse_literal` só
    /// produz `Inteiro`/`Real`/`Texto`/`Caractere`/`Logico`).
    fn processar_const(&mut self, c: &DeclaracaoConst) {
        let tipo = match &c.valor {
            Expr::Inteiro(_) => TipoResolvido::Inteiro,
            Expr::Real(_) => TipoResolvido::Real,
            Expr::Texto(_) => TipoResolvido::Cadeia,
            Expr::Caractere(_) => TipoResolvido::Caractere,
            Expr::Logico(_) => TipoResolvido::Logico,
            outro => unreachable!("const com valor não-literal (bug do parser): {outro:?}"),
        };
        if let Err(e) = self.tabela.declarar(&c.nome, CategoriaSimbolo::Const(tipo), c.linha) {
            self.erros.push(e);
        }
    }

    /// `tipo NOME = <definição>` — resolve a definição
    /// (com a tabela de tipos já completa, seção "coletar_tipos") e declara
    /// o nome no escopo atual (detecção de colisão com `const`/`var`/
    /// sub-rotinas de mesmo nome).
    fn processar_tipo(&mut self, t: &DeclaracaoTipo) {
        // 'classe' não passa por 'resolver_tipo' (ver nota em
        // 'tipos::resolver_tipo_rec') — a informação completa já foi
        // coletada em 'coletar_classes'; aqui só precisamos
        // declarar o nome no escopo (para detecção de colisão com
        // const/var/sub-rotinas) com um TipoResolvido::Classe mínimo, e
        // verificar o corpo de cada método implementado internamente
        // (assinaturas e métodos externos já foram tratados em
        // 'coletar_classes'/'validar_implementacao_de_metodos').
        if let Tipo::Classe { heranca, membros } = &t.definicao {
            let resolvido = TipoResolvido::Classe { nome: t.nome.clone(), heranca: heranca.clone() };
            if let Err(e) = self.tabela.declarar(&t.nome, CategoriaSimbolo::Tipo(resolvido), t.linha)
            {
                self.erros.push(e);
            }
            for membro in membros {
                if let ItemClasse::MetodoInterno(sub, _modificador) = &membro.item {
                    self.processar_metodo_interno(&t.nome, sub);
                }
            }
            return;
        }

        match resolver_tipo(&t.definicao, &self.tabela_tipos) {
            Ok(resolvido) => {
                if let Err(e) =
                    self.tabela.declarar(&t.nome, CategoriaSimbolo::Tipo(resolvido), t.linha)
                {
                    self.erros.push(e);
                }
            }
            Err(erro_tipo) => self.erros.push(erro_resolucao_tipo(&t.nome, erro_tipo, t.linha)),
        }
    }

    /// `var NOME1, NOME2, ... : <tipo>`.
    fn processar_var(&mut self, v: &DeclaracaoVar) {
        match resolver_tipo(&v.tipo, &self.tabela_tipos) {
            Ok(resolvido) => {
                for nome in &v.nomes {
                    if let Err(e) = self.tabela.declarar(
                        nome,
                        CategoriaSimbolo::Var(resolvido.clone()),
                        v.linha,
                    ) {
                        self.erros.push(e);
                    }
                }
            }
            Err(erro_tipo) => {
                let nome_ref = nome_tipo_em_erro(&v.tipo).unwrap_or_default();
                self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, v.linha));
            }
        }
    }

    /// Declara `nome` no escopo atual com `assinatura`, com
    /// suporte a **sobrecarga ad-hoc**: se já existir uma
    /// sub-rotina com esse nome no mesmo escopo, a nova assinatura é
    /// **acrescentada** à lista de sobrecargas em vez de gerar erro de
    /// redeclaração — desde que (a) a categoria seja a mesma
    /// (`procedimento`/`função` não se misturam sob o mesmo nome) e (b)
    /// a lista de tipos de parâmetro seja diferente de todas as já
    /// existentes (mesma lista de tipos = erro de redeclaração real,
    /// não sobrecarga válida — duas assinaturas idênticas seriam
    /// ambíguas em qualquer chamada). Colisão com `const`/`var`/`tipo`
    /// de mesmo nome continua erro, como antes (delegado a
    /// [`TabelaSimbolos::declarar`] no caso comum, primeira declaração).
    fn declarar_subrotina_com_sobrecarga(
        &mut self,
        nome: &str,
        assinatura: AssinaturaSubRotina,
        linha: usize,
    ) {
        let existente = self.tabela.buscar_no_escopo_atual(nome).cloned();
        match existente {
            None => {
                if let Err(e) = self.tabela.declarar(
                    nome,
                    CategoriaSimbolo::SubRotina(vec![assinatura]),
                    linha,
                ) {
                    self.erros.push(e);
                }
            }
            Some(simbolo) => {
                let descricao_anterior = simbolo.categoria.descricao();
                let linha_anterior = simbolo.linha_declaracao;
                match simbolo.categoria {
                    CategoriaSimbolo::SubRotina(mut assinaturas) => {
                        if assinaturas.first().map(|a| a.categoria) != Some(assinatura.categoria) {
                            self.erros.push(ErroSemantico {
                                linha,
                                mensagem: format!(
                                    "'{nome}' já foi declarado como {} na linha {} — uma \
                                     sobrecarga  precisa manter a mesma categoria \
                                     ('procedimento' ou 'função') em todas as versões.",
                                    descricao_anterior,
                                    linha_anterior
                                ),
                            });
                            return;
                        }
                        if assinaturas.iter().any(|a| mesma_lista_de_tipos(a, &assinatura)) {
                            self.erros.push(ErroSemantico {
                                linha,
                                mensagem: format!(
                                    "'{nome}' já foi declarado com a mesma quantidade e tipos de \
                                     parâmetro na linha {} — duas sobrecargas  \
                                     precisam ter aridade ou tipos de parâmetro diferentes; do \
                                     contrário, uma chamada não saberia qual delas usar.",
                                    linha_anterior
                                ),
                            });
                            return;
                        }
                        assinaturas.push(assinatura);
                        self.tabela.substituir_categoria_no_escopo_atual(
                            nome,
                            CategoriaSimbolo::SubRotina(assinaturas),
                        );
                    }
                    _ => {
                        // Colisão com 'const'/'var'/'tipo' — delega a
                        // 'TabelaSimbolos::declarar' para reaproveitar a
                        // mensagem de erro padrão (vai falhar, já que
                        // sabemos que 'nome' já existe; é só para manter
                        // uma única fonte de verdade para essa mensagem).
                        if let Err(e) = self.tabela.declarar(
                            nome,
                            CategoriaSimbolo::SubRotina(vec![assinatura]),
                            linha,
                        ) {
                            self.erros.push(e);
                        }
                    }
                }
            }
        }
    }

    /// `(procedimento|função) NOME(<parâmetros>) [: <retorno>] <declarações
    /// locais> início <corpo> fim`.
    ///
    /// 1. Resolve a assinatura (tipos dos parâmetros e do retorno).
    /// 2. Declara `NOME` no escopo **atual** (permite chamada recursiva e
    ///    chamadas de sub-rotinas declaradas mais abaixo no mesmo escopo).
    /// 3. Abre um novo escopo: para `função`, o próprio nome também é
    ///    declarado *dentro* desse escopo como uma variável do tipo de
    ///    retorno — é assim que `NOME <- <expr>` define o valor
    ///    retornado.
    /// 4. Declara os parâmetros e processa `declaracoes_locais`
    ///    recursivamente (incluindo sub-rotinas aninhadas, que
    ///    assim veem este escopo — lexical scoping).
    /// 5. Fecha o escopo. A verificação de `corpo` é a próxima etapa.
    fn processar_subrotina(&mut self, sub: &SubRotina) {
        let mut parametros_resolvidos = Vec::with_capacity(sub.parametros.len());
        for p in &sub.parametros {
            match resolver_tipo(&p.tipo, &self.tabela_tipos) {
                Ok(tipo) => parametros_resolvidos.push(ParametroResolvido {
                    nomes: p.nomes.clone(),
                    tipo,
                    por_referencia: p.por_referencia,
                }),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(&p.tipo).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, sub.linha));
                }
            }
        }

        let tipo_retorno = match &sub.tipo_retorno {
            Some(t) => match resolver_tipo(t, &self.tabela_tipos) {
                Ok(tr) => Some(tr),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(t).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, sub.linha));
                    None
                }
            },
            None => None,
        };

        let assinatura = AssinaturaSubRotina {
            categoria: sub.categoria,
            parametros: parametros_resolvidos.clone(),
            tipo_retorno: tipo_retorno.clone(),
        };
        self.declarar_subrotina_com_sobrecarga(&sub.nome, assinatura, sub.linha);

        self.tabela.entrar_escopo();

        if sub.categoria == CategoriaSubRotina::Funcao {
            if let Some(tr) = &tipo_retorno {
                // 'NOME <- expr' dentro do corpo define o retorno.
                if let Err(e) =
                    self.tabela.declarar(&sub.nome, CategoriaSimbolo::Var(tr.clone()), sub.linha)
                {
                    self.erros.push(e);
                }
            }
        }

        for p in &parametros_resolvidos {
            for nome in &p.nomes {
                if let Err(e) =
                    self.tabela.declarar(nome, CategoriaSimbolo::Var(p.tipo.clone()), sub.linha)
                {
                    self.erros.push(e);
                }
            }
        }

        self.processar_declaracoes(&sub.declaracoes_locais);
        self.verificar_bloco_de_subrotina(&sub.corpo, tipo_retorno.as_ref());

        self.tabela.sair_escopo();
    }

    /// `função <Classe>..<MÉTODO>(...) [: tipo] ... início ... fim`, ou a
    /// forma `procedimento` equivalente (implementação
    /// externa de método). Busca a [`InfoClasse`] de `classe` e delega
    /// para [`Verificador::processar_corpo_de_metodo`].
    fn processar_metodo_externo(&mut self, classe: &str, metodo: &SubRotina) {
        let chave_classe = classe.to_lowercase();
        let Some(info) = self.info_classes.get(&chave_classe).cloned() else {
            // Já reportado em 'marcar_metodos_externos_implementados'
            // (passada anterior) — evita duplicar o mesmo erro aqui.
            return;
        };
        self.processar_corpo_de_metodo(&info, metodo);
    }

    /// Implementação **interna** de um método (corpo dentro da própria
    /// declaração de `classe ... fim_classe`). Busca a
    /// [`InfoClasse`] de `nome_classe` e delega para
    /// [`Verificador::processar_corpo_de_metodo`] — mesmo tratamento de
    /// `este`/campos/parâmetros que um método externo.
    fn processar_metodo_interno(&mut self, nome_classe: &str, metodo: &SubRotina) {
        let chave_classe = nome_classe.to_lowercase();
        let Some(info) = self.info_classes.get(&chave_classe).cloned() else {
            // Não deveria acontecer — 'coletar_classes' sempre popula
            // 'info_classes' antes desta passada rodar — mas evita pânico
            // num cenário inesperado.
            return;
        };
        self.processar_corpo_de_metodo(&info, metodo);
    }

    /// Núcleo comum a método interno e externo: resolve a
    /// assinatura, abre um escopo com `este`/campos da classe (seção
    /// 10.4) seguido de um escopo aninhado para os parâmetros (para que
    /// um parâmetro possa sombrear um campo de mesmo nome — ex.:
    /// `PÕENOME(NOME : cadeia)` com campo `NOME`, desambiguado via
    /// `este.NOME`), processa declarações locais e verifica o corpo.
    /// **Não** declara `metodo.nome` no escopo geral — um método só é
    /// chamável via `OBJETO.MÉTODO()`, nunca como uma chamada solta.
    fn processar_corpo_de_metodo(&mut self, info: &InfoClasse, metodo: &SubRotina) {
        let mut parametros_resolvidos = Vec::with_capacity(metodo.parametros.len());
        for p in &metodo.parametros {
            match resolver_tipo(&p.tipo, &self.tabela_tipos) {
                Ok(tipo) => parametros_resolvidos.push(ParametroResolvido {
                    nomes: p.nomes.clone(),
                    tipo,
                    por_referencia: p.por_referencia,
                }),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(&p.tipo).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, metodo.linha));
                }
            }
        }

        let tipo_retorno = match &metodo.tipo_retorno {
            Some(t) => match resolver_tipo(t, &self.tabela_tipos) {
                Ok(tr) => Some(tr),
                Err(erro_tipo) => {
                    let nome_ref = nome_tipo_em_erro(t).unwrap_or_default();
                    self.erros.push(erro_resolucao_tipo(&nome_ref, erro_tipo, metodo.linha));
                    None
                }
            },
            None => None,
        };

        self.tabela.entrar_escopo();
        self.declarar_contexto_de_metodo(info, metodo, &tipo_retorno);

        // Escopo aninhado para os parâmetros: um parâmetro com o mesmo
        // nome de um campo da classe faz *shadowing* do campo (mesmo
        // mecanismo já usado para variáveis locais sombreando variáveis
        // externas) — é assim que 'PÕENOME(NOME : cadeia)' com campo
        // 'NOME' funciona: dentro do método, 'NOME' refere-se ao
        // parâmetro; 'este.NOME' continua acessando o campo (seção
        // 10.3). Sem este segundo escopo, declarar parâmetro e campo com
        // o mesmo nome no mesmo escopo seria erro de "já declarado".
        self.tabela.entrar_escopo();
        for p in &parametros_resolvidos {
            for nome in &p.nomes {
                if let Err(e) =
                    self.tabela.declarar(nome, CategoriaSimbolo::Var(p.tipo.clone()), metodo.linha)
                {
                    self.erros.push(e);
                }
            }
        }

        // 'classe_atual' habilita acesso a membros
        // 'seção_privada'/'seção_protegida' enquanto verificamos o corpo
        // deste método — restaurado ao valor anterior (não apenas
        // limpo para `None`) para que métodos chamados/processados a
        // partir de dentro de outro método (não há aninhamento real
        // hoje, mas a troca é feita de forma segura mesmo assim) não
        // vazem o contexto de classe um para o outro incorretamente.
        let classe_anterior = self.classe_atual.take();
        self.classe_atual = Some(info.nome.to_lowercase());

        self.processar_declaracoes(&metodo.declaracoes_locais);
        self.verificar_bloco_de_subrotina(&metodo.corpo, tipo_retorno.as_ref());

        self.classe_atual = classe_anterior;

        self.tabela.sair_escopo();
        self.tabela.sair_escopo();
    }

    /// Coleta todos os campos de `nome_classe` que resolvem **sem
    /// ambiguidade** através da árvore de herança —
    /// usado para declarar, no escopo de um método, todos os
    /// campos (próprios e herdados) acessíveis diretamente por nome,
    /// sem prefixo (mesma convenção do interpretador, ver
    /// `interpreter::valor_padrao_classe`).
    ///
    /// ⚠️ **Limitação conhecida:** um campo cujo nome é ambíguo entre
    /// duas bases (ex.: duas bases diretas com um campo de mesmo
    /// nome) é **omitido** desta lista — não entra no escopo
    /// direto do método. Usá-lo sem qualificação dentro do corpo do
    /// método gera "identificador não declarado" em vez da mensagem de
    /// ambiguidade mais específica que `buscar_campo_com_heranca`
    /// produziria para um acesso `OBJETO.NOME` fora do método. Cobrir
    /// esse caso exigiria estender `CategoriaSimbolo` com uma variante
    /// "ambíguo" só para gerar uma mensagem melhor — adiado até
    /// aparecer um caso real do material que precise disso.
    fn campos_com_heranca(&self, nome_classe: &str) -> Vec<InfoCampo> {
        let chave = nome_classe.to_lowercase();
        let Some(info) = self.info_classes.get(&chave) else { return Vec::new() };

        // Nomes de campo possíveis: os próprios da classe, mais todos os
        // que aparecem em qualquer base (direta ou indireta) — coletados
        // via uma travessia simples só para enumerar nomes candidatos;
        // a resolução de fato (incluindo prioridade/ambiguidade) usa
        // 'buscar_campo_com_heranca' para cada nome.
        let mut nomes = Vec::new();
        let mut pilha = vec![chave];
        let mut visitados = std::collections::HashSet::new();
        while let Some(atual) = pilha.pop() {
            if !visitados.insert(atual.clone()) {
                continue; // já visitado por outro caminho, ou ciclo
            }
            let Some(info_atual) = self.info_classes.get(&atual) else { continue };
            for campo in &info_atual.campos {
                if !nomes.iter().any(|n: &String| n.eq_ignore_ascii_case(&campo.nome)) {
                    nomes.push(campo.nome.clone());
                }
            }
            pilha.extend(info_atual.heranca.iter().map(|b| b.to_lowercase()));
        }

        nomes
            .into_iter()
            .filter_map(|nome| match self.buscar_campo_com_heranca(&info.nome, &nome) {
                ResolucaoMembro::Encontrado(campo, _) => Some(campo),
                ResolucaoMembro::Ambiguo(_) | ResolucaoMembro::NaoEncontrado => None,
            })
            .collect()
    }

    /// Declara, no escopo atual (já aberto pelo chamador), o contexto
    /// comum a qualquer corpo de método de `info.nome`:
    /// `este` (referência à própria classe), todos os campos da classe
    /// e da cadeia de herança diretamente por nome, e — se for
    /// `função` — o próprio nome do método como variável de retorno
    /// (mesma convenção de `processar_subrotina`).
    fn declarar_contexto_de_metodo(
        &mut self,
        info: &InfoClasse,
        metodo: &SubRotina,
        tipo_retorno: &Option<TipoResolvido>,
    ) {
        let tipo_da_classe = TipoResolvido::Classe { nome: info.nome.clone(), heranca: info.heranca.clone() };
        if let Err(e) =
            self.tabela.declarar("este", CategoriaSimbolo::Var(tipo_da_classe), metodo.linha)
        {
            self.erros.push(e);
        }
        for campo in self.campos_com_heranca(&info.nome) {
            if let Err(e) = self.tabela.declarar(
                &campo.nome,
                CategoriaSimbolo::Var(campo.tipo.clone()),
                metodo.linha,
            ) {
                self.erros.push(e);
            }
        }
        if metodo.categoria == CategoriaSubRotina::Funcao {
            if let Some(tr) = tipo_retorno {
                if let Err(e) =
                    self.tabela.declarar(&metodo.nome, CategoriaSimbolo::Var(tr.clone()), metodo.linha)
                {
                    self.erros.push(e);
                }
            }
        }
    }

    // =================================================================================
    // Verificação de comandos 
    // =================================================================================

    /// Verifica o corpo de uma sub-rotina (ou o `bloco_principal` do
    /// programa): coleta TODOS os rótulos declarados em qualquer nível de
    /// bloco aninhado dentro de `bloco` (uma única vez, recursivamente —
    /// ver [`Verificador::coletar_rotulos`]) antes de verificar os
    /// comandos, e restaura o conjunto de rótulos anterior ao final (para
    /// que sub-rotinas aninhadas, tenham seu próprio espaço de
    /// rótulos, isolado do da sub-rotina que as contém).
    fn verificar_bloco_de_subrotina(&mut self, bloco: &Bloco, tipo_retorno_atual: Option<&TipoResolvido>) {
        let novos_rotulos = self.coletar_rotulos(bloco);
        let anterior = std::mem::replace(&mut self.rotulos_validos, novos_rotulos);
        self.verificar_bloco(bloco, tipo_retorno_atual);
        self.rotulos_validos = anterior;
    }

    /// Coleta recursivamente todo `Comando::Rotulo` em `bloco`, incluindo
    /// dentro de `se`/laços/`caso` aninhados (mas NÃO atravessando o corpo
    /// de uma sub-rotina aninhada — essa tem seu próprio espaço
    /// de rótulos, coletado em sua própria chamada de
    /// [`Verificador::verificar_bloco_de_subrotina`]). Reporta erro se dois
    /// rótulos no mesmo espaço tiverem o mesmo nome (case-insensitive).
    fn coletar_rotulos(&mut self, bloco: &Bloco) -> std::collections::HashSet<String> {
        let mut rotulos = std::collections::HashSet::new();
        self.coletar_rotulos_rec(bloco, &mut rotulos);
        rotulos
    }

    fn coletar_rotulos_rec(&mut self, bloco: &Bloco, rotulos: &mut std::collections::HashSet<String>) {
        for comando in bloco {
            match comando {
                Comando::Rotulo { nome, linha } => {
                    let chave = nome.to_lowercase();
                    if !rotulos.insert(chave) {
                        self.erros.push(ErroSemantico {
                            linha: *linha,
                            mensagem: format!(
                                "o rótulo '{nome}' já foi declarado anteriormente nesta \
                                 sub-rotina (PEPPE é case-insensitive )."
                            ),
                        });
                    }
                }
                Comando::Se { entao, senao, .. } | Comando::ExcetoSe { entao, senao, .. } => {
                    self.coletar_rotulos_rec(entao, rotulos);
                    if let Some(senao) = senao {
                        self.coletar_rotulos_rec(senao, rotulos);
                    }
                }
                Comando::Caso { ramos, senao, .. } => {
                    for ramo in ramos {
                        self.coletar_rotulos_rec(&ramo.corpo, rotulos);
                    }
                    if let Some(senao) = senao {
                        self.coletar_rotulos_rec(senao, rotulos);
                    }
                }
                Comando::Enquanto { corpo, .. }
                | Comando::AteSeja { corpo, .. }
                | Comando::Repita { corpo, .. }
                | Comando::Execute { corpo, .. }
                | Comando::Laco { corpo, .. }
                | Comando::Para { corpo, .. } => {
                    self.coletar_rotulos_rec(corpo, rotulos);
                }
                // Sub-rotinas aninhadas têm seu próprio espaço
                // de rótulos — não atravessado aqui.
                _ => {}
            }
        }
    }

    /// Verifica cada comando de `bloco` em sequência. `tipo_retorno_atual`
    /// é `Some(tipo)` quando estamos dentro de uma `função` (usado para
    /// nada nesta etapa além de estar disponível a quem precisar — a
    /// verificação de que toda função efetivamente atribui seu retorno é
    /// responsabilidade do interpretador/fluxo de controle, não desta
    /// passada estática simples).
    fn verificar_bloco(&mut self, bloco: &Bloco, tipo_retorno_atual: Option<&TipoResolvido>) {
        for comando in bloco {
            self.verificar_comando(comando, tipo_retorno_atual);
        }
    }

    fn verificar_comando(&mut self, comando: &Comando, tipo_retorno_atual: Option<&TipoResolvido>) {
        match comando {
            Comando::Atribuicao { destino, valor, linha } => {
                if matches!(destino.acessos.last(), Some(Acesso::Metodo { .. })) {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: format!(
                            "'{}' é uma chamada de método, não é um lugar que pode receber \
                             atribuição  — o valor de retorno de um método não \
                             pode ser usado como destino de '<-'.",
                            nome_lvalue(destino)
                        ),
                    });
                    self.tipo_de_expr(valor);
                    self.tipo_de_lvalue(destino);
                    return;
                }
                let tipo_destino = self.tipo_de_lvalue(destino);
                let tipo_valor = match &tipo_destino {
                    Some(TipoResolvido::Funcao { parametros }) => {
                        let parametros = parametros.clone();
                        self.tipo_de_expr_como_referencia_funcao(valor, &parametros, *linha)
                    }
                    _ => self.tipo_de_expr(valor),
                };
                if let (Some(tv), Some(td)) = (tipo_valor, tipo_destino) {
                    match compatibilidade_com_heranca(&tv, &td, &self.tabela_heranca) {
                        Compatibilidade::Direta => {}
                        Compatibilidade::PrecisaCast => self.erros.push(ErroSemantico {
                            linha: *linha,
                            mensagem: format!(
                                "não é possível atribuir um valor '{}' a '{}' (tipo '{}') \
                                 sem conversão explícita. Use um cast, ex.: '{}({})' \
                                 .",
                                tv.nome_exibicao(),
                                nome_lvalue(destino),
                                td.nome_exibicao(),
                                td.nome_exibicao(),
                                nome_lvalue(destino)
                            ),
                        }),
                        Compatibilidade::Incompativel => self.erros.push(ErroSemantico {
                            linha: *linha,
                            mensagem: format!(
                                "não é possível atribuir um valor '{}' a '{}' (tipo '{}') \
                                 — os tipos são incompatíveis.",
                                tv.nome_exibicao(),
                                nome_lvalue(destino),
                                td.nome_exibicao()
                            ),
                        }),
                    }
                }
            }

            Comando::Leia { variaveis, .. } => {
                for v in variaveis {
                    self.tipo_de_lvalue(v);
                }
            }
            Comando::LeiaSeco { variavel, .. } => {
                self.tipo_de_lvalue(variavel);
            }

            Comando::Escreva { itens, linha, .. } => {
                for item in itens {
                    let tipo_expr = self.tipo_de_expr(&item.expressao);
                    if let Some(largura) = &item.largura {
                        self.exigir_tipo_logico_ou_numerico_inteiro(largura, *linha, "largura");
                    }
                    if let Some(decimais) = &item.decimais {
                        self.exigir_tipo_logico_ou_numerico_inteiro(decimais, *linha, "decimais");
                        if let Some(t) = &tipo_expr {
                            if *t != TipoResolvido::Real && *t != TipoResolvido::Generico {
                                self.erros.push(ErroSemantico {
                                    linha: *linha,
                                    mensagem: format!(
                                        "o especificador de decimais (':decimais') só faz \
                                         sentido para valores 'real', mas o valor é '{}' \
                                         .",
                                        t.nome_exibicao()
                                    ),
                                });
                            }
                        }
                    }
                }
            }

            Comando::Se { condicao, entao, senao, linha }
            | Comando::ExcetoSe { condicao, entao, senao, linha } => {
                self.exigir_tipo_logico(condicao, *linha);
                self.verificar_bloco(entao, tipo_retorno_atual);
                if let Some(senao) = senao {
                    self.verificar_bloco(senao, tipo_retorno_atual);
                }
            }

            Comando::Caso { expressao, ramos, senao, .. } => {
                self.tipo_de_expr(expressao);
                for ramo in ramos {
                    self.tipo_de_expr(&ramo.valor);
                    self.verificar_bloco(&ramo.corpo, tipo_retorno_atual);
                }
                if let Some(senao) = senao {
                    self.verificar_bloco(senao, tipo_retorno_atual);
                }
            }

            Comando::Enquanto { condicao, corpo, linha }
            | Comando::AteSeja { condicao, corpo, linha } => {
                self.exigir_tipo_logico(condicao, *linha);
                self.verificar_corpo_de_laco(corpo, tipo_retorno_atual);
            }

            Comando::Repita { corpo, condicao, linha }
            | Comando::Execute { corpo, condicao, linha } => {
                self.verificar_corpo_de_laco(corpo, tipo_retorno_atual);
                self.exigir_tipo_logico(condicao, *linha);
            }

            Comando::Laco { corpo, .. } => {
                self.verificar_corpo_de_laco(corpo, tipo_retorno_atual);
            }

            Comando::Para { variavel, inicio, fim, passo, corpo, linha } => {
                self.verificar_variavel_controle_para(variavel, *linha);
                self.exigir_tipo_numerico(inicio, *linha, "início do 'para'");
                self.exigir_tipo_numerico(fim, *linha, "fim do 'para'");
                if let Some(p) = passo {
                    self.exigir_tipo_numerico(p, *linha, "'passo' do 'para'");
                }
                self.verificar_corpo_de_laco(corpo, tipo_retorno_atual);
            }

            Comando::Dimensione { variavel, dimensoes, linha } => {
                let categoria_opt = self.tabela.buscar(variavel).map(|s| s.categoria.clone());
                match categoria_opt {
                    Some(CategoriaSimbolo::Var(TipoResolvido::Conjunto {
                        dimensoes: dims_declaradas,
                        ..
                    })) => {
                        if dimensoes.len() != dims_declaradas.len() {
                            self.erros.push(ErroSemantico {
                                linha: *linha,
                                mensagem: format!(
                                    "'{}' foi declarado com {} dimensão(ões), mas \
                                     'dimensione' forneceu {}  — o número de \
                                     pares <início>..<fim> deve ser igual ao número de \
                                     dimensões na declaração do tipo.",
                                    variavel,
                                    dims_declaradas.len(),
                                    dimensoes.len()
                                ),
                            });
                        }
                    }
                    Some(outro) => self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: format!(
                            "'dimensione' só pode ser usado em uma variável do tipo \
                             'conjunto', mas '{}' é {}.",
                            variavel,
                            outro.descricao()
                        ),
                    }),
                    None => self.erros.push(erro_identificador_nao_declarado(variavel, *linha)),
                }
                for (ini, fim) in dimensoes {
                    self.exigir_tipo_numerico(ini, *linha, "limite de 'dimensione'");
                    self.exigir_tipo_numerico(fim, *linha, "limite de 'dimensione'");
                }
            }

            Comando::ChamadaProcedimento { nome, argumentos, linha } => {
                self.verificar_chamada(nome, argumentos, *linha, CategoriaSubRotina::Procedimento);
            }

            Comando::ChamadaMetodo { alvo, linha } => {
                // Comando solto: ignora o valor de retorno,
                // se houver — válido tanto para 'procedimento' (sem
                // retorno) quanto para 'função' (descarta o retorno,
                // mesma permissividade de 'ESTUDANTE.CALCMÉDIA()' no
                // material de origem, que chama uma função só pelo efeito
                // colateral de atualizar o campo MÉDIA). Qualquer erro
                // (classe inexistente, método inexistente, aridade/tipo
                // de argumento) já é reportado por 'tipo_de_lvalue'.
                self.tipo_de_lvalue(alvo);
                if !matches!(alvo.acessos.last(), Some(Acesso::Metodo { .. })) {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: "comando de chamada de método malformado (bug interno do \
                                    parser) — o último acesso deveria ser uma chamada de \
                                    método."
                            .to_string(),
                    });
                }
            }

            Comando::Rotulo { .. } => {
                // Já coletado e validado (duplicatas) por coletar_rotulos,
                // chamado uma vez no início de verificar_bloco_de_subrotina.
            }

            Comando::IrPara { rotulo, linha } => {
                if !self.rotulos_validos.contains(&rotulo.to_lowercase()) {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: format!(
                            "o rótulo '{rotulo}' não foi declarado nesta sub-rotina. \
                             'ir_para' só pode saltar para um rótulo ('{rotulo}':) \
                             definido na mesma sub-rotina ou programa ."
                        ),
                    });
                }
            }

            Comando::Interrompa { linha } => {
                if self.profundidade_laco == 0 {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: "'interrompa' só pode ser usado dentro de um laço \
                                   (enquanto/até_seja/repita/execute/laço/para) ."
                            .to_string(),
                    });
                }
            }
            Comando::Continue { linha } => {
                if self.profundidade_laco == 0 {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: "'continue' só pode ser usado dentro de um laço \
                                   (enquanto/até_seja/repita/execute/laço/para) ."
                            .to_string(),
                    });
                }
            }
            Comando::SaiaCaso { condicao, linha } => {
                if self.profundidade_laco == 0 {
                    self.erros.push(ErroSemantico {
                        linha: *linha,
                        mensagem: "'saia_caso' só pode ser usado dentro de um 'laço' \
                                   ."
                            .to_string(),
                    });
                }
                self.exigir_tipo_logico(condicao, *linha);
            }

            Comando::Limpar { .. } => {}
            Comando::LimparLinha { coluna, linha } => {
                if let Some(c) = coluna {
                    self.exigir_tipo_numerico(c, *linha, "coluna de 'limpar_linha'");
                }
            }
            Comando::Posicionar { coluna, linha_destino, linha } => {
                self.exigir_tipo_numerico(coluna, *linha, "coluna de 'posicionar'");
                self.exigir_tipo_numerico(linha_destino, *linha, "linha de 'posicionar'");
            }
            Comando::CorFundo { cor, linha } | Comando::CorFrente { cor, linha } => {
                self.exigir_tipo_numerico(cor, *linha, "cor");
            }
            Comando::Pausa { .. } => {}
        }
    }

    /// Verifica o corpo de qualquer laço, incrementando/decrementando o
    /// contador usado para validar `interrompa`/`saia_caso`.
    fn verificar_corpo_de_laco(&mut self, corpo: &Bloco, tipo_retorno_atual: Option<&TipoResolvido>) {
        self.profundidade_laco += 1;
        self.verificar_bloco(corpo, tipo_retorno_atual);
        self.profundidade_laco -= 1;
    }

    /// `para VAR de ... até ...` — `VAR` deve existir e ser
    /// `inteiro` (ou `real`, embora seja pouco usual) ou `generico`.
    fn verificar_variavel_controle_para(&mut self, nome: &str, linha: usize) {
        let categoria_opt = self.tabela.buscar(nome).map(|s| s.categoria.clone());
        match categoria_opt {
            Some(CategoriaSimbolo::Var(t)) if numerico_para_controle(&t) => {}
            Some(CategoriaSimbolo::Var(t)) => self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "a variável de controle do 'para' deve ser numérica \
                     (inteiro/real), mas '{}' é '{}'.",
                    nome,
                    t.nome_exibicao()
                ),
            }),
            Some(outro) => self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "a variável de controle do 'para' deve ser uma variável, \
                     mas '{}' é {}.",
                    nome,
                    outro.descricao()
                ),
            }),
            None => self.erros.push(erro_identificador_nao_declarado(nome, linha)),
        }
    }

    /// Verifica `expr` e reporta erro se seu tipo não for `lógico` (usado
    /// em condições de `se`/`exceto_se`/laços e `saia_caso`).
    fn exigir_tipo_logico(&mut self, expr: &Expr, linha: usize) {
        if let Some(t) = self.tipo_de_expr(expr) {
            if t != TipoResolvido::Logico && t != TipoResolvido::Generico {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "uma condição deve ser do tipo 'lógico', mas encontrei '{}'.",
                        t.nome_exibicao()
                    ),
                });
            }
        }
    }

    /// Verifica `expr` e reporta erro se seu tipo não for numérico
    /// (`inteiro`/`real`) — usado em limites de `para`/`dimensione` e
    /// argumentos de comandos CONIO.
    fn exigir_tipo_numerico(&mut self, expr: &Expr, linha: usize, contexto: &str) {
        if let Some(t) = self.tipo_de_expr(expr) {
            if !matches!(t, TipoResolvido::Inteiro | TipoResolvido::Real | TipoResolvido::Generico)
            {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "{contexto} deve ser numérico (inteiro/real), mas encontrei '{}'.",
                        t.nome_exibicao()
                    ),
                });
            }
        }
    }

    /// Verifica `expr` e reporta erro se seu tipo não for numérico —
    /// variante usada para os especificadores `:largura`/`:decimais` de
    /// `escreva`, que devem ser `inteiro`.
    fn exigir_tipo_logico_ou_numerico_inteiro(&mut self, expr: &Expr, linha: usize, contexto: &str) {
        if let Some(t) = self.tipo_de_expr(expr) {
            if t != TipoResolvido::Inteiro && t != TipoResolvido::Generico {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "o especificador de {contexto} em 'escreva' deve ser 'inteiro', \
                         mas encontrei '{}' .",
                        t.nome_exibicao()
                    ),
                });
            }
        }
    }

    /// `NOME(arg1, arg2, ...)` ou `NOME` como comando (procedimento) ou
    /// dentro de uma expressão (função, ver [`Self::tipo_de_expr`]) —
    /// verifica que `nome` existe, é da `categoria_esperada`, resolve
    /// **qual sobrecarga** usar com base nos tipos dos
    /// argumentos, e confere que os argumentos são compatíveis em
    /// quantidade e tipo com os parâmetros da sobrecarga escolhida.
    /// Retorna o tipo de retorno (só relevante para função).
    fn verificar_chamada(
        &mut self,
        nome: &str,
        argumentos: &[Expr],
        linha: usize,
        categoria_esperada: CategoriaSubRotina,
    ) -> Option<TipoResolvido> {
        let categoria_opt = self.tabela.buscar(nome).map(|s| s.categoria.clone());

        // Chamada INDIRETA através de uma variável de tipo 'função'
        // — 'RESPOSTA(args)' onde 'RESPOSTA' guarda uma
        // referência a função, não o nome de uma sub-rotina declarada.
        // O tipo de retorno não é conhecido estaticamente (só os
        // parâmetros entram em `TipoResolvido::Funcao`) — o resultado é
        // 'generico' (decisão confirmada: compatível com qualquer uso
        // posterior, sem checagem estática adicional do retorno).
        //
        // 'categoria_esperada' é ignorado de propósito aqui: como uma
        // variável 'função' só pode guardar referência a FUNÇÃO (nunca
        // procedimento — já garantido no momento da atribuição, ver
        // 'resolver_referencia_funcao'), chamar essa variável é sempre
        // "chamar uma função". Usá-la como comando solto
        // ('RESPOSTA()', via Comando::ChamadaProcedimento) só descarta
        // o retorno — mesma permissividade já adotada para
        // 'OBJETO.MÉTODO()' como comando, não é erro.
        if let Some(CategoriaSimbolo::Var(TipoResolvido::Funcao { parametros })) = &categoria_opt {
            let parametros = parametros.clone();
            let tipos_argumentos: Vec<Option<TipoResolvido>> =
                argumentos.iter().map(|a| self.tipo_de_expr(a)).collect();
            if tipos_argumentos.len() != parametros.len() {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "'{nome}' espera {} argumento(s), mas a chamada tem {} .",
                        parametros.len(),
                        tipos_argumentos.len()
                    ),
                });
                return Some(TipoResolvido::Generico);
            }
            for (i, (esperado, recebido)) in parametros.iter().zip(tipos_argumentos.iter()).enumerate() {
                if let Some(recebido) = recebido {
                    if compatibilidade(recebido, esperado) == Compatibilidade::Incompativel {
                        self.erros.push(ErroSemantico {
                            linha,
                            mensagem: format!(
                                "argumento {} de '{nome}' deveria ser '{}', mas é '{}' \
                                 .",
                                i + 1,
                                esperado.nome_exibicao(),
                                recebido.nome_exibicao()
                            ),
                        });
                    }
                }
            }
            return Some(TipoResolvido::Generico);
        }

        let assinaturas = match categoria_opt {
            Some(CategoriaSimbolo::SubRotina(a)) => a,
            Some(CategoriaSimbolo::Var(_)) => {
                // Pode ser chamada recursiva de uma função pelo próprio nome
                // (estilo Pascal) — o escopo atual declara o nome
                // como 'Var' (variável de retorno), mas nos escopos externos
                // está a 'SubRotina' correspondente. Tenta encontrá-la.
                match self.tabela.buscar_em_escopos_externos(nome) {
                    Some(s) => match &s.categoria {
                        CategoriaSimbolo::SubRotina(a) => a.clone(),
                        _ => {
                            self.erros.push(ErroSemantico {
                                linha,
                                mensagem: format!(
                                    "'{}' não pode ser chamado como sub-rotina — é variável.",
                                    nome
                                ),
                            });
                            for arg in argumentos { self.tipo_de_expr(arg); }
                            return None;
                        }
                    },
                    None => {
                        self.erros.push(ErroSemantico {
                            linha,
                            mensagem: format!(
                                "'{}' não pode ser chamado como sub-rotina — é variável.",
                                nome
                            ),
                        });
                        for arg in argumentos { self.tipo_de_expr(arg); }
                        return None;
                    }
                }
            }
            Some(outro) => {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "'{}' não pode ser chamado como sub-rotina — é {}.",
                        nome,
                        outro.descricao()
                    ),
                });
                for arg in argumentos {
                    self.tipo_de_expr(arg);
                }
                return None;
            }
            None => {
                self.erros.push(erro_identificador_nao_declarado(nome, linha));
                for arg in argumentos {
                    self.tipo_de_expr(arg);
                }
                return None;
            }
        };

        // Avalia cada argumento **uma única vez** (efeitos colaterais —
        // erros de identificador não declarado, etc. — não devem se
        // repetir por candidata) antes de resolver a sobrecarga.
        let tipos_argumentos: Vec<Option<TipoResolvido>> =
            argumentos.iter().map(|a| self.tipo_de_expr(a)).collect();

        let assinatura = self.resolver_sobrecarga(nome, &assinaturas, &tipos_argumentos, linha);

        if assinatura.categoria != categoria_esperada {
            let (esperado, encontrado) = match categoria_esperada {
                CategoriaSubRotina::Procedimento => ("um procedimento", "uma função"),
                CategoriaSubRotina::Funcao => ("uma função", "um procedimento"),
            };
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{nome}' é {encontrado}, mas foi chamado como se fosse {esperado}."
                ),
            });
        }

        let total_parametros: usize = assinatura.parametros.iter().map(|p| p.nomes.len()).sum();
        if argumentos.len() != total_parametros {
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{}' espera {} argumento(s), mas foi chamado com {}.",
                    nome,
                    total_parametros,
                    argumentos.len()
                ),
            });
        }

        // Compara argumento a argumento até o menor dos dois tamanhos —
        // evita pânico de índice quando a aridade já está incorreta (o erro
        // de contagem acima já foi reportado).
        let parametros_expandidos: Vec<&ParametroResolvido> = assinatura
            .parametros
            .iter()
            .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
            .collect();

        for (i, tipo_arg) in tipos_argumentos.iter().enumerate() {
            let Some(tipo_arg) = tipo_arg else { continue };
            if let Some(param) = parametros_expandidos.get(i) {
                match compatibilidade(tipo_arg, &param.tipo) {
                    Compatibilidade::Direta => {}
                    Compatibilidade::PrecisaCast | Compatibilidade::Incompativel => {
                        self.erros.push(ErroSemantico {
                            linha,
                            mensagem: format!(
                                "o argumento {} de '{}' deveria ser '{}', mas encontrei '{}'.",
                                i + 1,
                                nome,
                                param.tipo.nome_exibicao(),
                                tipo_arg.nome_exibicao()
                            ),
                        });
                    }
                }
            }
        }

        assinatura.tipo_retorno
    }

    /// Escolhe, entre `candidatas` (todas as sobrecargas de `nome`), a
    /// que deve ser usada para uma chamada com
    /// `tipos_argumentos` (já avaliados — `None` num índice significa
    /// que aquele argumento já teve erro próprio, reportado em outro
    /// lugar). Caso comum (`candidatas.len() == 1`): retorna a única
    /// sem nenhuma lógica de resolução adicional, mantendo o mesmo
    /// comportamento simples esperado nesse caso.
    ///
    /// Com múltiplas candidatas: filtra por aridade exata; dentre essas,
    /// escolhe a que aceita **todos** os argumentos com
    /// [`Compatibilidade::Direta`] (sem cast). Zero candidatas aceitam →
    /// erro "nenhuma versão aceita esses argumentos" (lista as aridades
    /// disponíveis). Mais de uma aceita → erro de ambiguidade explícito
    /// (decisão do autor: PEPPE não tem regra de prioridade entre
    /// conversões implícitas — duas sobrecargas viáveis para a mesma
    /// chamada são sempre um erro, nunca resolvidas silenciosamente).
    /// Em ambos os casos de erro, retorna a primeira candidata de
    /// aridade compatível (ou a primeira de todas, se nenhuma aridade
    /// bater) só para permitir que a verificação continue sem pânico —
    /// o erro já reportado é o que importa.
    fn resolver_sobrecarga(
        &mut self,
        nome: &str,
        candidatas: &[AssinaturaSubRotina],
        tipos_argumentos: &[Option<TipoResolvido>],
        linha: usize,
    ) -> AssinaturaSubRotina {
        if candidatas.len() == 1 {
            return candidatas[0].clone();
        }

        let aridade_chamada = tipos_argumentos.len();
        let mesma_aridade: Vec<&AssinaturaSubRotina> = candidatas
            .iter()
            .filter(|a| a.parametros.iter().map(|p| p.nomes.len()).sum::<usize>() == aridade_chamada)
            .collect();

        if mesma_aridade.is_empty() {
            let aridades: Vec<String> = candidatas
                .iter()
                .map(|a| a.parametros.iter().map(|p| p.nomes.len()).sum::<usize>().to_string())
                .collect();
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "nenhuma versão de '{nome}' aceita {aridade_chamada} argumento(s) — as \
                     versões disponíveis  esperam {} argumento(s), \
                     respectivamente.",
                    aridades.join(", ")
                ),
            });
            return candidatas[0].clone();
        }

        // Se algum argumento já teve erro próprio (tipo desconhecido),
        // não há como julgar compatibilidade com confiança — escolhe a
        // primeira candidata de aridade certa e não adiciona ruído com
        // mais um erro de sobrecarga sobre um problema já reportado.
        if tipos_argumentos.iter().any(|t| t.is_none()) {
            return mesma_aridade[0].clone();
        }

        fn aceita_diretamente(
            assinatura: &AssinaturaSubRotina,
            tipos_argumentos: &[Option<TipoResolvido>],
        ) -> bool {
            let parametros_expandidos: Vec<&ParametroResolvido> = assinatura
                .parametros
                .iter()
                .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
                .collect();
            tipos_argumentos.iter().zip(parametros_expandidos.iter()).all(|(t, p)| {
                let tipo_arg = t.as_ref().expect("já filtramos None acima");
                compatibilidade(tipo_arg, &p.tipo) == Compatibilidade::Direta
            })
        }

        let aceitas: Vec<&AssinaturaSubRotina> = mesma_aridade
            .iter()
            .copied()
            .filter(|a| aceita_diretamente(a, tipos_argumentos))
            .collect();

        match aceitas.len() {
            1 => aceitas[0].clone(),
            0 => {
                let tipos_chamada: Vec<String> = tipos_argumentos
                    .iter()
                    .map(|t| t.as_ref().expect("já filtramos None acima").nome_exibicao())
                    .collect();
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "nenhuma versão de '{nome}' com {aridade_chamada} argumento(s) aceita \
                         os tipos ({}) — confira as sobrecargas disponíveis .",
                        tipos_chamada.join(", ")
                    ),
                });
                mesma_aridade[0].clone()
            }
            _ => {
                self.erros.push(ErroSemantico {
                    linha,
                    mensagem: format!(
                        "a chamada a '{nome}' é ambígua: mais de uma sobrecarga  \
                         com {aridade_chamada} argumento(s) aceita os tipos fornecidos. \
                         Adicione um cast explícito a um dos argumentos para desambiguar."
                    ),
                });
                mesma_aridade[0].clone()
            }
        }
    }

    // =================================================================================
    // Verificação de expressões e l-values (campo/índice)
    // =================================================================================

    /// Calcula o tipo de `expr` no contexto de uma atribuição/passagem
    /// onde o tipo ESPERADO é `TipoResolvido::Funcao` —
    /// usado por `Comando::Atribuicao` quando o destino é desse tipo.
    /// Intercepta os dois casos especiais de "referência a função sem
    /// chamar": um identificador sozinho (`SOMATORIO`) referenciando
    /// uma sub-rotina solta, e `OBJETO.MÉTODO` (sem parênteses)
    /// referenciando um método de instância. Em qualquer outro caso
    /// (ex.: copiar uma variável de tipo função já existente, `X ← Y`),
    /// delega para [`Self::tipo_de_expr`] sem alteração — o caminho
    /// normal de `Expr::Variavel` já resolve isso corretamente via
    /// `tipo_de_lvalue`.
    fn tipo_de_expr_como_referencia_funcao(
        &mut self,
        expr: &Expr,
        parametros_esperados: &[TipoResolvido],
        linha: usize,
    ) -> Option<TipoResolvido> {
        let Expr::Variavel(lvalue) = expr else {
            return self.tipo_de_expr(expr);
        };

        // Caso 1: 'SOMATORIO' sozinho (sem acessos) — referência a uma
        // sub-rotina SOLTA. Só intercepta se 'lvalue.nome' de fato for
        // uma sub-rotina na tabela de símbolos (não uma variável comum)
        // — senão, é só uma variável normal sendo lida, caminho usual.
        if lvalue.acessos.is_empty() && lvalue.qualificador_base.is_none() {
            if let Some(CategoriaSimbolo::SubRotina(assinaturas)) =
                self.tabela.buscar(&lvalue.nome).map(|s| s.categoria.clone())
            {
                return self.resolver_referencia_funcao(
                    &lvalue.nome,
                    &assinaturas,
                    parametros_esperados,
                    linha,
                );
            }
            return self.tipo_de_expr(expr);
        }

        // Caso 2: 'OBJETO.MÉTODO' (exatamente um acesso, e é um CAMPO —
        // não 'Acesso::Metodo', já que isso seria uma chamada com
        // parênteses, não uma referência). Só intercepta se 'MÉTODO' de
        // fato resolver como método na classe de 'OBJETO' — senão, é
        // um acesso a campo comum, caminho usual.
        if let [Acesso::Campo(nome_membro)] = lvalue.acessos.as_slice() {
            let lvalue_objeto = LValue {
                qualificador_base: lvalue.qualificador_base.clone(),
                nome: lvalue.nome.clone(),
                acessos: vec![],
                linha,
            };
            // Resolve só o tipo do OBJETO primeiro (sem o acesso a
            // MÉTODO) — se isso já falhar (objeto não declarado),
            // 'tipo_de_lvalue' já reportou o erro e retornamos sem
            // tentar de novo. Se tiver sucesso mas não for uma classe,
            // ou o nome não resolver como método, delega para
            // 'tipo_de_expr(expr)' completo (resolve 'lvalue' de novo,
            // sem duplicar erro: a primeira resolução do objeto, aqui,
            // já teve sucesso nesses dois ramos).
            let Some(tipo_objeto) = self.tipo_de_lvalue(&lvalue_objeto) else { return None };
            let TipoResolvido::Classe { nome: nome_classe, .. } = &tipo_objeto else {
                return self.tipo_de_expr(expr);
            };
            let classe_origem = lvalue.qualificador_base.as_deref().unwrap_or(nome_classe.as_str());
            if let ResolucaoMembro::Encontrado(candidatos, _) =
                self.buscar_metodo_com_heranca(classe_origem, nome_membro)
            {
                let assinaturas: Vec<AssinaturaSubRotina> =
                    candidatos.into_iter().map(|m| m.assinatura).collect();
                return self.resolver_referencia_funcao(nome_membro, &assinaturas, parametros_esperados, linha);
            }
        }

        self.tipo_de_expr(expr)
    }

    /// Valida que `nome` (sub-rotina ou método, já com suas `assinaturas`
    /// candidatas) pode ser atribuído a uma variável de tipo `função`
    /// cujos parâmetros esperados são `parametros_esperados`: exatamente
    /// uma assinatura (mais de uma = sobrecarregado = erro de ambiguidade
    /// explícito) precisa ser `função` (não `procedimento`), e os tipos de
    /// parâmetro precisam bater exatamente, na ordem (sem coerção —
    /// mesma aridade e mesmos tipos, não só compatíveis: o objetivo é a
    /// referência poder ser chamada depois com qualquer argumento que
    /// bata com `parametros_esperados`, então a assinatura real precisa
    /// ser idêntica a essa, não apenas "aceitável" para ela).
    fn resolver_referencia_funcao(
        &mut self,
        nome: &str,
        assinaturas: &[AssinaturaSubRotina],
        parametros_esperados: &[TipoResolvido],
        linha: usize,
    ) -> Option<TipoResolvido> {
        if assinaturas.len() > 1 {
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{nome}' está sobrecarregado  — não pode ser atribuído a \
                     uma variável de tipo função, já que não há como saber qual sobrecarga \
                     se quer referenciar ."
                ),
            });
            return Some(TipoResolvido::Funcao { parametros: parametros_esperados.to_vec() });
        }
        let assinatura = &assinaturas[0];
        if assinatura.categoria != CategoriaSubRotina::Funcao {
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{nome}' é um procedimento (não retorna valor) — uma variável de tipo \
                     função só pode referenciar funções ."
                ),
            });
            return Some(TipoResolvido::Funcao { parametros: parametros_esperados.to_vec() });
        }
        let tipos_reais = tipos_expandidos(assinatura);
        let bate = tipos_reais.len() == parametros_esperados.len()
            && tipos_reais.iter().zip(parametros_esperados.iter()).all(|(a, b)| *a == b);
        if !bate {
            let tipos_reais_nomes: Vec<String> = tipos_reais.iter().map(|t| t.nome_exibicao()).collect();
            let tipos_esperados_nomes: Vec<String> =
                parametros_esperados.iter().map(|t| t.nome_exibicao()).collect();
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{nome}' tem assinatura função({}) — incompatível com função({}) \
                     esperado .",
                    tipos_reais_nomes.join(", "),
                    tipos_esperados_nomes.join(", ")
                ),
            });
        }
        Some(TipoResolvido::Funcao { parametros: parametros_esperados.to_vec() })
    }

    /// Calcula o tipo de `expr`, reportando qualquer erro semântico
    /// encontrado pelo caminho (identificador não declarado, operador
    /// incompatível, etc.). Retorna `None` quando o tipo não pôde ser
    /// determinado (já houve erro) — o chamador deve tratar isso como "não
    /// dá para verificar mais nada aqui", não como um tipo válido.
    fn tipo_de_expr(&mut self, expr: &Expr) -> Option<TipoResolvido> {
        match expr {
            Expr::Inteiro(_) => Some(TipoResolvido::Inteiro),
            Expr::Real(_) => Some(TipoResolvido::Real),
            Expr::Texto(_) => Some(TipoResolvido::Cadeia),
            Expr::Caractere(_) => Some(TipoResolvido::Caractere),
            Expr::Logico(_) => Some(TipoResolvido::Logico),

            Expr::Variavel(lvalue) => {
                // Caso especial: se o ÚLTIMO acesso da cadeia
                // é uma chamada de método e esse método é um
                // 'procedimento' (sem retorno), usar isso como valor de
                // expressão é um erro distinto de "identificador não
                // declarado" — 'tipo_de_lvalue' retorna 'None' nesse caso
                // sem registrar erro (é válido como comando solto), então
                // aqui é o lugar certo para reportar quando o contexto é
                // de expressão, não de comando.
                if let Some(Acesso::Metodo { nome: nome_metodo, .. }) = lvalue.acessos.last() {
                    let total_erros_antes = self.erros.len();
                    let resultado = self.tipo_de_lvalue(lvalue);
                    if resultado.is_none() && self.erros.len() == total_erros_antes {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!(
                                "'{}' é um procedimento (sem retorno) — não pode ser \
                                 usado como valor dentro de uma expressão. Procedimentos \
                                 só podem ser chamados como comando isolado .",
                                nome_metodo
                            ),
                        });
                    }
                    return resultado;
                }
                self.tipo_de_lvalue(lvalue)
            }

            Expr::Chamada { nome, argumentos, linha } => {
                // Pode ser uma função do usuário OU uma função pré-definida
                // (matemática, ou de texto) — estas
                // últimas não entram na tabela de símbolos, então uma
                // chamada a 'raizq'/'tamanho'/etc. não declarada pelo
                // usuário não gera erro de "não declarado" aqui.
                if self.tabela.buscar(nome).is_none() {
                    if let Some(tipo_retorno) = tipo_retorno_predefinida(nome) {
                        for arg in argumentos {
                            self.tipo_de_expr(arg);
                        }
                        return Some(tipo_retorno);
                    }
                    // Verificação explícita para nomes que podem não ser
                    // capturados pela comparação Unicode em tipo_retorno_predefinida
                    let n = nome.to_lowercase();
                    let tipo_explicito = match n.as_str() {
                        "concatenar" | "aparar" | "cópia" | "copia" | "maiúsculo"
                        | "maiusculo" | "minúsculo" | "minusculo" => {
                            Some(TipoResolvido::Cadeia)
                        }
                        "posição" | "posicao" | "tamanho" | "ord" | "succ" | "pred" => {
                            Some(TipoResolvido::Inteiro)
                        }
                        "chr" => Some(TipoResolvido::Caractere),
                        _ => None,
                    };
                    if let Some(tipo_retorno) = tipo_explicito {
                        for arg in argumentos {
                            self.tipo_de_expr(arg);
                        }
                        return Some(tipo_retorno);
                    }
                }
                self.verificar_chamada(nome, argumentos, *linha, CategoriaSubRotina::Funcao)
            }

            Expr::Binaria { op, esquerda, direita, linha } => {
                let te = self.tipo_de_expr(esquerda);
                let td = self.tipo_de_expr(direita);
                match (te, td) {
                    (Some(te), Some(td)) => match tipo_resultado_binario(*op, &te, &td) {
                        Ok(t) => Some(t),
                        Err(msg) => {
                            self.erros.push(ErroSemantico { linha: *linha, mensagem: msg });
                            None
                        }
                    },
                    _ => None,
                }
            }

            Expr::Unaria { op, expr, linha } => {
                let t = self.tipo_de_expr(expr)?;
                match tipo_resultado_unario(*op, &t) {
                    Ok(t) => Some(t),
                    Err(msg) => {
                        self.erros.push(ErroSemantico { linha: *linha, mensagem: msg });
                        None
                    }
                }
            }

            Expr::Cast { tipo, expr, .. } => {
                self.tipo_de_expr(expr);
                Some(tipo_primitivo_para_resolvido(*tipo))
            }
        }
    }

    /// Aplica a regra de encapsulamento a um acesso a
    /// `nome_membro` (campo ou método), declarado com `visibilidade` na
    /// classe `classe_dono` (a classe que efetivamente o declarou,
    /// subindo a cadeia de herança — não necessariamente a classe da
    /// variável que o programador escreveu). Não faz nada (sempre
    /// permitido) para `Visibilidade::Publica`. Push um erro semântico
    /// quando o acesso não é permitido a partir de `self.classe_atual`.
    ///
    /// Regras:
    /// - `seção_privada`: só acessível de dentro de um método da própria
    ///   `classe_dono` (`self.classe_atual == Some(classe_dono)`).
    /// - `seção_protegida`: acessível de dentro de um método de
    ///   `classe_dono` ou de qualquer classe que herde dela, direta ou
    ///   indiretamente (`e_subclasse_de`).
    /// - Fora de qualquer método (`self.classe_atual == None`, ex.: bloco
    ///   principal do programa, ou dentro de uma sub-rotina solta), só
    ///   membros públicos são acessíveis.
    fn checar_visibilidade(
        &mut self,
        visibilidade: Visibilidade,
        classe_dono: &str,
        nome_membro: &str,
        eh_metodo: bool,
        linha: usize,
    ) {
        let permitido = match visibilidade {
            Visibilidade::Publica => true,
            Visibilidade::Privada => {
                self.classe_atual.as_deref() == Some(classe_dono)
            }
            Visibilidade::Protegida => match &self.classe_atual {
                Some(atual) => e_subclasse_de(atual, classe_dono, &self.tabela_heranca),
                None => false,
            },
        };
        if !permitido {
            let categoria = if eh_metodo { "método" } else { "campo" };
            let nome_visibilidade = match visibilidade {
                Visibilidade::Privada => "privado (seção_privada)",
                Visibilidade::Protegida => "protegido (seção_protegida)",
                Visibilidade::Publica => unreachable!("já tratado no braço 'true' acima"),
            };
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{nome_membro}' é um {categoria} {nome_visibilidade} da classe \
                     '{classe_dono}' — não pode ser acessado de fora da classe (seção \
                     10.4.1){}.",
                    if matches!(visibilidade, Visibilidade::Protegida) {
                        " ou de uma classe que não herda dela"
                    } else {
                        ""
                    }
                ),
            });
        }
    }

    /// Implementação genérica da resolução de um nome de membro através
    /// da árvore de herança (múltiplas bases diretas por classe, sem
    /// herança virtual): `extrair` decide o que conta como
    /// "achado" numa única [`InfoClasse`] (campo, ou lista de
    /// sobrecargas de método). Regra de prioridade: se a própria classe
    /// em `nome_classe` declara o nome diretamente, usa isso sem nem
    /// olhar as bases (nunca ambíguo nesse nível — análogo a campo
    /// redefinido na derivada "escondendo" o da base em C++). Senão,
    /// resolve recursivamente em cada base direta; se exatamente uma
    /// base resolve com sucesso, usa essa; se duas ou mais resolvem
    /// (mesmo que apontando, por caminhos diferentes, para a mesma
    /// classe-ancestral — "diamond problem" sem `virtual`), é
    /// [`ResolucaoMembro::Ambiguo`].
    fn resolver_em_bases<T>(
        &self,
        nome_classe: &str,
        extrair: &dyn Fn(&InfoClasse) -> Option<T>,
    ) -> ResolucaoMembro<T> {
        self.resolver_em_bases_rec(nome_classe, extrair, &mut Vec::new())
    }

    /// Implementação recursiva de [`Self::resolver_em_bases`]. `caminho`
    /// acumula as classes já visitadas **neste ramo específico** da
    /// recursão (não globalmente — em herança múltipla é normal que
    /// ramos diferentes passem pela mesma classe-ancestral, isso não é
    /// ciclo, é só o "diamond problem" sem `virtual`); serve apenas
    /// para impedir recursão infinita caso a coleta de classes tenha
    /// deixado passar um ciclo de herança real (`A herda de B`, `B
    /// herda de A`), o que não deveria acontecer mas não está validado
    /// antes deste ponto no pipeline — por segurança, encerra como
    /// `NaoEncontrado` nesse ramo em vez de invadir a pilha.
    fn resolver_em_bases_rec<T>(
        &self,
        nome_classe: &str,
        extrair: &dyn Fn(&InfoClasse) -> Option<T>,
        caminho: &mut Vec<String>,
    ) -> ResolucaoMembro<T> {
        let chave = nome_classe.to_lowercase();
        if caminho.contains(&chave) {
            return ResolucaoMembro::NaoEncontrado;
        }
        let Some(info) = self.info_classes.get(&chave) else { return ResolucaoMembro::NaoEncontrado };
        if let Some(achado) = extrair(info) {
            return ResolucaoMembro::Encontrado(achado, chave);
        }
        caminho.push(chave);
        let mut encontrados: Vec<(T, String)> = Vec::new();
        let mut doadores_ambiguos: Vec<String> = Vec::new();
        for base in &info.heranca {
            match self.resolver_em_bases_rec(base, extrair, caminho) {
                ResolucaoMembro::Encontrado(item, doadora) => encontrados.push((item, doadora)),
                ResolucaoMembro::Ambiguo(mut doadoras) => doadores_ambiguos.append(&mut doadoras),
                ResolucaoMembro::NaoEncontrado => {}
            }
        }
        caminho.pop();
        if !doadores_ambiguos.is_empty() || encontrados.len() > 1 {
            doadores_ambiguos.extend(encontrados.into_iter().map(|(_, doadora)| doadora));
            doadores_ambiguos.sort();
            doadores_ambiguos.dedup();
            return ResolucaoMembro::Ambiguo(doadores_ambiguos);
        }
        match encontrados.into_iter().next() {
            Some((item, doadora)) => ResolucaoMembro::Encontrado(item, doadora),
            None => ResolucaoMembro::NaoEncontrado,
        }
    }

    /// Retorna o [`InfoCampo`] de `nome_campo`, considerando toda a
    /// árvore de herança a partir de `nome_classe`,
    /// junto com o nome (em minúsculas) da classe que
    /// efetivamente o **declarou** — necessário para a regra de
    /// encapsulamento: um campo `seção_protegida` é
    /// acessível de dentro de qualquer subclasse da classe que o
    /// declarou, não só da classe que o programador está acessando.
    fn buscar_campo_com_heranca(&self, nome_classe: &str, nome_campo: &str) -> ResolucaoMembro<InfoCampo> {
        self.resolver_em_bases(nome_classe, &|info| info.campo(nome_campo).cloned())
    }

    /// Como [`Verificador::buscar_campo_com_heranca`], mas para
    /// métodos — com uma diferença importante por causa de sobrecarga
    ///: o "achado" em cada classe é a lista **completa**
    /// de sobrecargas de `nome_metodo` ali (nunca combina sobrecargas
    /// de classes diferentes entre si — mesma regra simplificada usada
    /// em C++ na ausência de `using Base::método;`); [`Self::
    /// resolver_sobrecarga`] decide, entre essas candidatas, qual usar
    /// para uma chamada específica.
    fn buscar_metodo_com_heranca(&self, nome_classe: &str, nome_metodo: &str) -> ResolucaoMembro<Vec<InfoMetodo>> {
        self.resolver_em_bases(nome_classe, &|info| {
            let candidatos = info.metodos_por_nome(nome_metodo);
            if candidatos.is_empty() {
                None
            } else {
                Some(candidatos.into_iter().cloned().collect())
            }
        })
    }

    /// Verifica os argumentos de uma chamada (sub-rotina solta ou
    /// método) contra `parametros` já resolvidos, mas
    /// para quando os tipos dos argumentos **já foram calculados**
    /// antes (ex.: para resolver qual sobrecarga usar,
    /// antes de saber contra quais `parametros` validar) — evita chamar
    /// [`Self::tipo_de_expr`] uma segunda vez sobre os mesmos
    /// argumentos, o que duplicaria quaisquer erros que o cálculo do
    /// tipo já tenha reportado. Usa [`compatibilidade_com_heranca`] (não
    /// a versão simples), para que um argumento de classe derivada seja
    /// aceito onde se espera a classe-base.
    fn verificar_argumentos_ja_avaliados(
        &mut self,
        nome_chamada: &str,
        parametros: &[ParametroResolvido],
        tipos_argumentos: &[Option<TipoResolvido>],
        linha: usize,
    ) {
        let total_parametros: usize = parametros.iter().map(|p| p.nomes.len()).sum();
        if tipos_argumentos.len() != total_parametros {
            self.erros.push(ErroSemantico {
                linha,
                mensagem: format!(
                    "'{}' espera {} argumento(s), mas foi chamado com {}.",
                    nome_chamada,
                    total_parametros,
                    tipos_argumentos.len()
                ),
            });
        }

        let parametros_expandidos: Vec<&ParametroResolvido> = parametros
            .iter()
            .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
            .collect();

        for (i, tipo_arg) in tipos_argumentos.iter().enumerate() {
            let Some(tipo_arg) = tipo_arg else { continue };
            if let Some(param) = parametros_expandidos.get(i) {
                match compatibilidade_com_heranca(tipo_arg, &param.tipo, &self.tabela_heranca) {
                    Compatibilidade::Direta => {}
                    Compatibilidade::PrecisaCast | Compatibilidade::Incompativel => {
                        self.erros.push(ErroSemantico {
                            linha,
                            mensagem: format!(
                                "o argumento {} de '{}' deveria ser '{}', mas encontrei '{}'.",
                                i + 1,
                                nome_chamada,
                                param.tipo.nome_exibicao(),
                                tipo_arg.nome_exibicao()
                            ),
                        });
                    }
                }
            }
        }
    }


    /// Calcula o tipo de um [`LValue`] (`NOME`, `NOME.CAMPO`, `NOME[i]`,
    /// encadeados ), reportando identificador não declarado,
    /// categoria errada (ex.: chamar uma função sem `()` como variável),
    /// acesso de campo em algo que não é `registro`, campo inexistente,
    /// índice em algo que não é `conjunto`, número de índices diferente do
    /// número de dimensões, ou índice não-numérico.
    fn tipo_de_lvalue(&mut self, lvalue: &LValue) -> Option<TipoResolvido> {
        let simbolo = match self.tabela.buscar(&lvalue.nome) {
            Some(s) => s.clone(),
            None => {
                // Constantes pré-definidas — não entram na tabela
                // de símbolos, mas são sempre do tipo real.
                let nome_lower = lvalue.nome.to_lowercase();
                if (nome_lower == "p_pi" || nome_lower == "p_euler" || nome_lower == "p_infinito")
                    && lvalue.acessos.is_empty()
                {
                    return Some(TipoResolvido::Real);
                }
                self.erros.push(erro_identificador_nao_declarado(&lvalue.nome, lvalue.linha));
                return None;
            }
        };

        let mut tipo_atual = match &simbolo.categoria {
            CategoriaSimbolo::Var(t) => t.clone(),
            CategoriaSimbolo::Const(t) if lvalue.acessos.is_empty() => t.clone(),
            outro => {
                self.erros.push(ErroSemantico {
                    linha: lvalue.linha,
                    mensagem: format!(
                        "'{}' não pode ser usado como variável aqui — é {}.",
                        lvalue.nome,
                        outro.descricao()
                    ),
                });
                return None;
            }
        };

        // Qualificador de escopo:
        // 'CLS_BASE..NOME.CAMPO' desambigua de qual base resolver o
        // PRIMEIRO acesso da cadeia. Valida aqui que a base indicada é
        // de fato uma ancestral (direta ou indireta) do tipo declarado
        // de 'lvalue.nome' — sem isso, a qualificação não tem sentido
        // (não é possível "ver" um objeto como uma classe não
        // relacionada). Acessos seguintes da cadeia, e o caso de
        // 'lvalue.acessos' vazio, não usam o qualificador (não há
        // ambiguidade possível: o tipo já está resolvido a essa altura).
        if let Some(base) = &lvalue.qualificador_base {
            match &tipo_atual {
                TipoResolvido::Classe { nome: nome_classe, .. } => {
                    if !e_subclasse_de(nome_classe, base, &self.tabela_heranca) {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!(
                                "'{base}' não é uma classe-base de '{nome_classe}' — a \
                                 qualificação 'CLS_BASE..{}' (Fase 6) só \
                                 faz sentido quando 'CLS_BASE' é, direta ou indiretamente, \
                                 uma das classes-base de '{nome_classe}'.",
                                lvalue.nome
                            ),
                        });
                        return None;
                    }
                    if !self.info_classes.contains_key(&base.to_lowercase()) {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!("'{base}' não é uma classe declarada ."),
                        });
                        return None;
                    }
                }
                outro => {
                    self.erros.push(ErroSemantico {
                        linha: lvalue.linha,
                        mensagem: format!(
                            "a qualificação 'CLS_BASE..{}' só se aplica a instâncias de \
                             classe (Fase 6) — '{}' é '{}'.",
                            lvalue.nome,
                            lvalue.nome,
                            outro.nome_exibicao()
                        ),
                    });
                    return None;
                }
            }
        }

        for (i, acesso) in lvalue.acessos.iter().enumerate() {
            // A partir de qual classe resolver este acesso: a base
            // qualificada, só no primeiro acesso da cadeia; em
            // qualquer outro caso, a classe do tipo atual normalmente.
            let classe_partida_override =
                if i == 0 { lvalue.qualificador_base.as_deref() } else { None };
            match acesso {
                Acesso::Campo(nome_campo) => match &tipo_atual {
                    TipoResolvido::Registro(campos) => {
                        match campos.iter().find(|c| c.nome.eq_ignore_ascii_case(nome_campo)) {
                            Some(c) => tipo_atual = c.tipo.clone(),
                            None => {
                                self.erros.push(ErroSemantico {
                                    linha: lvalue.linha,
                                    mensagem: format!(
                                        "o tipo de '{}' não tem campo '{}' .",
                                        lvalue.nome, nome_campo
                                    ),
                                });
                                return None;
                            }
                        }
                    }
                    TipoResolvido::Classe { nome: nome_classe, .. } => {
                        let classe_origem = classe_partida_override.unwrap_or(nome_classe.as_str());
                        match self.buscar_campo_com_heranca(classe_origem, nome_campo) {
                            ResolucaoMembro::Encontrado(campo, classe_dono) => {
                                self.checar_visibilidade(
                                    campo.visibilidade,
                                    &classe_dono,
                                    nome_campo,
                                    false,
                                    lvalue.linha,
                                );
                                tipo_atual = campo.tipo;
                            }
                            ResolucaoMembro::Ambiguo(doadoras) => {
                                self.erros.push(ErroSemantico {
                                    linha: lvalue.linha,
                                    mensagem: format!(
                                        "'{}' é ambíguo em '{}' — existe em mais de uma \
                                         classe-base ({}) sem qualificação. Use \
                                         'CLS_BASE..{}' para indicar de qual base vem \
                                         (Fase 6 — herança múltipla).",
                                        nome_campo,
                                        classe_origem,
                                        doadoras.join(", "),
                                        nome_campo
                                    ),
                                });
                                return None;
                            }
                            ResolucaoMembro::NaoEncontrado => {
                                self.erros.push(ErroSemantico {
                                    linha: lvalue.linha,
                                    mensagem: format!(
                                        "a classe '{}' não tem campo '{}' .",
                                        classe_origem, nome_campo
                                    ),
                                });
                                return None;
                            }
                        }
                    }
                    outro => {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!(
                                "não é possível acessar o campo '.{}' em '{}' — \
                                 não é um 'registro' nem uma instância de classe (é '{}').",
                                nome_campo,
                                lvalue.nome,
                                outro.nome_exibicao()
                            ),
                        });
                        return None;
                    }
                },
                Acesso::Indice(indices) => match &tipo_atual {
                    TipoResolvido::Conjunto { dimensoes, elemento } => {
                        if indices.len() != dimensoes.len() {
                            self.erros.push(ErroSemantico {
                                linha: lvalue.linha,
                                mensagem: format!(
                                    "'{}' tem {} dimensão(ões), mas foi indexado com {}.",
                                    lvalue.nome,
                                    dimensoes.len(),
                                    indices.len()
                                ),
                            });
                        }
                        for idx in indices {
                            self.exigir_tipo_numerico(idx, lvalue.linha, "um índice");
                        }
                        tipo_atual = (**elemento).clone();
                    }
                    outro => {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!(
                                "não é possível indexar '{}' com '[...]' — \
                                 não é um 'conjunto' (é '{}').",
                                lvalue.nome,
                                outro.nome_exibicao()
                            ),
                        });
                        return None;
                    }
                },
                Acesso::Metodo { nome: nome_metodo, argumentos } => match &tipo_atual {
                    TipoResolvido::Classe { nome: nome_classe, .. } => {
                        let classe_origem =
                            classe_partida_override.unwrap_or(nome_classe.as_str()).to_string();
                        match self.buscar_metodo_com_heranca(&classe_origem, nome_metodo) {
                            ResolucaoMembro::Encontrado(candidatos, classe_dono) => {
                                // Avalia os argumentos uma única vez antes de
                                // resolver qual sobrecarga usar —
                                // mesmo princípio de 'verificar_chamada'.
                                let tipos_argumentos: Vec<Option<TipoResolvido>> =
                                    argumentos.iter().map(|a| self.tipo_de_expr(a)).collect();
                                let assinaturas: Vec<AssinaturaSubRotina> =
                                    candidatos.iter().map(|m| m.assinatura.clone()).collect();
                                let assinatura_escolhida = self.resolver_sobrecarga(
                                    nome_metodo,
                                    &assinaturas,
                                    &tipos_argumentos,
                                    lvalue.linha,
                                );
                                // A visibilidade é a mesma para todas as
                                // sobrecargas de um método na prática (o
                                // material não prevê visibilidade diferente
                                // por sobrecarga), então usar a do primeiro
                                // candidato é seguro.
                                let metodo = candidatos
                                    .iter()
                                    .find(|m| m.assinatura == assinatura_escolhida)
                                    .unwrap_or(&candidatos[0]);
                                self.checar_visibilidade(
                                    metodo.visibilidade,
                                    &classe_dono,
                                    nome_metodo,
                                    true,
                                    lvalue.linha,
                                );
                                self.verificar_argumentos_ja_avaliados(
                                    nome_metodo,
                                    &assinatura_escolhida.parametros,
                                    &tipos_argumentos,
                                    lvalue.linha,
                                );
                                match assinatura_escolhida.tipo_retorno {
                                    Some(tr) => tipo_atual = tr,
                                    None if i == lvalue.acessos.len() - 1 => {
                                        // Último acesso da cadeia, sem retorno ('procedimento'):
                                        // válido como comando solto. Quem
                                        // chama esta função em contexto de expressão
                                        // (Expr::Variavel) decide se um retorno 'None' aqui é
                                        // aceitável ou não — não é responsabilidade desta
                                        // função, que só resolve o tipo do *acesso*, não o
                                        // contexto de uso.
                                        return None;
                                    }
                                    None => {
                                        // 'procedimento' (sem retorno) usado no meio de uma
                                        // cadeia (ex.: '.METODO().CAMPO') — não há valor para
                                        // continuar o encadeamento.
                                        self.erros.push(ErroSemantico {
                                            linha: lvalue.linha,
                                            mensagem: format!(
                                                "'{}' é um procedimento (sem retorno) — não é \
                                                 possível continuar acessando campos/métodos \
                                                 depois dele.",
                                                nome_metodo
                                            ),
                                        });
                                        return None;
                                    }
                                }
                            }
                            ResolucaoMembro::Ambiguo(doadoras) => {
                                self.erros.push(ErroSemantico {
                                    linha: lvalue.linha,
                                    mensagem: format!(
                                        "'{}' é ambíguo em '{}' — existe em mais de uma \
                                         classe-base ({}) sem qualificação. Use \
                                         'CLS_BASE..{}(...)' para indicar de qual base vem \
                                         (Fase 6 — herança múltipla).",
                                        nome_metodo,
                                        classe_origem,
                                        doadoras.join(", "),
                                        nome_metodo
                                    ),
                                });
                                for arg in argumentos {
                                    self.tipo_de_expr(arg);
                                }
                                return None;
                            }
                            ResolucaoMembro::NaoEncontrado => {
                                self.erros.push(ErroSemantico {
                                    linha: lvalue.linha,
                                    mensagem: format!(
                                        "a classe '{}' não tem método '{}' .",
                                        classe_origem, nome_metodo
                                    ),
                                });
                                for arg in argumentos {
                                    self.tipo_de_expr(arg);
                                }
                                return None;
                            }
                        }
                    }
                    outro => {
                        self.erros.push(ErroSemantico {
                            linha: lvalue.linha,
                            mensagem: format!(
                                "não é possível chamar o método '.{}(...)' em '{}' — \
                                 não é uma instância de classe (é '{}').",
                                nome_metodo,
                                lvalue.nome,
                                outro.nome_exibicao()
                            ),
                        });
                        for arg in argumentos {
                            self.tipo_de_expr(arg);
                        }
                        return None;
                    }
                },
            }
        }

        Some(tipo_atual)
    }
}

/// Mensagem didática para um erro de resolução de tipo.
fn erro_resolucao_tipo(
    nome_var_ou_campo: &str,
    erro: ErroResolucaoTipo,
    linha: usize,
) -> ErroSemantico {
    match erro {
        ErroResolucaoTipo::TipoNaoDefinido(nome_tipo) => ErroSemantico {
            linha,
            mensagem: format!(
                "o tipo '{nome_tipo}' não foi definido. Verifique se existe \
                 'tipo {nome_tipo} = ...' em algum lugar do programa, ou se o \
                 nome do tipo{ctx} está escrito corretamente.",
                nome_tipo = nome_tipo,
                ctx = if nome_var_ou_campo.is_empty() {
                    String::new()
                } else {
                    format!(" usado em '{nome_var_ou_campo}'")
                },
            ),
        },
        ErroResolucaoTipo::CicloDeTipos(ciclo) => ErroSemantico {
            linha,
            mensagem: format!(
                "definição de tipo cíclica: {} — um tipo não pode ser definido, \
                 direta ou indiretamente, em termos de si mesmo.",
                ciclo.join(" -> ")
            ),
        },
    }
}

/// Para mensagens de erro: se `tipo` é (ou contém diretamente) uma
/// referência [`Tipo::Nomeado`], retorna esse nome — usado para dizer "o
/// tipo 'X' usado em 'cad_aluno'" quando `cad_aluno` é, ele mesmo, o nome
/// não encontrado, ou quando o erro vem de um campo/elemento aninhado.
fn nome_tipo_em_erro(tipo: &Tipo) -> Option<String> {
    match tipo {
        Tipo::Nomeado(nome) => Some(nome.clone()),
        Tipo::Conjunto { elemento, .. } => nome_tipo_em_erro(elemento),
        _ => None,
    }
}

/// "Acha" um vetor de parâmetros (onde cada [`ParametroResolvido`] pode
/// agrupar vários nomes do mesmo tipo, ex.: `X, Y : inteiro`) para uma
/// lista com um tipo por parâmetro real, na ordem declarada — facilita
/// comparar/percorrer posição a posição (usado tanto para sobrecarga
/// quanto já era feito ad-hoc em `verificar_chamada`/
/// `verificar_argumentos_ja_avaliados`).
fn tipos_expandidos(assinatura: &AssinaturaSubRotina) -> Vec<&TipoResolvido> {
    assinatura
        .parametros
        .iter()
        .flat_map(|p| std::iter::repeat(&p.tipo).take(p.nomes.len()))
        .collect()
}

/// Duas assinaturas têm a **mesma aridade e tipos de parâmetro**, na
/// mesma ordem — categoria e tipo de retorno não importam aqui (são
/// usados em outro lugar para decidir se duas sobrecargas de mesmo nome
/// podem coexistir; ver [`Verificador::pode_sobrecarregar`]). Usado
/// tanto para detectar uma sobrecarga redundante (erro de redeclaração)
/// quanto, na resolução de chamada, para achar a candidata que casa
/// exatamente com os tipos dos argumentos.
fn mesma_lista_de_tipos(a: &AssinaturaSubRotina, b: &AssinaturaSubRotina) -> bool {
    let ta = tipos_expandidos(a);
    let tb = tipos_expandidos(b);
    ta.len() == tb.len() && ta.iter().zip(tb.iter()).all(|(x, y)| x == y)
}

/// Compara duas assinaturas para fins de override:
/// mesma categoria (`procedimento`/`função`), mesma aridade, mesmo tipo
/// de retorno e mesmo tipo (e modo de passagem) em cada parâmetro, na
/// mesma ordem. **Nomes de parâmetro não importam** — `EXECUTA(X :
/// inteiro)` e `EXECUTA(VALOR : inteiro)` são a mesma assinatura para
/// este efeito (igual a C++/Java: o nome do parâmetro não faz parte da
/// assinatura).
fn assinaturas_compativeis_para_override(a: &AssinaturaSubRotina, b: &AssinaturaSubRotina) -> bool {
    if a.categoria != b.categoria || a.tipo_retorno != b.tipo_retorno {
        return false;
    }
    if a.parametros.len() != b.parametros.len() {
        return false;
    }
    a.parametros.iter().zip(b.parametros.iter()).all(|(pa, pb)| {
        pa.tipo == pb.tipo && pa.por_referencia == pb.por_referencia && pa.nomes.len() == pb.nomes.len()
    })
}

/// Mensagem didática padrão para uso de um identificador nunca declarado
///.
fn erro_identificador_nao_declarado(nome: &str, linha: usize) -> ErroSemantico {
    ErroSemantico {
        linha,
        mensagem: format!(
            "'{nome}' não foi declarado. Verifique se há uma declaração \
             'var {nome} : <tipo>' (ou 'const'/parâmetro) antes do uso, ou \
             se o nome está escrito corretamente."
        ),
    }
}

/// Descrição de um [`LValue`] em sintaxe PEPPE, para mensagens de erro
/// (ex.: `NOME` ou `NOME.CAMPO[i]`).
fn nome_lvalue(lvalue: &LValue) -> String {
    let mut s = lvalue.nome.clone();
    for acesso in &lvalue.acessos {
        match acesso {
            Acesso::Campo(nome) => {
                s.push('.');
                s.push_str(nome);
            }
            Acesso::Indice(_) => s.push_str("[...]"),
            Acesso::Metodo { nome, .. } => {
                s.push('.');
                s.push_str(nome);
                s.push_str("(...)");
            }
        }
    }
    s
}

/// `true` para `inteiro`/`real`/`generico` — tipos aceitáveis como variável
/// de controle de um `para`.
fn numerico_para_controle(t: &TipoResolvido) -> bool {
    matches!(t, TipoResolvido::Inteiro | TipoResolvido::Real | TipoResolvido::Generico)
}

/// Converte um [`TipoPrimitivo`] (usado em *casts*) para
/// [`TipoResolvido`].
fn tipo_primitivo_para_resolvido(tp: TipoPrimitivo) -> TipoResolvido {
    match tp {
        TipoPrimitivo::Inteiro => TipoResolvido::Inteiro,
        TipoPrimitivo::Real => TipoResolvido::Real,
        TipoPrimitivo::Cadeia => TipoResolvido::Cadeia,
        TipoPrimitivo::Caractere => TipoResolvido::Caractere,
        TipoPrimitivo::Logico => TipoResolvido::Logico,
    }
}

/// Se `nome` (case-insensitive) corresponde a uma função pré-definida —
/// matemática ou de texto —, retorna seu tipo de
/// retorno. Usado para que chamar `raizq(...)`/`tamanho(...)`/etc. não gere
/// falso erro de "não declarado", *e* para que o tipo resultante seja o
/// correto (não uma aproximação genérica) — importante para detectar, por
/// exemplo, `N <- tamanho(S)` como `inteiro <- inteiro` (direto) em vez de
/// erroneamente exigir um cast.
///
/// A verificação completa de aridade/tipos dos *argumentos* dessas funções
/// continua sendo um refinamento futuro; aqui garantimos apenas o tipo
/// de retorno.
fn tipo_retorno_predefinida(nome: &str) -> Option<TipoResolvido> {
    const RETORNAM_INTEIRO: &[&str] = &[
        "piso", "teto", "arredonda", "trunca", "sinal", "tamanho", "posição",
        "ord", "succ", "pred",
    ];
    const RETORNAM_CADEIA: &[&str] = &[
        "cópia", "aparar", "maiúsculo", "minúsculo", "concatenar",
    ];
    const RETORNAM_REAL: &[&str] = &[
        "raizq", "raizc", "raize", "potência", "seno", "cosseno", "tangente",
        "arco_seno", "arco_cosseno", "arco_tangente", "graus_para_radianos",
        "radianos_para_graus", "log", "log10", "exp", "aleatório",
    ];
    const RETORNAM_CARACTERE: &[&str] = &["chr"];
    // 'abs'/'máximo'/'mínimo' preservam o tipo do(s) argumento(s) em tempo
    // de execução (inteiro com inteiro -> inteiro; senão -> real) — não dá
    // para saber estaticamente sem inspecionar os argumentos. Aproximação
    // aceitável: tratá-las como 'real' aqui (compatibilidade direta com
    // 'inteiro' via coerção numérica — nunca exige cast).
    const RETORNAM_REAL_APROXIMADO: &[&str] = &["abs", "máximo", "mínimo"];
    // Funções de cast também podem aparecer como Expr::Chamada
    // quando o parser não as reconhece como tipo primitivo (não deveria
    // ocorrer, mas mantemos por segurança) — seu tipo de retorno é o
    // próprio nome do tipo.
    let nome_lower = nome.to_lowercase();

    if nome_lower.eq_ignore_ascii_case("inteiro") {
        return Some(TipoResolvido::Inteiro);
    }
    if nome_lower.eq_ignore_ascii_case("real") {
        return Some(TipoResolvido::Real);
    }
    if nome_lower.eq_ignore_ascii_case("cadeia") {
        return Some(TipoResolvido::Cadeia);
    }
    if nome_lower.eq_ignore_ascii_case("caractere") {
        return Some(TipoResolvido::Caractere);
    }
    if nome_lower == "lógico" || nome_lower == "logico" {
        return Some(TipoResolvido::Logico);
    }

    if RETORNAM_INTEIRO.iter().any(|p| *p == nome_lower) {
        return Some(TipoResolvido::Inteiro);
    }
    if RETORNAM_CADEIA.iter().any(|p| *p == nome_lower) {
        return Some(TipoResolvido::Cadeia);
    }
    if RETORNAM_REAL.iter().any(|p| *p == nome_lower) {
        return Some(TipoResolvido::Real);
    }
    if RETORNAM_CARACTERE.iter().any(|p| *p == nome_lower) {
        return Some(TipoResolvido::Caractere);
    }
    if RETORNAM_REAL_APROXIMADO.iter().any(|p| *p == nome_lower) {
        return Some(TipoResolvido::Real);
    }
    None
}

// =====================================================================================
// Testes
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::tokenizar, parser::parsear};

    fn verificar_fonte(fonte: &str) -> ResultadoVerificacao {
        let tokens = tokenizar(fonte).expect("erro léxico inesperado");
        let programa = parsear(tokens).expect("erro sintático inesperado");
        verificar(&programa)
    }

    #[test]
    fn programa_simples_sem_erros() {
        let r = verificar_fonte(
            r#"programa ADIÇÃO_NÚMEROS
var
  X, A, B : inteiro
início
  leia A
  leia B
  X <- A + B
  escreva X
fim"#,
        );
        assert_eq!(r.erros, vec![]);

        let x = r.tabela_global.buscar("X").expect("X deveria estar na tabela global");
        assert_eq!(x.categoria, CategoriaSimbolo::Var(TipoResolvido::Inteiro));

        // Case-insensitive: busca por 'x' (minúsculo) encontra 'X'.
        let x_minusculo = r.tabela_global.buscar("x").expect("busca case-insensitive");
        assert_eq!(x_minusculo.nome_original, "X");
    }

    #[test]
    fn redeclaracao_case_insensitive_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  NOTA : real
  nota : real
início
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("já foi declarado"));
        assert!(r.erros[0].mensagem.contains("NOTA"));
        assert!(r.erros[0].mensagem.contains("nota"));
    }

    #[test]
    fn tipo_nao_definido_em_var() {
        let r = verificar_fonte(
            r#"programa P
var
  ALUNO : nao_existe
início
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'nao_existe' não foi definido"));
    }

    #[test]
    fn ciclo_de_tipos_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  A = B
  B = A
var
  X : a
início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("cíclica")));
    }

    #[test]
    fn tipos_em_ordem_de_dependencia_invertida() {
        // CAD_ALUNO usa BIMESTRE, mas BIMESTRE é declarado DEPOIS —
        // referência "para a frente" deve funcionar (coletar_tipos, passada 1).
        let r = verificar_fonte(
            r#"programa P
tipo
  CAD_ALUNO = registro
                NOME  : cadeia
                NOTAS : bimestre
              fim_registro
  BIMESTRE = conjunto [1..4] de real
var
  ALUNO : cad_aluno
início
fim"#,
        );
        assert_eq!(r.erros, vec![]);

        let cad = r.tabela_global.buscar("CAD_ALUNO").unwrap();
        match &cad.categoria {
            CategoriaSimbolo::Tipo(TipoResolvido::Registro(campos)) => {
                assert_eq!(campos.len(), 2);
                assert_eq!(campos[1].nome, "NOTAS");
                assert!(matches!(campos[1].tipo, TipoResolvido::Conjunto { .. }));
            }
            outro => panic!("esperava Tipo(Registro), encontrei {outro:?}"),
        }
    }

    #[test]
    fn tipo_duplicado_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  BIMESTRE = conjunto [1..4] de real
  BIMESTRE = conjunto [1..2] de inteiro
var
  X : bimestre
início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("já foi definido")));
    }

    #[test]
    fn procedimento_com_parametros_padrao_a() {
        // CALC_FAT_V2 (marcador 'ref')
        let r = verificar_fonte(
            r#"programa P
  procedimento FATORIAL(N : inteiro; ref FAT : inteiro)
  var
    I : inteiro
  início
    para I de 1 até N passo 1 faça
      FAT <- FAT * I
    fim_para
  fim
var
  LIMITE, RESP : inteiro
início
  RESP <- 1
  FATORIAL(LIMITE, RESP)
fim"#,
        );
        assert_eq!(r.erros, vec![]);

        let fatorial = r.tabela_global.buscar("FATORIAL").unwrap();
        match &fatorial.categoria {
            CategoriaSimbolo::SubRotina(assinaturas) => {
                assert_eq!(assinaturas.len(), 1);
                let a = &assinaturas[0];
                assert_eq!(a.categoria, CategoriaSubRotina::Procedimento);
                assert_eq!(a.tipo_retorno, None);
                assert_eq!(a.parametros.len(), 2);
                assert_eq!(a.parametros[0].nomes, vec!["N"]);
                assert!(!a.parametros[0].por_referencia);
                assert_eq!(a.parametros[1].nomes, vec!["FAT"]);
                assert!(a.parametros[1].por_referencia);
            }
            outro => panic!("esperava SubRotina, encontrei {outro:?}"),
        }
    }

    #[test]
    fn funcao_com_tipo_retorno_resolvido() {
        let r = verificar_fonte(
            r#"programa P
  função FATORIAL(N : inteiro) : inteiro
  var
    I, FAT : inteiro
  início
    FAT <- 1
    para I de 1 até N passo 1 faça
      FAT <- FAT * I
    fim_para
    FATORIAL <- FAT
  fim
var
  LIMITE : inteiro
início
  leia LIMITE
  escreva FATORIAL(LIMITE)
fim"#,
        );
        assert_eq!(r.erros, vec![]);

        let fatorial = r.tabela_global.buscar("FATORIAL").unwrap();
        match &fatorial.categoria {
            CategoriaSimbolo::SubRotina(assinaturas) => {
                assert_eq!(assinaturas.len(), 1);
                let a = &assinaturas[0];
                assert_eq!(a.categoria, CategoriaSubRotina::Funcao);
                assert_eq!(a.tipo_retorno, Some(TipoResolvido::Inteiro));
            }
            outro => panic!("esperava SubRotina, encontrei {outro:?}"),
        }
    }

    #[test]
    fn parametro_com_mesmo_nome_da_funcao_e_erro() {
        // O nome da função já ocupa o "slot" de retorno dentro do seu
        // próprio escopo — um parâmetro com o mesmo nome colide.
        let r = verificar_fonte(
            r#"programa P
  função F(F : inteiro) : inteiro
  início
    F <- F
  fim
início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("já foi declarado")));
    }

    #[test]
    fn registro_e_conjunto_anonimos_em_var() {
        // Tipos declarados inline (sem 'tipo NOME = ...').
        let r = verificar_fonte(
            r#"programa P
var
  NOTAS : conjunto [1..4] de real
  ALUNO : registro
            NOME : cadeia
            IDADE : inteiro
          fim_registro
início
fim"#,
        );
        assert_eq!(r.erros, vec![]);

        let notas = r.tabela_global.buscar("NOTAS").unwrap();
        assert!(matches!(notas.categoria, CategoriaSimbolo::Var(TipoResolvido::Conjunto { .. })));

        let aluno = r.tabela_global.buscar("ALUNO").unwrap();
        match &aluno.categoria {
            CategoriaSimbolo::Var(TipoResolvido::Registro(campos)) => {
                assert_eq!(campos.len(), 2);
                assert_eq!(campos[0].nome, "NOME");
                assert_eq!(campos[1].nome, "IDADE");
            }
            outro => panic!("esperava Var(Registro), encontrei {outro:?}"),
        }
    }

    #[test]
    fn const_tipos_resolvidos() {
        let r = verificar_fonte(
            r#"programa P
const
  PI = 3.14159
  LIMITE = 100
  SAUDACAO = "Olá"
  ATIVO = .verdadeiro.
início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
        assert_eq!(
            r.tabela_global.buscar("PI").unwrap().categoria,
            CategoriaSimbolo::Const(TipoResolvido::Real)
        );
        assert_eq!(
            r.tabela_global.buscar("LIMITE").unwrap().categoria,
            CategoriaSimbolo::Const(TipoResolvido::Inteiro)
        );
        assert_eq!(
            r.tabela_global.buscar("SAUDACAO").unwrap().categoria,
            CategoriaSimbolo::Const(TipoResolvido::Cadeia)
        );
        assert_eq!(
            r.tabela_global.buscar("ATIVO").unwrap().categoria,
            CategoriaSimbolo::Const(TipoResolvido::Logico)
        );
    }

    #[test]
    fn const_e_var_com_mesmo_nome_colidem() {
        let r = verificar_fonte(
            r#"programa P
const
  LIMITE = 100
var
  limite : inteiro
início
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("uma constante"));
    }

    #[test]
    fn sub_rotina_aninhada_compartilha_escopo_do_pai() {
        // Procedimento aninhado dentro de outro, sem erros.
        let r = verificar_fonte(
            r#"programa P
  procedimento EXTERNO(N : inteiro)
  var
    I : inteiro

    procedimento INTERNO(X : inteiro)
    início
      I <- X
    fim

  início
    I <- N
    INTERNO(N)
  fim
início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn identificador_nao_declarado_em_expressao() {
        let r = verificar_fonte(
            r#"programa P
var
  X : inteiro
início
  X <- Y + 1
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'Y' não foi declarado"));
    }

    #[test]
    fn atribuicao_incompativel_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  ATIVO : lógico
  N : inteiro
início
  ATIVO <- N
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("incompatíveis"));
    }

    #[test]
    fn atribuicao_que_precisa_cast_e_erro_didatico() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
  R : real
início
  R <- 3.5
  N <- R
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("conversão explícita"));
    }

    #[test]
    fn atribuicao_inteiro_para_real_e_direta() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
  R : real
início
  N <- 5
  R <- N
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn condicao_nao_logica_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  se (N) então
    escreva N
  fim_se
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'lógico'"));
    }

    #[test]
    fn operador_binario_incompativel_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  NOME : cadeia
  N : inteiro
início
  NOME <- NOME + N
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("cadeia(")));
    }

    #[test]
    fn interrompa_fora_de_laco_e_erro() {
        let r = verificar_fonte(
            r#"programa P
início
  interrompa
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'interrompa'"));
    }

    #[test]
    fn saia_caso_fora_de_laco_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  saia_caso (N > 0)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("'saia_caso'")));
    }

    #[test]
    fn interrompa_dentro_de_laco_e_valido() {
        let r = verificar_fonte(
            r#"programa P
var
  I : inteiro
início
  laço
    I <- I + 1
    saia_caso (I > 10)
    interrompa
  fim_laço
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn interrompa_dentro_de_para_e_valido() {
        let r = verificar_fonte(
            r#"programa P
var
  I : inteiro
início
  para I de 1 até 10 passo 1 faça
    interrompa
  fim_para
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn continue_fora_de_laco_e_erro() {
        let r = verificar_fonte(
            r#"programa P
início
  continue
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'continue'"));
    }

    #[test]
    fn continue_dentro_de_laco_e_valido() {
        let r = verificar_fonte(
            r#"programa P
var
  I : inteiro
início
  para I de 1 até 10 passo 1 faça
    continue
  fim_para
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn chamada_com_aridade_errada_e_erro() {
        let r = verificar_fonte(
            r#"programa P
  procedimento SOMA(A, B : inteiro)
  início
  fim
início
  SOMA(1)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("espera 2 argumento")));
    }

    #[test]
    fn chamada_com_tipo_de_argumento_incompativel_e_erro() {
        let r = verificar_fonte(
            r#"programa P
  procedimento SOMA(A, B : inteiro)
  início
  fim
var
  NOME : cadeia
início
  SOMA(NOME, 2)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("argumento 1")));
    }

    #[test]
    fn chamar_funcao_como_procedimento_e_erro() {
        let r = verificar_fonte(
            r#"programa P
  função DOBRO(X : inteiro) : inteiro
  início
    DOBRO <- X * 2
  fim
início
  DOBRO(5)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("é uma função")));
    }

    #[test]
    fn acesso_a_campo_inexistente_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CAD_ALUNO = registro
                NOME : cadeia
              fim_registro
var
  ALUNO : cad_aluno
início
  escreva ALUNO.IDADE
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não tem campo 'IDADE'")));
    }

    #[test]
    fn acesso_a_campo_em_nao_registro_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  escreva N.CAMPO
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não é um 'registro'")));
    }

    #[test]
    fn indice_em_nao_conjunto_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  escreva N[1]
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não é um 'conjunto'")));
    }

    #[test]
    fn acesso_de_campo_e_indice_encadeado_valido() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CAD = registro
          NOTAS : conjunto [1..4] de real
        fim_registro
var
  ALUNO : conjunto [1..8] de cad
  I, J : inteiro
  X : real
início
  ALUNO[I].NOTAS[J] <- X
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn dimensione_em_variavel_nao_conjunto_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  dimensione N[1..10]
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("'dimensione' só pode")));
    }

    #[test]
    fn dimensione_valida_sem_erros() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
  A : conjunto [] de cadeia
início
  leia N
  dimensione A[1..N]
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn dimensione_matriz_2d_com_numero_correto_de_dimensoes() {
        let r = verificar_fonte(
            r#"programa P
var
  L, C : inteiro
  M : conjunto [,] de real
início
  leia L
  leia C
  dimensione M[1..L, 1..C]
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn dimensione_com_numero_de_dimensoes_diferente_do_declarado_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
  M : conjunto [,] de real
início
  leia N
  dimensione M[1..N]
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("2 dimensão(ões)"));
        assert!(r.erros[0].mensagem.contains("forneceu 1"));
    }

    #[test]
    fn variavel_de_controle_para_nao_numerica_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  NOME : cadeia
início
  para NOME de 1 até 10 passo 1 faça
    escreva NOME
  fim_para
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("variável de controle")));
    }

    #[test]
    fn escreva_com_decimais_em_inteiro_e_erro() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
início
  escreva N : 8 : 2
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("só faz sentido para valores 'real'")));
    }

    #[test]
    fn escreva_com_formatacao_valida_sem_erros() {
        let r = verificar_fonte(
            r#"programa P
var
  R : real
  N : inteiro
início
  escreva R : 8 : 2
  escreva N : 8
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn chamada_a_funcao_predefinida_nao_gera_erro() {
        // 'raizq' não foi declarada pelo usuário, mas é pré-definida
        // — não deve gerar "não declarado".
        let r = verificar_fonte(
            r#"programa P
var
  X : real
início
  X <- raizq(16.0)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn tamanho_retorna_inteiro_sem_exigir_cast() {
        // 'tamanho' deve ser inferido como 'inteiro' diretamente — antes
        // da correção, a aproximação genérica 'real' faria
        // este teste falhar exigindo cast.
        let r = verificar_fonte(
            r#"programa P
var
  NOME : cadeia
  N : inteiro
início
  NOME <- "Maria"
  N <- tamanho(NOME)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn posicao_retorna_inteiro_sem_exigir_cast() {
        let r = verificar_fonte(
            r#"programa P
var
  P : inteiro
início
  P <- posição("ana", "banana")
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn copia_retorna_cadeia_sem_exigir_cast() {
        let r = verificar_fonte(
            r#"programa P
var
  TRECHO : cadeia
início
  TRECHO <- cópia("Olá, mundo", 1, 3)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn cast_explicito_em_expressao_e_aceito() {
        let r = verificar_fonte(
            r#"programa P
var
  N : inteiro
  R : real
início
  R <- 3.7
  N <- inteiro(R)
  N <- (inteiro) R
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn ir_para_rotulo_existente_e_valido() {
        let r = verificar_fonte(
            r#"programa P
var
  I : inteiro
início
  I <- 1
  INICIO_DO_LACO:
    escreva I
    I <- I + 1
    se (I <= 3) então
      ir_para INICIO_DO_LACO
    fim_se
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn ir_para_rotulo_inexistente_e_erro() {
        let r = verificar_fonte(
            r#"programa P
início
  ir_para NAO_EXISTE
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'NAO_EXISTE' não foi declarado"));
    }

    #[test]
    fn ir_para_para_a_frente_e_valido() {
        // Referência "para a frente" — o rótulo aparece depois do 'ir_para'
        // no código, mas ainda está no mesmo bloco da sub-rotina.
        // (Nome do rótulo não pode ser 'FIM': é a palavra-chave 'fim' em
        // qualquer grafia, PEPPE é case-insensitive .)
        let r = verificar_fonte(
            r#"programa P
início
  ir_para TERMINO
  escreva "isto não deveria executar"
  TERMINO:
    escreva "fim"
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn rotulo_duplicado_case_insensitive_e_erro() {
        let r = verificar_fonte(
            r#"programa P
início
  ROTULO:
    escreva "a"
  rotulo:
    escreva "b"
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("já foi declarado"));
    }

    #[test]
    fn rotulo_dentro_de_se_e_visivel_para_ir_para_fora() {
        // Escopo de rótulo é a sub-rotina inteira, não o bloco
        // aninhado onde ele aparece.
        let r = verificar_fonte(
            r#"programa P
var
  X : inteiro
início
  leia X
  se (X > 0) então
    DENTRO:
      escreva "positivo"
  fim_se
  ir_para DENTRO
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn ir_para_nao_cruza_para_outra_subrotina() {
        let r = verificar_fonte(
            r#"programa P
  procedimento A()
  início
    RA:
      escreva "a"
  fim
início
  ir_para RA
fim"#,
        );
        assert_eq!(r.erros.len(), 1);
        assert!(r.erros[0].mensagem.contains("'RA' não foi declarado"));
    }

    #[test]
    fn programa_completo_adicao_numeros_sem_erros() {
        // Garantia de regressão: o exemplo de referência do livro continua
        // passando por toda a pipeline (lexer -> parser -> checker).
        let r = verificar_fonte(
            r#"programa ADIÇÃO_NÚMEROS
var
  X, A, B : inteiro
início
  escreva "ADIÇÃO DE NÚMEROS\n"
  escreva "Entre o 1o. valor numérico inteiro: "
  leia A
  escreva "Entre o 2o. valor numérico inteiro: "
  leia B
  X <- A + B
  escreva "Resultado = ", X : 8, "\n"
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    // =====================================================================================
    // Programação Orientada a Objetos: classe sem herança
    // =====================================================================================

    #[test]
    fn classe_com_metodo_interno_completo_sem_erros() {
        // Equivalente a CLASSE_OBJETO_MÉTODO_INTERNO do material de origem.
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
              NOTAS : conjunto [1..4] de real
              MÉDIA : real
              função CALCMÉDIA() : real
              var
                I : inteiro
                SOMA : real
              início
                SOMA <- 0
                para I de 1 até 4 passo 1 faça
                  SOMA <- SOMA + NOTAS[I]
                fim_para
                MÉDIA <- SOMA / 4
                CALCMÉDIA <- MÉDIA
              fim
          fim_classe

objeto
  ESTUDANTE : Aluno

var
  I : inteiro

início
  leia ESTUDANTE.NOME
  para I de 1 até 4 passo 1 faça
    leia ESTUDANTE.NOTAS[I]
  fim_para
  ESTUDANTE.CALCMÉDIA()
  escreva ESTUDANTE.NOME
  escreva ESTUDANTE.MÉDIA
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn classe_com_metodo_externo_completo_sem_erros() {
        // Equivalente a CLASSE_OBJETO_MÉTODO_EXTERNO do material de origem.
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME  : cadeia
              NOTAS : conjunto [1..4] de real
              MÉDIA : real
              função CALCMÉDIA : real
           fim_classe

  função Aluno..CALCMÉDIA() : real
  var
    I : inteiro
    SOMA : real
  início
    SOMA <- 0
    para I de 1 até 4 passo 1 faça
      SOMA <- SOMA + NOTAS[I]
    fim_para
    MÉDIA <- SOMA / 4
    CALCMÉDIA <- MÉDIA
  fim

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.CALCMÉDIA()
  escreva ESTUDANTE.MÉDIA
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn metodo_declarado_sem_implementacao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              função CALCMÉDIA : real
          fim_classe

objeto
  ESTUDANTE : Aluno

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("nunca foi implementado")));
    }

    #[test]
    fn acesso_a_campo_inexistente_em_classe_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.SOBRENOME
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não tem campo")));
    }

    #[test]
    fn chamada_de_metodo_inexistente_em_classe_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.METODO_FANTASMA()
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não tem método")));
    }

    #[test]
    fn objeto_e_var_sao_equivalentes_sem_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  A : Aluno

var
  B : Aluno

início
  escreva A.NOME
  escreva B.NOME
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn atribuicao_entre_classe_base_e_derivada_via_heranca_e_aceita() {
        // Equivalente ao núcleo de POLIFORMISMO_UNIVERSAL_INCLUSÃO:
        // 'REFERENCIA <- OBJ2' onde REFERENCIA é da classe-base e OBJ2 é
        // da derivada. Nome não pode ser 'REF': colide com a
        // palavra-chave reservada 'ref' (case-insensitive).
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
  fim

objeto
  REFERENCIA : Pai

var
  OBJ2 : Filho

início
  REFERENCIA <- OBJ2
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn atribuicao_entre_classes_nao_relacionadas_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            NOME : cadeia
        fim_classe

  Outro = classe
            seção_pública
              VALOR : inteiro
          fim_classe

objeto
  A : Pai

var
  B : Outro

início
  A <- B
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("incompatíveis")));
    }

    #[test]
    fn chamada_de_metodo_como_destino_de_atribuicao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              função CALCMÉDIA : real
          fim_classe

  função Aluno..CALCMÉDIA() : real
  início
    CALCMÉDIA <- 0
  fim

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.CALCMÉDIA() <- 5
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não é um lugar que pode receber atribuição")));
    }

    #[test]
    fn parametro_com_mesmo_nome_de_campo_sombreia_campo_em_metodo_externo() {
        // Equivalente ao núcleo de ENCAPSULAMENTO: PÕENOME(NOME : cadeia)
        // com campo NOME — dentro do método, NOME é o parâmetro;
        // este.NOME é o campo.
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              procedimento PÕENOME(NOME : cadeia)
            seção_privada
              NOME : cadeia
          fim_classe

  procedimento Aluno..PÕENOME(NOME : cadeia)
  início
    este.NOME <- NOME
  fim

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.PÕENOME("Ana")
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    // =====================================================================================
    // Programação Orientada a Objetos: encapsulamento aplicado
    // =====================================================================================

    #[test]
    fn acesso_externo_a_campo_privado_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_privada
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.NOME
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("privado")));
    }

    #[test]
    fn acesso_externo_a_metodo_privado_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_privada
              função CALCULA_INTERNO : real
          fim_classe

  função Aluno..CALCULA_INTERNO() : real
  início
    CALCULA_INTERNO <- 0
  fim

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.CALCULA_INTERNO()
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("privado")));
    }

    #[test]
    fn acesso_a_campo_publico_de_fora_da_classe_continua_permitido() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.NOME
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn acesso_a_campo_privado_de_dentro_de_outro_metodo_da_mesma_classe_e_permitido() {
        // 'OUTRO.NOME' (não 'este.NOME') dentro de um método de Aluno,
        // acessando o campo privado de OUTRA instância da MESMA classe —
        // deve ser permitido (a regra é por classe, não por instância).
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              função MESMO_NOME(OUTRO : Aluno) : lógico
            seção_privada
              NOME : cadeia
          fim_classe

  função Aluno..MESMO_NOME(OUTRO : Aluno) : lógico
  início
    MESMO_NOME <- (NOME = OUTRO.NOME)
  fim

início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn acesso_a_campo_privado_de_fora_da_classe_mesmo_dentro_de_outra_classe_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_privada
              NOME : cadeia
          fim_classe

  Turma = classe
            seção_pública
              função PEGA_NOME(A : Aluno) : cadeia
          fim_classe

  função Turma..PEGA_NOME(A : Aluno) : cadeia
  início
    PEGA_NOME <- A.NOME
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("privado")));
    }

    #[test]
    fn acesso_a_campo_protegido_de_subclasse_e_permitido() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_protegida
            NOME : cadeia
        fim_classe

  Filho = classe herança de Pai
            seção_pública
              função PEGA_NOME() : cadeia
          fim_classe

  função Filho..PEGA_NOME() : cadeia
  início
    PEGA_NOME <- NOME
  fim

início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn acesso_a_campo_protegido_de_classe_nao_relacionada_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_protegida
            NOME : cadeia
        fim_classe

  OutraClasse = classe
                  seção_pública
                    função TENTA(P : Pai) : cadeia
                fim_classe

  função OutraClasse..TENTA(P : Pai) : cadeia
  início
    TENTA <- P.NOME
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("protegido")));
    }

    #[test]
    fn acesso_a_campo_privado_no_bloco_principal_e_erro() {
        // Fora de qualquer método (bloco principal), nem mesmo um campo
        // protegido é acessível — só público.
        let r = verificar_fonte(
            r#"programa P
tipo
  Aluno = classe
            seção_protegida
              NOME : cadeia
          fim_classe

objeto
  ESTUDANTE : Aluno

início
  escreva ESTUDANTE.NOME
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("protegido")));
    }

    // =====================================================================================
    // Programação Orientada a Objetos — virtual/sobrepor
    // =====================================================================================

    #[test]
    fn sobrepor_com_virtual_correspondente_na_base_e_aceito() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            virtual procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
  fim

início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn sobrepor_sem_metodo_correspondente_na_base_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            procedimento OUTRO()
        fim_classe

  procedimento Pai..OUTRO()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("sobrepor")));
    }

    #[test]
    fn sobrepor_de_metodo_nao_virtual_na_base_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não foi declarado 'virtual'")));
    }

    #[test]
    fn sobrepor_com_assinatura_diferente_do_virtual_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            virtual procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor procedimento EXECUTA(X : inteiro)
          fim_classe

  procedimento Filho..EXECUTA(X : inteiro)
  início
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("assinatura")));
    }

    #[test]
    fn metodo_redefinido_sem_sobrepor_quando_base_e_virtual_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            virtual procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("'sobrepor'")));
    }

    #[test]
    fn metodo_com_mesmo_nome_mas_assinatura_diferente_sem_sobrepor_e_aceito() {
        // Assinatura diferente = sobrecarga, não override — não exige
        // 'sobrepor' mesmo que o nome coincida com um 'virtual' da base.
        let r = verificar_fonte(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            virtual procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
  fim

  Filho = classe herança de Pai
            seção_pública
              procedimento EXECUTA(X : inteiro)
          fim_classe

  procedimento Filho..EXECUTA(X : inteiro)
  início
  fim

início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    // =====================================================================================
    // Programação Orientada a Objetos: sobrecarga ad-hoc
    // =====================================================================================

    #[test]
    fn sobrecarga_de_subrotina_solta_com_aridades_diferentes_e_aceita() {
        let r = verificar_fonte(
            r#"programa P
  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função CALCULAR(R, H : real) : real
  início
    CALCULAR <- R * H
  fim

  função CALCULAR(X, Y, Z : inteiro) : inteiro
  início
    CALCULAR <- X + Y + Z
  fim

var
  A : inteiro
  B : real
início
  A <- CALCULAR(5)
  B <- CALCULAR(2.0, 3.0)
  A <- CALCULAR(1, 2, 3)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn sobrecarga_com_mesma_lista_de_tipos_e_erro_de_redeclaracao() {
        let r = verificar_fonte(
            r#"programa P
  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função CALCULAR(Y : inteiro) : inteiro
  início
    CALCULAR <- Y * 3
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("mesma quantidade e tipos de parâmetro")));
    }

    #[test]
    fn sobrecarga_misturando_procedimento_e_funcao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  procedimento CALCULAR(R, H : real)
  início
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("mesma categoria")));
    }

    #[test]
    fn chamada_com_aridade_que_nenhuma_sobrecarga_aceita_e_erro() {
        let r = verificar_fonte(
            r#"programa P
  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função CALCULAR(R, H : real) : real
  início
    CALCULAR <- R * H
  fim

var
  A : inteiro
início
  A <- CALCULAR(1, 2, 3)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("nenhuma versão de 'CALCULAR' aceita 3")));
    }

    #[test]
    fn chamada_ambigua_entre_duas_sobrecargas_e_erro() {
        // 'CALCULAR(5)' com um literal inteiro: a candidata 'inteiro'
        // bate diretamente (mesmo tipo) e a candidata 'real' TAMBÉM bate
        // diretamente, porque inteiro->real é promoção numérica usual
        // ('Compatibilidade::Direta', não 'PrecisaCast' — ver
        // 'tipos::compatibilidade'). Duas candidatas aceitando o mesmo
        // argumento sem cast é exatamente o caso de ambiguidade real
        // (decisão do autor: sempre erro explícito, nunca prioridade
        // implícita entre conversões).
        let r = verificar_fonte(
            r#"programa P
  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função CALCULAR(X : real) : real
  início
    CALCULAR <- X * 2.0
  fim

var
  A : inteiro
início
  A <- CALCULAR(5)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("é ambígua")));
    }

    #[test]
    fn sobrecarga_de_metodo_dentro_de_classe_e_aceita() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Calculadora = classe
                  seção_pública
                    função CALCULAR(X : inteiro) : inteiro
                    função CALCULAR(R, H : real) : real
              fim_classe

  função Calculadora..CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função Calculadora..CALCULAR(R, H : real) : real
  início
    CALCULAR <- R * H
  fim

objeto
  CALC : Calculadora

var
  A : inteiro
  B : real
início
  A <- CALC.CALCULAR(5)
  B <- CALC.CALCULAR(2.0, 3.0)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn sobrecarga_de_metodo_com_mesma_lista_de_tipos_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  Calculadora = classe
                  seção_pública
                    função CALCULAR(X : inteiro) : inteiro
                    função CALCULAR(Y : inteiro) : inteiro
              fim_classe

  função Calculadora..CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X * 2
  fim

  função Calculadora..CALCULAR(Y : inteiro) : inteiro
  início
    CALCULAR <- Y * 3
  fim

início
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("mesma quantidade e tipos de parâmetro")));
    }

    // =====================================================================================
    // Programação Orientada a Objetos: herança múltipla
    // =====================================================================================

    #[test]
    fn heranca_multipla_sem_colisao_de_nome_e_aceita_sem_qualificador() {
        // Exemplo de referência do autor: CLS_ALUNO herda de CLS_SALA e
        // CLS_TURMA, campos de nomes diferentes — acesso direto, sem
        // qualificação, deve funcionar normalmente.
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_protegida
                 SALA : inteiro
             fim_classe

  CLS_TURMA = classe
                seção_protegida
                  TURMA : caractere
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA, de CLS_TURMA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

início
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn heranca_multipla_campo_ambiguo_sem_qualificador_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_pública
                 CODIGO : inteiro
             fim_classe

  CLS_TURMA = classe
                seção_pública
                  CODIGO : inteiro
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA, de CLS_TURMA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

var
  X : inteiro
início
  X <- ALUNO.CODIGO
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("é ambíguo")));
    }

    #[test]
    fn heranca_multipla_campo_ambiguo_com_qualificador_e_aceito() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_pública
                 CODIGO : inteiro
             fim_classe

  CLS_TURMA = classe
                seção_pública
                  CODIGO : inteiro
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA, de CLS_TURMA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

var
  X : inteiro
início
  X <- CLS_SALA..ALUNO.CODIGO
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn qualificador_com_classe_nao_relacionada_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_pública
                 CODIGO : inteiro
             fim_classe

  CLS_OUTRA = classe
                seção_pública
                  X : inteiro
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO

var
  X : inteiro
início
  X <- CLS_OUTRA..ALUNO.CODIGO
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("não é uma classe-base de")));
    }

    #[test]
    fn heranca_multipla_metodo_ambiguo_sem_qualificador_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_A = classe
            seção_pública
              função PEGA() : inteiro
          fim_classe

  função CLS_A..PEGA() : inteiro
  início
    PEGA <- 1
  fim

  CLS_B = classe
            seção_pública
              função PEGA() : inteiro
          fim_classe

  função CLS_B..PEGA() : inteiro
  início
    PEGA <- 2
  fim

  CLS_C = classe herança de CLS_A, de CLS_B
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  OBJ : CLS_C

var
  X : inteiro
início
  X <- OBJ.PEGA()
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("é ambíguo")));
    }

    #[test]
    fn heranca_multipla_metodo_ambiguo_com_qualificador_e_aceito() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_A = classe
            seção_pública
              função PEGA() : inteiro
          fim_classe

  função CLS_A..PEGA() : inteiro
  início
    PEGA <- 1
  fim

  CLS_B = classe
            seção_pública
              função PEGA() : inteiro
          fim_classe

  função CLS_B..PEGA() : inteiro
  início
    PEGA <- 2
  fim

  CLS_C = classe herança de CLS_A, de CLS_B
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  OBJ : CLS_C

var
  X : inteiro
início
  X <- CLS_A..OBJ.PEGA()
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn atribuicao_de_derivada_para_qualquer_uma_das_multiplas_bases_e_aceita() {
        let r = verificar_fonte(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_pública
                 SALA : inteiro
             fim_classe

  CLS_TURMA = classe
                seção_pública
                  TURMA : caractere
              fim_classe

  CLS_ALUNO = classe herança de CLS_SALA, de CLS_TURMA
                seção_pública
                  NOME : cadeia
              fim_classe

objeto
  ALUNO : CLS_ALUNO
  REF_SALA : CLS_SALA
  REF_TURMA : CLS_TURMA

início
  REF_SALA <- ALUNO
  REF_TURMA <- ALUNO
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    // =====================================================================================
    // Funções como valores de primeira classe
    // =====================================================================================

    #[test]
    fn atribuir_subrotina_solta_compativel_a_variavel_de_funcao_e_aceito() {
        // Núcleo de POLIFORMISMO_ADHOC_SOBRECARGA_2 do material de origem.
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função SOMATORIO(N : inteiro) : inteiro
  início
    SOMATORIO <- N
  fim

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- SOMATORIO
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn chamada_indireta_atraves_de_variavel_de_funcao_e_aceita() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função SOMATORIO(N : inteiro) : inteiro
  início
    SOMATORIO <- N
  fim

var
  RESPOSTA : FUNC1
  X : inteiro

início
  RESPOSTA <- SOMATORIO
  X <- RESPOSTA(10)
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn atribuir_subrotina_sobrecarregada_a_variavel_de_funcao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função CALCULAR(X : inteiro) : inteiro
  início
    CALCULAR <- X
  fim

  função CALCULAR(X, Y : inteiro) : inteiro
  início
    CALCULAR <- X + Y
  fim

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- CALCULAR
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("sobrecarregado")));
    }

    #[test]
    fn atribuir_procedimento_a_variavel_de_funcao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  procedimento FAZER(N : inteiro)
  início
  fim

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- FAZER
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("é um procedimento")));
    }

    #[test]
    fn atribuir_subrotina_com_assinatura_incompativel_a_variavel_de_funcao_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função SOMA(X, Y : real) : real
  início
    SOMA <- X + Y
  fim

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- SOMA
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("incompatível")));
    }

    #[test]
    fn atribuir_metodo_de_instancia_a_variavel_de_funcao_e_aceito() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função()

  Aluno = classe
            seção_pública
              função CALCMÉDIA() : real
          fim_classe

  função Aluno..CALCMÉDIA() : real
  início
    CALCMÉDIA <- 0
  fim

objeto
  ESTUDANTE : Aluno

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- ESTUDANTE.CALCMÉDIA
fim"#,
        );
        assert_eq!(r.erros, vec![]);
    }

    #[test]
    fn chamada_indireta_com_aridade_errada_e_erro() {
        let r = verificar_fonte(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função SOMATORIO(N : inteiro) : inteiro
  início
    SOMATORIO <- N
  fim

var
  RESPOSTA : FUNC1
  X : inteiro

início
  RESPOSTA <- SOMATORIO
  X <- RESPOSTA(10, 20)
fim"#,
        );
        assert!(r.erros.iter().any(|e| e.mensagem.contains("espera 1 argumento")));
    }
}
