//! Interpretador *tree-walking* da linguagem PEPPE — núcleo estrutural
//! (seções 1–9 da especificação).
//!
//! Este módulo assume que o programa já passou pelo verificador semântico
//! (`checker.rs`) sem erros — ele não duplica checagens de tipo "estáticas"
//! (variável não declarada, operador incompatível, etc.); seu trabalho é
//! **executar**, e detectar apenas os erros que só podem ocorrer em tempo
//! de execução (divisão por zero, índice fora dos limites, etc. — ver
//! [`ErroExecucao`] e a seção 20.1 da especificação, "tratamento de erros
//! em tempo de execução", ainda em aberto quanto à recuperação via
//! `tente`/`captura`; por ora todo [`ErroExecucao`] é fatal).
//!
//! ## Arquitetura
//!
//! - [`Valor`] — o valor em tempo de execução de cada tipo primitivo, mais
//!   `Registro` (mapa de campos, cópia por valor — seção 10.5) e `Conjunto`
//!   (array N-dimensional "achatado" em um `Vec<Valor>` + as dimensões,
//!   cópia por valor exceto quando acessado por referência via parâmetro
//!   `ref`).
//! - [`Celula`] = `Rc<RefCell<Valor>>` — toda variável no ambiente é uma
//!   célula. Passagem por valor (`vlr`/padrão) clona o **valor** para uma
//!   nova célula; passagem por referência (`ref`) compartilha a **mesma**
//!   célula entre chamador e chamado — é assim que `FATORIAL(N, RESP)`
//!   com `RESP` por referência consegue alterar a variável do chamador.
//! - [`Ambiente`] — pilha de escopos (`Vec<HashMap<String, Celula>>`),
//!   *case-insensitive* (seção 1.3), espelhando a [`crate::checker::TabelaSimbolos`]
//!   mas guardando valores em vez de tipos.
//! - [`Interpretador`] — o avaliador propriamente dito: `executar_programa`,
//!   `executar_bloco`/`executar_comando` (com [`FluxoControle`] para
//!   `interrompa`/retorno de função/`ir_para`), `avaliar_expr`.
//!
//! `leia`/`escreva`/`pausa`/CONIO são abstraídos pela trait [`ConsoleIO`],
//! para permitir tanto um console real (`peppe-cli`, via stdin/stdout)
//! quanto um console "falso" nos testes (buffer de entrada pré-programado +
//! captura da saída).

use crate::ast::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

// =====================================================================================
// Valores em tempo de execução
// =====================================================================================

/// Um valor PEPPE em tempo de execução.
#[derive(Debug, Clone, PartialEq)]
pub enum Valor {
    Inteiro(i64),
    Real(f64),
    /// `cadeia` — qualquer comprimento, incluindo vazio (seção 3).
    Cadeia(String),
    /// `caractere` — sempre exatamente 1 caractere Unicode (seção 3); a
    /// validação de "exatamente 1" é responsabilidade de quem constrói o
    /// valor (ex.: o *cast* `caractere(x)`, seção 10.5.1), não desta enum.
    Caractere(char),
    Logico(bool),
    /// `registro` — mapa de campo (grafia original) -> célula. Cópia por
    /// valor (seção 10.5): clonar um `Valor::Registro` clona o `HashMap`
    /// inteiro e cada célula é re-alocada (nunca compartilhada entre duas
    /// cópias independentes do registro).
    Registro(HashMap<String, Celula>),
    /// `conjunto` — array N-dimensional achatado em ordem *row-major*
    /// (a última dimensão varia mais rápido), junto com os limites
    /// concretos de cada dimensão (após `dimensione`, se dinâmico). Cópia
    /// por valor, mesma observação do `Registro`.
    ///
    /// `elemento_padrao` guarda um valor "molde" do tipo do elemento
    /// (construído uma vez, na criação do `conjunto`, via
    /// [`Interpretador::valor_padrao`]) — usado por `dimensione` (seção
    /// 4.5.1) para preencher novas posições mesmo quando o array está
    /// vazio (dimensão dinâmica nunca dimensionada antes), sem precisar
    /// re-resolver o tipo declarado da variável a partir do nome (o que
    /// exigiria rastrear escopo/tipo por nome separadamente).
    Conjunto { dimensoes: Vec<(i64, i64)>, elementos: Vec<Celula>, elemento_padrao: Box<Valor> },
    /// Instância de `classe` (seção 10.2/10.4) — **semântica de
    /// referência**, diferente de `Registro`/`Conjunto`: `Rc<RefCell<...>>`
    /// compartilhado entre todas as cópias do `Valor`. Atribuir uma
    /// variável de tipo-classe a outra (`REF ← OBJ2`) clona apenas o
    /// `Rc` — ambas passam a apontar para os **mesmos** campos, então
    /// mutar um through `REF` é visível through `OBJ2` e vice-versa
    /// (mesmo comportamento de objetos em Java/Python/C#, e o que o
    /// material de origem espera de `POLIFORMISMO_UNIVERSAL_INCLUSÃO`:
    /// `REF` aponta ora para `OBJ1`, ora para `OBJ2`, sem nunca copiar).
    /// `classe` guarda o nome da classe **concreta** da instância (não o
    /// tipo declarado da variável que a referencia) — é o que permite
    /// dispatch dinâmico de método nas próximas fases (seção 10.6):
    /// mesmo que a variável seja declarada como `Pai`, se a instância
    /// nela for de `Filho`, é o método de `Filho` que executa.
    Objeto { classe: String, campos: Rc<RefCell<HashMap<String, Celula>>> },
    /// Referência a função de primeira classe (seção 10.5.3) —
    /// resultado de atribuir um nome de função sem chamar (`X ←
    /// SOMATORIO`) ou um método de instância sem chamar (`X ←
    /// OBJETO.MÉTODO`) a uma variável de tipo `função`. `receptor =
    /// None` para sub-rotina solta; `receptor = Some(objeto)` para
    /// método — `objeto` é uma cópia do `Valor::Objeto` capturado no
    /// momento da atribuição (clonar um `Objeto` clona só o `Rc`
    /// interno, então isso preserva a identidade real da instância:
    /// mutações posteriores nela continuam visíveis através da
    /// referência, e dispatch dinâmico, seção 10.6, funciona igual a
    /// uma chamada direta — `Self::avaliar_metodo` resolve sempre pela
    /// classe real do objeto, nunca pelo tipo declarado de quem o
    /// capturou).
    ReferenciaFuncao { nome: String, receptor: Option<Box<Valor>> },
}

impl Valor {
    /// Nome do tipo para mensagens de erro de execução (seção 15.3, estilo
    /// consistente com `TipoResolvido::nome_exibicao`).
    pub fn nome_tipo(&self) -> String {
        match self {
            Valor::Inteiro(_) => "inteiro".to_string(),
            Valor::Real(_) => "real".to_string(),
            Valor::Cadeia(_) => "cadeia".to_string(),
            Valor::Caractere(_) => "caractere".to_string(),
            Valor::Logico(_) => "lógico".to_string(),
            Valor::Registro(_) => "registro".to_string(),
            Valor::Conjunto { .. } => "conjunto".to_string(),
            // Nome de exibição é o nome da classe, não a palavra "objeto"
            // — mais útil em mensagens de erro (seção 15.3), e consistente
            // com TipoResolvido::Classe::nome_exibicao (que também usa o
            // nome da classe).
            Valor::Objeto { classe, .. } => classe.clone(),
            Valor::ReferenciaFuncao { .. } => "função".to_string(),
        }
    }

    /// Clona profundamente um valor — usado em toda passagem por valor
    /// (parâmetros sem `ref`, atribuição de `registro`/`conjunto`, seção
    /// 10.5): cada célula de `Registro`/`Conjunto` é re-alocada como uma
    /// célula nova e independente, nunca compartilhada com o original.
    pub fn clonar_por_valor(&self) -> Valor {
        match self {
            Valor::Registro(campos) => Valor::Registro(
                campos
                    .iter()
                    .map(|(k, c)| (k.clone(), nova_celula(c.borrow().clonar_por_valor())))
                    .collect(),
            ),
            Valor::Conjunto { dimensoes, elementos, elemento_padrao } => Valor::Conjunto {
                dimensoes: dimensoes.clone(),
                elementos: elementos
                    .iter()
                    .map(|c| nova_celula(c.borrow().clonar_por_valor()))
                    .collect(),
                elemento_padrao: Box::new(elemento_padrao.clonar_por_valor()),
            },
            // 'Objeto' (seção 10.2/10.4) cai aqui de propósito: clonar um
            // Valor::Objeto via derive(Clone) clona apenas o Rc interno
            // (Rc::clone barato, mesmos dados compartilhados) — é
            // exatamente a semântica de referência que uma instância de
            // classe precisa ter, diferente da cópia profunda de
            // Registro/Conjunto.
            outro => outro.clone(),
        }
    }
}

/// Exibição textual de um valor para `escreva` (seção 6.2/6.2.2/6.2.3).
/// Formatação com largura/decimais (`:8:2`) é responsabilidade de quem
/// chama `escreva`, não desta função — aqui é só a representação "padrão".
impl fmt::Display for Valor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Valor::Inteiro(n) => write!(f, "{n}"),
            Valor::Real(n) => write!(f, "{n}"),
            Valor::Cadeia(s) => write!(f, "{s}"),
            Valor::Caractere(c) => write!(f, "{c}"),
            // ✅ seção 6.2.3: saída com pontos, maiúsculas — simétrico aos
            // literais de entrada '.Verdadeiro./.Falso.'.
            Valor::Logico(true) => write!(f, ".VERDADEIRO."),
            Valor::Logico(false) => write!(f, ".FALSO."),
            Valor::Registro(_) => write!(f, "<registro>"),
            Valor::Conjunto { .. } => write!(f, "<conjunto>"),
            Valor::Objeto { classe, .. } => write!(f, "<objeto:{classe}>"),
            Valor::ReferenciaFuncao { nome, .. } => write!(f, "<função:{nome}>"),
        }
    }
}

/// Uma "célula" de armazenamento — toda variável no [`Ambiente`] é uma
/// célula. `Rc` permite múltiplos donos (necessário para `ref`); `RefCell`
/// permite mutação através de um valor compartilhado.
pub type Celula = Rc<RefCell<Valor>>;

pub fn nova_celula(valor: Valor) -> Celula {
    Rc::new(RefCell::new(valor))
}

// =====================================================================================
// Ambiente (pilha de escopos)
// =====================================================================================

/// Pilha de escopos *case-insensitive* (seção 1.3) — mesma estrutura de
/// [`crate::checker::TabelaSimbolos`], mas associando nomes a [`Celula`]s
/// em vez de tipos. `Clone` é raso e barato (seção 9.6): cada [`Celula`]
/// é um `Rc`, então clonar um `Ambiente` clona só os mapas/contadores de
/// referência, nunca os `Valor`es por dentro — duas cópias do mesmo
/// `Ambiente` continuam compartilhando exatamente as mesmas células.
#[derive(Clone)]
pub struct Ambiente {
    escopos: Vec<HashMap<String, Celula>>,
}

impl Ambiente {
    pub fn novo() -> Self {
        Ambiente { escopos: vec![HashMap::new()] }
    }

    pub fn entrar_escopo(&mut self) {
        self.escopos.push(HashMap::new());
    }

    pub fn sair_escopo(&mut self) {
        self.escopos.pop();
        debug_assert!(!self.escopos.is_empty(), "o escopo global nunca deve ser removido");
    }

    /// Declara `nome` no escopo **atual**, associando-o a `celula`. Usado
    /// tanto para `var X : tipo` (nova célula com valor padrão) quanto para
    /// parâmetros (célula nova, cópia por valor, ou célula compartilhada do
    /// chamador, para `ref` — seção 9.3).
    pub fn declarar(&mut self, nome: &str, celula: Celula) {
        let chave = nome.to_lowercase();
        self.escopos.last_mut().expect("sempre há ao menos o escopo global").insert(chave, celula);
    }

    /// Busca a célula de `nome`, do escopo atual até o global
    /// (case-insensitive, lexical scoping — seção 9.6).
    pub fn buscar(&self, nome: &str) -> Option<Celula> {
        let chave = nome.to_lowercase();
        self.escopos.iter().rev().find_map(|e| e.get(&chave)).cloned()
    }
}

impl Default for Ambiente {
    fn default() -> Self {
        Self::novo()
    }
}

// =====================================================================================
// E/S de console — abstraída via trait (seção 6)
// =====================================================================================

/// Abstrai `leia`/`escreva`/`leia_seco`/`pausa`/CONIO, para permitir tanto
/// um console real (`peppe-cli`, stdin/stdout via `crossterm`) quanto um
/// console "falso" nos testes (entrada pré-programada, saída capturada em
/// um `String`).
///
/// `leia`/`leia_seco` retornam a linha **sem** o terminador de fim de linha
/// (o "consome a linha inteira, incluindo o Enter" da seção 6.1 já é
/// responsabilidade de quem implementa a trait).
pub trait ConsoleIO {
    fn escrever(&mut self, texto: &str);
    fn ler_linha(&mut self) -> String;
    /// Leitura sem eco (seção 6.3) — em um console real, desabilita o eco
    /// do terminal durante a leitura; em testes, comporta-se como
    /// [`Self::ler_linha`].
    fn ler_linha_sem_eco(&mut self) -> String {
        self.ler_linha()
    }
    /// `pausa` (seção 6.4) — por padrão, apenas descarta uma linha.
    fn pausar(&mut self) {
        self.ler_linha();
    }
    fn limpar(&mut self) {}
    fn limpar_linha(&mut self, _coluna: Option<i64>) {}
    fn posicionar(&mut self, _coluna: i64, _linha: i64) {}
    fn cor_fundo(&mut self, _cor: i64) {}
    fn cor_frente(&mut self, _cor: i64) {}
}

/// Console em memória para testes: lê de uma fila pré-programada de linhas
/// e acumula tudo que seria escrito em [`ConsoleMemoria::saida`].
#[derive(Debug, Default)]
pub struct ConsoleMemoria {
    pub entrada: std::collections::VecDeque<String>,
    pub saida: String,
}

impl ConsoleMemoria {
    pub fn com_entrada(linhas: &[&str]) -> Self {
        ConsoleMemoria {
            entrada: linhas.iter().map(|s| s.to_string()).collect(),
            saida: String::new(),
        }
    }
}

impl ConsoleIO for ConsoleMemoria {
    fn escrever(&mut self, texto: &str) {
        self.saida.push_str(texto);
    }
    fn ler_linha(&mut self) -> String {
        self.entrada.pop_front().unwrap_or_default()
    }
}

// =====================================================================================
// Erros de execução (seção 20.1 — todo erro de execução é fatal por ora)
// =====================================================================================

/// Um erro que só pode ser detectado em tempo de execução (o verificador
/// semântico, por ser estático, não tem como prevê-lo) — divisão por zero,
/// índice fora dos limites, recursão sem caso de parada (estouro de pilha,
/// detectado via contador, não via SO), etc.
///
/// **Decisão pendente (seção 20.1):** por ora, todo [`ErroExecucao`] é
/// fatal — interrompe a execução do programa. Introduzir recuperação
/// (`tente`/`captura`) é um refinamento futuro, não implementado aqui.
#[derive(Debug, Clone, PartialEq)]
pub struct ErroExecucao {
    pub linha: usize,
    pub mensagem: String,
}

impl fmt::Display for ErroExecucao {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Erro de execução, linha {}: {}", self.linha, self.mensagem)
    }
}

fn erro(linha: usize, mensagem: impl Into<String>) -> ErroExecucao {
    ErroExecucao { linha, mensagem: mensagem.into() }
}

// =====================================================================================
// Controle de fluxo dentro de um bloco
// =====================================================================================

/// Sinaliza, subindo pela pilha de chamadas de `executar_comando`/
/// `executar_bloco`, que um comando especial de controle de fluxo foi
/// executado e a execução normal do bloco/laço/sub-rotina atual deve parar.
enum FluxoControle {
    /// `interrompa` ou `saia_caso` com condição verdadeira — para o laço
    /// mais interno (seção 8).
    Interromper,
    /// `continue` — pula para a próxima iteração do laço mais interno.
    Continuar,
    /// `ir_para RÓTULO` (seção 8) — propaga subindo pela pilha de blocos
    /// até encontrar o bloco que efetivamente contém esse rótulo (em
    /// qualquer posição, antes ou depois do `ir_para` — seção 8 permite
    /// saltos "para a frente" e "para trás"), que então reinicia sua
    /// execução a partir dali. O checker já garante que o rótulo existe
    /// em algum bloco da sub-rotina atual (não cruza para outra
    /// sub-rotina), então este sinal nunca deveria escapar até
    /// `chamar_sub_rotina` sem ser capturado por algum bloco no caminho.
    SaltarPara(String),
}

/// `Ok(None)` = execução normal, completou o bloco.
/// `Ok(Some(fluxo))` = um comando de controle de fluxo interrompeu o bloco.
/// `Err(..)` = erro de execução fatal.
type ResultadoExecucao = Result<Option<FluxoControle>, ErroExecucao>;

// =====================================================================================
// Interpretador
// =====================================================================================

/// Executa `programa` usando `console` para toda E/S. Assume que `programa`
/// já passou pelo verificador semântico sem erros — não preventivamente
/// re-verifica tipos.
pub fn interpretar(programa: &Programa, console: &mut dyn ConsoleIO) -> Result<(), ErroExecucao> {
    let interp = Interpretador::novo(programa);
    let mut ambiente = Ambiente::novo();
    // Ao declarar as 'var'/'const' de topo em ordem, `declarar_topo`
    // também captura — como efeito colateral — o escopo de fechamento
    // (seção 9.6) de cada sub-rotina de nível de topo, exatamente no
    // ponto em que aparece no texto (ver documentação da função).
    interp.declarar_topo(&programa.declaracoes, &mut ambiente)?;
    if let Some(FluxoControle::SaltarPara(rotulo)) =
        interp.executar_bloco(&programa.bloco_principal, &mut ambiente, console)?
    {
        return Err(erro(
            0,
            format!(
                "'ir_para {rotulo}' não encontrou o rótulo em nenhum bloco \
                 alcançável a partir de onde foi executado (bug do checker, \
                 ou rótulo dentro de uma estrutura aninhada inacessível — \
                 ver nota em 'executar_bloco')"
            ),
        ));
    }
    Ok(())
}

/// Nome "de exibição" de um [`Tipo`] (AST, não resolvido) — suficiente
/// para comparar com [`Valor::nome_tipo`] na resolução de sobrecarga em
/// runtime (seção 10.5, [`Interpretador::resolver_sobrecarga_runtime`]).
/// Cobre só os casos que fazem sentido como tipo de parâmetro de uma
/// sub-rotina/método: primitivos e nomes de tipo (alias, registro,
/// classe — todos identificados pelo próprio nome declarado, igual ao
/// que `TipoResolvido::nome_exibicao` faria, mas sem precisar resolver
/// aliases — não é necessário aqui porque o checker já garantiu, antes
/// da execução, que a resolução de sobrecarga é determinística).
fn nome_tipo_declarado(tipo: &Tipo) -> String {
    match tipo {
        Tipo::Primitivo(TipoPrimitivo::Inteiro) => "inteiro".to_string(),
        Tipo::Primitivo(TipoPrimitivo::Real) => "real".to_string(),
        Tipo::Primitivo(TipoPrimitivo::Cadeia) => "cadeia".to_string(),
        Tipo::Primitivo(TipoPrimitivo::Caractere) => "caractere".to_string(),
        Tipo::Primitivo(TipoPrimitivo::Logico) => "lógico".to_string(),
        Tipo::Generico => "generico".to_string(),
        Tipo::Nomeado(nome) => nome.clone(),
        Tipo::Registro(_) => "registro".to_string(),
        Tipo::Conjunto { .. } => "conjunto".to_string(),
        Tipo::Classe { .. } => "classe".to_string(),
        Tipo::Funcao { .. } => "função".to_string(),
    }
}

/// Tabela de sub-rotinas indexada por nome em minúsculas (case-insensitive
/// — seção 1.3), montada uma vez a partir da AST do programa.
struct Interpretador<'p> {
    /// Uma ou mais sub-rotinas para o mesmo nome (seção 10.5 —
    /// sobrecarga ad-hoc): o caso comum é um vetor de tamanho 1; com
    /// sobrecarga, `avaliar_chamada`/`executar_comando` re-resolvem qual
    /// elemento usar a partir dos valores reais dos argumentos (ver
    /// [`Interpretador::resolver_sobrecarga_runtime`]) — o checker já
    /// validou, antes da execução, que essa resolução é sempre possível
    /// e nunca ambígua para qualquer programa que chegue até aqui.
    sub_rotinas: HashMap<String, Vec<&'p SubRotina>>,
    /// nome em minúsculas -> definição de tipo (mesma simplificação do
    /// checker — seção "coletar_tipos" de `checker.rs`).
    tabela_tipos: HashMap<String, &'p Tipo>,
    /// (classe em minúsculas, método em minúsculas) -> implementações —
    /// tanto `MetodoInterno` (corpo dentro de `classe ... fim_classe`)
    /// quanto [`DeclaracaoTopo::MetodoExterno`] caem aqui, indistintamente
    /// (seção 10.3). Vetor por causa de sobrecarga (seção 10.5), mesma
    /// observação de [`Self::sub_rotinas`]. Métodos não têm escopo de
    /// fechamento Pascal (seção 9.6) — seus únicos "globais" são os
    /// campos da própria instância (`este`), resolvidos separadamente em
    /// `Self::avaliar_metodo`, então aqui basta a referência direta.
    ///
    /// Nota sobre dispatch dinâmico (seção 10.6): `avaliar_metodo` busca
    /// a implementação a partir da classe **real** da instância (o
    /// campo `classe` dentro de `Valor::Objeto`), nunca do tipo
    /// declarado da variável no AST — então uma vez que o checker
    /// valida o uso correto de `virtual`/`sobrepor` (`Verificador::
    /// validar_overrides`), o dispatch dinâmico já decorre naturalmente
    /// desta tabela, sem precisar consultar o modificador em tempo de
    /// execução.
    metodos: HashMap<(String, String), Vec<&'p SubRotina>>,
    /// ESCOPO DE FECHAMENTO (seção 9.6, estilo Pascal) de cada
    /// sub-rotina — chave por identidade de ponteiro (duas sobrecargas
    /// nunca compartilham endereço). Cada entrada é um SNAPSHOT
    /// (clone raso — mesmas células, nunca recriadas) do ambiente
    /// exatamente no ponto em que `Self::declarar_topo` processou
    /// aquela `DeclaracaoTopo::SubRotina`: tudo que já tinha sido
    /// declarado antes dela (no mesmo bloco, recursivamente, incluindo
    /// qualquer fechamento herdado de um nível externo) já está no
    /// snapshot; nada declarado depois entra, porque ainda não existia
    /// no ambiente no momento da captura. Populado uma única vez por
    /// sub-rotina, na primeira (e única) vez que `declarar_topo`
    /// processa as declarações daquele bloco — para sub-rotinas de
    /// nível de topo, isso acontece em `interpretar`; para aninhadas,
    /// a cada chamada da sub-rotina externa que as contém (apropriado,
    /// já que o conteúdo do fechamento pode mudar entre chamadas, ex.:
    /// uma variável local da externa com valor diferente a cada
    /// invocação). `RefCell` porque todos os métodos de execução são
    /// `&self` (não `&mut self`).
    fechamentos: RefCell<HashMap<*const SubRotina, Ambiente>>,
}

impl<'p> Interpretador<'p> {
    fn novo(programa: &'p Programa) -> Self {
        let mut interp = Interpretador {
            sub_rotinas: HashMap::new(),
            tabela_tipos: HashMap::new(),
            metodos: HashMap::new(),
            fechamentos: RefCell::new(HashMap::new()),
        };
        interp.coletar_sub_rotinas_e_tipos(&programa.declaracoes);
        interp
    }

    fn coletar_sub_rotinas_e_tipos(&mut self, declaracoes: &'p [DeclaracaoTopo]) {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::SubRotina(s) => {
                    self.sub_rotinas.entry(s.nome.to_lowercase()).or_default().push(s);
                    self.coletar_sub_rotinas_e_tipos(&s.declaracoes_locais);
                }
                DeclaracaoTopo::Tipo(t) => {
                    self.tabela_tipos.insert(t.nome.to_lowercase(), &t.definicao);
                    if let Tipo::Classe { membros, .. } = &t.definicao {
                        for membro in membros {
                            if let ItemClasse::MetodoInterno(sub, _modificador) = &membro.item {
                                self.metodos
                                    .entry((t.nome.to_lowercase(), sub.nome.to_lowercase()))
                                    .or_default()
                                    .push(sub);
                            }
                        }
                    }
                }
                DeclaracaoTopo::MetodoExterno { classe, metodo } => {
                    self.metodos
                        .entry((classe.to_lowercase(), metodo.nome.to_lowercase()))
                        .or_default()
                        .push(metodo);
                }
                DeclaracaoTopo::Const(_) | DeclaracaoTopo::Var(_) => {}
            }
        }
    }

    /// Escolhe, entre `candidatas` (sobrecargas de um mesmo nome, seção
    /// 10.5), a que deve executar para uma chamada com `argumentos` (já
    /// avaliados para `Valor`). O checker já garantiu, antes da
    /// execução, que existe exatamente uma candidata viável para
    /// qualquer programa sem erros semânticos — então aqui não há
    /// tratamento de ambiguidade ou "nenhuma candidata aceita": esses
    /// casos já teriam impedido a execução. Resolve por aridade exata
    /// e, dentre as de aridade certa, pelo nome de tipo (via
    /// [`Valor::nome_tipo`]) de cada argumento na mesma posição —
    /// suficiente porque o checker já não deixaria passar uma chamada
    /// onde isso fosse ambíguo.
    fn resolver_sobrecarga_runtime<'s>(
        &self,
        candidatas: &'s [&'p SubRotina],
        argumentos: &[Valor],
    ) -> &'s &'p SubRotina {
        if candidatas.len() == 1 {
            return &candidatas[0];
        }
        let aridade_chamada = argumentos.len();
        let mesma_aridade: Vec<&'s &'p SubRotina> = candidatas
            .iter()
            .filter(|s| s.parametros.iter().map(|p| p.nomes.len()).sum::<usize>() == aridade_chamada)
            .collect();
        for candidata in mesma_aridade.iter().copied() {
            let parametros_expandidos: Vec<&Parametro> = candidata
                .parametros
                .iter()
                .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
                .collect();
            let bate = argumentos.iter().zip(parametros_expandidos.iter()).all(|(valor, param)| {
                self.nome_tipo_declarado_resolvido(&param.tipo) == valor.nome_tipo()
            });
            if bate {
                return candidata;
            }
        }
        // Não deveria acontecer (o checker já validou a chamada) — mas
        // por segurança, evita pânico devolvendo a primeira de aridade
        // certa, ou a primeira de todas se nem isso bater.
        mesma_aridade.first().copied().unwrap_or(&candidatas[0])
    }

    /// Como [`nome_tipo_declarado`], mas resolve **um nível** de alias
    /// (`tipo Idade = inteiro`) usando [`Self::tabela_tipos`] antes de
    /// comparar — suficiente para o caso comum (sobrecarga usando um
    /// alias direto de primitivo); não segue cadeias de alias mais
    /// longas (`tipo A = B; tipo B = inteiro`), que ficam como limitação
    /// conhecida desta resolução em runtime.
    fn nome_tipo_declarado_resolvido(&self, tipo: &Tipo) -> String {
        if let Tipo::Nomeado(nome) = tipo {
            if let Some(definicao) = self.tabela_tipos.get(&nome.to_lowercase()) {
                if !matches!(definicao, Tipo::Classe { .. }) {
                    return nome_tipo_declarado(definicao);
                }
            }
        }
        nome_tipo_declarado(tipo)
    }


    /// Constrói o valor padrão de uma instância da classe `nome_classe`
    /// (seção 10.1/10.2/10.4, Fase 6 — múltiplas bases diretas): percorre
    /// toda a árvore de herança a partir de `nome_classe` (cada classe
    /// pode ter mais de uma base direta) coletando os campos de cada
    /// nível — uma instância é "plana", contém todos os campos
    /// achatados, próprios e herdados, em um único `HashMap` por nome —
    /// e constrói cada campo no seu valor padrão. Métodos não entram
    /// aqui — são resolvidos por nome a partir da `tabela_tipos`/info de
    /// classe no momento da chamada (`avaliar_metodo`), não armazenados
    /// na instância.
    ///
    /// ⚠️ **Limitação conhecida (Fase 6):** como a instância usa um
    /// único `HashMap<String, Celula>` indexado por nome de campo, não
    /// há como representar duas células distintas para o mesmo nome
    /// vindo de duas bases diferentes (diamond problem sem herança
    /// virtual, ou colisão de nome entre bases independentes) — o
    /// checker já rejeita qualquer acesso ambíguo a esse nome sem
    /// qualificação antes da execução, então um programa que chega até
    /// aqui só deveria ter colisões reais quando as duas bases
    /// representam, de fato, a *mesma* base comum mais acima (mesmo
    /// valor em ambos os caminhos) — qualquer outro caso de colisão de
    /// nome é uma limitação a revisar se aparecer no material do livro.
    fn valor_padrao_classe(&self, nome_classe: &str, ambiente: &Ambiente) -> Result<Valor, ErroExecucao> {
        // Monta a lista de níveis de membros, da(s) base(s) mais
        // distante(s) até 'nome_classe', percorrendo a árvore via busca
        // em profundidade — não há colisão de nome esperada entre
        // níveis (o checker já validou isso), então a ordem de inserção
        // no HashMap final não afeta a correção, é só uma convenção de
        // leitura (igual à versão anterior, de herança simples).
        let mut cadeia: Vec<Vec<MembroClasse>> = Vec::new();
        self.coletar_membros_com_heranca(nome_classe, &mut cadeia, &mut Vec::new())?;
        cadeia.reverse(); // da base mais distante até a própria classe

        let mut mapa = HashMap::new();
        for membros in &cadeia {
            for membro in membros {
                if let ItemClasse::Campo(decl_var) = &membro.item {
                    for nome_campo in &decl_var.nomes {
                        let v = self.valor_padrao(&decl_var.tipo, ambiente)?;
                        mapa.insert(nome_campo.clone(), nova_celula(v));
                    }
                }
            }
        }

        Ok(Valor::Objeto {
            classe: nome_classe.to_string(),
            campos: Rc::new(RefCell::new(mapa)),
        })
    }

    /// Percorre a árvore de herança a partir de `nome_classe` (Fase 6),
    /// empilhando em `cadeia` os membros de cada classe visitada — usado
    /// por [`Self::valor_padrao_classe`]. `caminho` evita recursão
    /// infinita em caso de ciclo de herança (não deveria acontecer — o
    /// checker valida isso —, mas por segurança); diferente de
    /// `Verificador::resolver_em_bases_rec`, aqui não há necessidade de
    /// detectar ambiguidade (o checker já fez isso antes da execução).
    fn coletar_membros_com_heranca(
        &self,
        nome_classe: &str,
        cadeia: &mut Vec<Vec<MembroClasse>>,
        caminho: &mut Vec<String>,
    ) -> Result<(), ErroExecucao> {
        let atual = nome_classe.to_lowercase();
        if caminho.contains(&atual) {
            return Ok(()); // ciclo de herança — não deveria acontecer
        }
        let Some(Tipo::Classe { heranca, membros }) = self.tabela_tipos.get(&atual) else {
            return Err(erro(
                0,
                format!("classe '{nome_classe}' não encontrada ao construir valor padrão (bug interno)"),
            ));
        };
        cadeia.push(membros.clone());
        caminho.push(atual);
        for base in heranca {
            self.coletar_membros_com_heranca(base, cadeia, caminho)?;
        }
        caminho.pop();
        Ok(())
    }

    /// Busca as sobrecargas de `nome_metodo` em `nome_classe` ou em
    /// qualquer base (direta ou indireta, Fase 6 — múltiplas bases
    /// diretas), parando no primeiro nível da árvore onde o nome
    /// existir em **qualquer** caminho — o checker já validou, antes da
    /// execução, que não há ambiguidade não-qualificada para nenhuma
    /// chamada que chegue até aqui, então (diferente do equivalente no
    /// checker, `Verificador::buscar_metodo_com_heranca`) esta busca em
    /// runtime não precisa replicar a detecção de ambiguidade — usa a
    /// primeira correspondência que achar.
    fn buscar_candidatos_metodo<'s>(
        &'s self,
        nome_classe: &str,
        nome_metodo: &str,
        caminho: &mut Vec<String>,
    ) -> Option<&'s Vec<&'p SubRotina>> {
        let atual = nome_classe.to_lowercase();
        if caminho.contains(&atual) {
            return None; // ciclo de herança — não deveria acontecer
        }
        if let Some(c) = self.metodos.get(&(atual.clone(), nome_metodo.to_lowercase())) {
            return Some(c);
        }
        caminho.push(atual.clone());
        let resultado = match self.tabela_tipos.get(&atual) {
            Some(Tipo::Classe { heranca, .. }) => {
                heranca.iter().find_map(|base| self.buscar_candidatos_metodo(base, nome_metodo, caminho))
            }
            _ => None,
        };
        caminho.pop();
        resultado
    }


    /// Resolve `tipo` (possivelmente [`Tipo::Nomeado`]) até chegar a uma
    /// forma "concreta" o suficiente para construir um [`Valor`] padrão —
    /// não precisa achatar completamente como `tipos::resolver_tipo` (o
    /// checker já validou que não há ciclos nem nomes inexistentes).
    fn resolver_tipo_raso<'a>(&self, tipo: &'a Tipo) -> &'a Tipo
    where
        'p: 'a,
    {
        match tipo {
            Tipo::Nomeado(nome) => {
                let chave = nome.to_lowercase();
                match self.tabela_tipos.get(&chave) {
                    Some(def) => self.resolver_tipo_raso(def),
                    None => tipo, // não deveria ocorrer (checker já validou)
                }
            }
            outro => outro,
        }
    }

    /// Constrói o valor padrão de `tipo` (seção 4: variáveis recém
    /// declaradas começam com um valor "zero" do seu tipo) — `0`/`0.0`/
    /// `""`/`' '`/`.falso.`, `registro` com cada campo no seu valor padrão,
    /// `conjunto` com cada dimensão estática já alocada (dimensões
    /// dinâmicas, seção 4.5.1, começam vazias até `dimensione`), instância
    /// de `classe` com todos os campos (próprios e herdados, seção
    /// 10.1/10.2) em seus valores padrão.
    fn valor_padrao(&self, tipo: &Tipo, ambiente: &Ambiente) -> Result<Valor, ErroExecucao> {
        // 'classe' é tratada antes de 'resolver_tipo_raso' (que não
        // preserva o nome pelo qual a classe foi declarada) — só
        // chegamos aqui vindo de 'Tipo::Nomeado("Aluno")', onde 'nome' É
        // o nome da classe.
        if let Tipo::Nomeado(nome) = tipo {
            let chave = nome.to_lowercase();
            if let Some(Tipo::Classe { .. }) = self.tabela_tipos.get(&chave) {
                return self.valor_padrao_classe(nome, ambiente);
            }
        }
        match self.resolver_tipo_raso(tipo) {
            Tipo::Primitivo(TipoPrimitivo::Inteiro) => Ok(Valor::Inteiro(0)),
            Tipo::Primitivo(TipoPrimitivo::Real) => Ok(Valor::Real(0.0)),
            Tipo::Primitivo(TipoPrimitivo::Cadeia) => Ok(Valor::Cadeia(String::new())),
            Tipo::Primitivo(TipoPrimitivo::Caractere) => Ok(Valor::Caractere(' ')),
            Tipo::Primitivo(TipoPrimitivo::Logico) => Ok(Valor::Logico(false)),
            Tipo::Generico => Ok(Valor::Inteiro(0)), // aproximação v1, ver tipos::TipoResolvido::Generico
            Tipo::Registro(campos) => {
                let mut mapa = HashMap::new();
                for campo in campos {
                    for nome in &campo.nomes {
                        let v = self.valor_padrao(&campo.tipo, ambiente)?;
                        mapa.insert(nome.clone(), nova_celula(v));
                    }
                }
                Ok(Valor::Registro(mapa))
            }
            Tipo::Conjunto { dimensoes, elemento } => {
                let mut limites = Vec::new();
                for dim in dimensoes {
                    match dim {
                        Some((ini, fim)) => {
                            let ini = self.avaliar_expr_const(ini, ambiente)?;
                            let fim = self.avaliar_expr_const(fim, ambiente)?;
                            limites.push((ini, fim));
                        }
                        // Dimensão dinâmica ainda não dimensionada: tamanho
                        // 0 até o 'dimensione' (seção 4.5.1).
                        None => limites.push((1, 0)),
                    }
                }
                let elemento_padrao = self.valor_padrao(elemento, ambiente)?;
                let total: i64 = limites
                    .iter()
                    .map(|(ini, fim)| (fim - ini + 1).max(0))
                    .product();
                let mut elementos = Vec::with_capacity(total.max(0) as usize);
                for _ in 0..total.max(0) {
                    elementos.push(nova_celula(elemento_padrao.clonar_por_valor()));
                }
                Ok(Valor::Conjunto {
                    dimensoes: limites,
                    elementos,
                    elemento_padrao: Box::new(elemento_padrao),
                })
            }
            // Tipo::Nomeado não deveria sobreviver a resolver_tipo_raso,
            // exceto no caso defensivo de nome inexistente (não deveria
            // ocorrer pós-checker).
            Tipo::Nomeado(nome) => Err(erro(
                0,
                format!("tipo '{nome}' não encontrado ao construir valor padrão (bug interno)"),
            )),
            Tipo::Funcao { .. } => {
                // Valor padrão antes de qualquer atribuição real (seção
                // 10.5.3) — nome vazio/sem receptor é uma sentinela que
                // nunca deveria ser CHAMADA sem antes passar por uma
                // atribuição (`RESPOSTA <- SOMATORIO`); se isso
                // acontecer, é capturado como erro de execução no ponto
                // da chamada indireta (ver Self::avaliar_chamada), não
                // aqui na construção.
                Ok(Valor::ReferenciaFuncao { nome: String::new(), receptor: None })
            }
            // 'Tipo::Classe' só deveria ser alcançado através de
            // 'Tipo::Nomeado' (interceptado no início desta função, antes
            // do match) — chegar aqui diretamente seria um 'classe ...
            // fim_classe' usado inline como tipo de campo/parâmetro, o
            // que a gramática não permite (só existe via 'tipo NOME =
            // classe ...' e referência por nome). Bug interno se ocorrer.
            Tipo::Classe { .. } => Err(erro(
                0,
                "tipo 'classe' não pode ser usado diretamente (bug interno — deveria \
                 ter sido interceptado via Tipo::Nomeado)"
                    .to_string(),
            )),
        }
    }

    /// Avalia uma expressão que deve ser constante neste ponto da execução
    /// (limites de `conjunto` estático, seção 4.5) e a converte para
    /// `i64`. Usado apenas na construção de valores padrão.
    fn avaliar_expr_const(&self, expr: &Expr, ambiente: &Ambiente) -> Result<i64, ErroExecucao> {
        match self.avaliar_expr_sem_console(expr, ambiente)? {
            Valor::Inteiro(n) => Ok(n),
            outro => Err(erro(
                0,
                format!(
                    "limite de dimensão deveria ser 'inteiro', mas é '{}'",
                    outro.nome_tipo()
                ),
            )),
        }
    }

    /// Variante de [`Self::avaliar_expr`] para contextos sem E/S possível
    /// (limites de `conjunto`/`dimensione`) — qualquer expressão que
    /// envolva `leia`/chamada de função com efeito colateral aqui seria
    /// incomum; usamos um console "mudo" que apenas descarta.
    fn avaliar_expr_sem_console(
        &self,
        expr: &Expr,
        ambiente: &Ambiente,
    ) -> Result<Valor, ErroExecucao> {
        let mut mudo = ConsoleMemoria::default();
        self.avaliar_expr(expr, ambiente, &mut mudo)
    }

    // =================================================================================
    // Declarações de nível superior (seção 4/9) — aloca células no ambiente
    // =================================================================================

    /// Declara `const`/`var` no `ambiente` atual (sub-rotinas já foram
    /// coletadas em [`Self::coletar_sub_rotinas_e_tipos`] e não precisam de
    /// célula — são chamadas pela AST, não por valor em uma variável).
    /// Declara `const`/`var` de `declaracoes` em `ambiente`, na ordem em
    /// que aparecem. Ao encontrar uma `DeclaracaoTopo::SubRotina`,
    /// CAPTURA o ESCOPO DE FECHAMENTO dela (seção 9.6, estilo Pascal):
    /// um snapshot (clone raso de `ambiente` — mesmas células, nunca
    /// recriadas) exatamente como está nesse instante, guardado em
    /// `self.fechamentos` por identidade de ponteiro da sub-rotina. Como
    /// o processamento é sequencial e cada `var`/`const` só é declarada
    /// quando alcançada, o snapshot contém exatamente o que foi
    /// declarado ANTES da sub-rotina no texto — nunca o que vem depois.
    /// Aninhamento pleno decorre naturalmente: ao processar
    /// `sub.declaracoes_locais` (chamado de dentro de
    /// `Self::chamar_sub_rotina`, com `ambiente` já igual ao fechamento
    /// herdado de fora mais os parâmetros/locais da própria chamada), o
    /// mesmo mecanismo se aplica recursivamente — uma sub-rotina
    /// aninhada herda automaticamente tudo que já era visível no nível
    /// externo, sem precisar re-percorrer nada além de `declaracoes`.
    fn declarar_topo(
        &self,
        declaracoes: &[DeclaracaoTopo],
        ambiente: &mut Ambiente,
    ) -> Result<(), ErroExecucao> {
        for decl in declaracoes {
            match decl {
                DeclaracaoTopo::Const(c) => {
                    let v = self.avaliar_expr_sem_console(&c.valor, ambiente)?;
                    ambiente.declarar(&c.nome, nova_celula(v));
                }
                DeclaracaoTopo::Var(v) => {
                    for nome in &v.nomes {
                        let valor = self.valor_padrao(&v.tipo, ambiente)?;
                        ambiente.declarar(nome, nova_celula(valor));
                    }
                }
                DeclaracaoTopo::SubRotina(s) => {
                    self.fechamentos.borrow_mut().insert(s as *const SubRotina, ambiente.clone());
                }
                DeclaracaoTopo::Tipo(_) => {}
                DeclaracaoTopo::MetodoExterno { .. } => {}
            }
        }
        Ok(())
    }

    // =================================================================================
    // L-values: resolve NOME/NOME.CAMPO/NOME[i] até a célula final
    // =================================================================================

    /// Resolve `lvalue` até a [`Celula`] que efetivamente guarda o valor —
    /// usada tanto para leitura (`avaliar_expr` sobre `Expr::Variavel`)
    /// quanto para escrita (`Comando::Atribuicao`, `leia`).
    fn resolver_celula(
        &self,
        lvalue: &LValue,
        ambiente: &Ambiente,
    ) -> Result<Celula, ErroExecucao> {
        // Constantes pré-definidas (seção 5.6) — não entram no ambiente
        // de execução, mas são reconhecidas aqui antes de reportar erro.
        let mut celula = match ambiente.buscar(&lvalue.nome) {
            Some(c) => c,
            None => {
                let nome_lower = lvalue.nome.to_lowercase();
                match nome_lower.as_str() {
                    "p_pi"       => nova_celula(Valor::Real(std::f64::consts::PI)),
                    "p_euler"    => nova_celula(Valor::Real(std::f64::consts::E)),
                    "p_infinito" => nova_celula(Valor::Real(f64::INFINITY)),
                    _ => return Err(erro(
                        lvalue.linha,
                        format!("'{}' não foi declarado (bug do checker?)", lvalue.nome),
                    )),
                }
            }
        };

        for acesso in &lvalue.acessos {
            celula = match acesso {
                Acesso::Campo(nome_campo) => {
                    let valor = celula.borrow();
                    match &*valor {
                        Valor::Registro(campos) => {
                            let achou = campos.iter().find(|(k, _)| k.eq_ignore_ascii_case(nome_campo));
                            match achou {
                                Some((_, c)) => c.clone(),
                                None => {
                                    return Err(erro(
                                        lvalue.linha,
                                        format!(
                                            "campo '{nome_campo}' não existe (bug do checker?)"
                                        ),
                                    ))
                                }
                            }
                        }
                        // O HashMap de uma instância de classe já contém
                        // TODOS os campos achatados, incluindo os
                        // herdados (seção 10.1/10.4) — 'valor_padrao'
                        // monta a instância assim, então não há lógica
                        // de herança a percorrer aqui em tempo de
                        // execução, só uma busca direta por nome (igual
                        // a 'registro').
                        Valor::Objeto { campos, .. } => {
                            let mapa = campos.borrow();
                            let achou = mapa.iter().find(|(k, _)| k.eq_ignore_ascii_case(nome_campo));
                            match achou {
                                Some((_, c)) => c.clone(),
                                None => {
                                    return Err(erro(
                                        lvalue.linha,
                                        format!(
                                            "campo '{nome_campo}' não existe (bug do checker?)"
                                        ),
                                    ))
                                }
                            }
                        }
                        outro => {
                            return Err(erro(
                                lvalue.linha,
                                format!(
                                    "não é possível acessar campo em '{}' (bug do checker?)",
                                    outro.nome_tipo()
                                ),
                            ))
                        }
                    }
                }
                Acesso::Metodo { .. } => {
                    // Uma chamada de método nunca produz uma "célula"
                    // encadeável (seção 10.4) — não é um lugar de
                    // memória, é a execução de um corpo de sub-rotina
                    // que retorna um valor solto. Quem precisa do
                    // resultado de uma chamada de método usa
                    // 'avaliar_metodo' diretamente (via 'Expr::Variavel'
                    // ou 'Comando::ChamadaMetodo'), não
                    // 'resolver_celula' — chegar aqui é um bug do
                    // checker (deveria ter rejeitado '.MÉTODO()' como
                    // destino de atribuição ou no meio de uma cadeia).
                    return Err(erro(
                        lvalue.linha,
                        "não é possível tratar uma chamada de método como um lugar de \
                         memória (bug do checker?)"
                            .to_string(),
                    ));
                }
                Acesso::Indice(indices_expr) => {
                    let mut indices = Vec::with_capacity(indices_expr.len());
                    for idx_expr in indices_expr {
                        let mut mudo = ConsoleMemoria::default();
                        let v = self.avaliar_expr(idx_expr, ambiente, &mut mudo)?;
                        match v {
                            Valor::Inteiro(n) => indices.push(n),
                            outro => {
                                return Err(erro(
                                    lvalue.linha,
                                    format!(
                                        "índice deveria ser 'inteiro', mas é '{}'",
                                        outro.nome_tipo()
                                    ),
                                ))
                            }
                        }
                    }
                    let valor = celula.borrow();
                    match &*valor {
                        Valor::Conjunto { dimensoes, elementos, .. } => {
                            let posicao = indice_linear(dimensoes, &indices, lvalue.linha)?;
                            match elementos.get(posicao) {
                                Some(c) => c.clone(),
                                None => {
                                    return Err(erro(
                                        lvalue.linha,
                                        format!(
                                            "índice fora dos limites em '{}' (seção 15)",
                                            lvalue.nome
                                        ),
                                    ))
                                }
                            }
                        }
                        outro => {
                            return Err(erro(
                                lvalue.linha,
                                format!(
                                    "não é possível indexar '{}' (bug do checker?)",
                                    outro.nome_tipo()
                                ),
                            ))
                        }
                    }
                }
            };
        }

        Ok(celula)
    }

    // =================================================================================
    // Avaliação de expressões (seção 5)
    // =================================================================================

    /// Avalia `expr` no contexto de uma atribuição cujo destino é de
    /// tipo `função` (seção 10.5.3) — intercepta os dois casos especiais
    /// de "referência sem chamar" (sub-rotina solta, ou `OBJETO.MÉTODO`
    /// sem parênteses) construindo um [`Valor::ReferenciaFuncao`]; em
    /// qualquer outro caso (ex.: copiar uma variável de tipo função já
    /// existente), delega para [`Self::avaliar_expr`] sem alteração. O
    /// checker já validou que a referência é inequívoca (não
    /// sobrecarregada) e compatível — aqui não há mais verificação a
    /// fazer, só construir o valor.
    fn avaliar_expr_como_referencia_funcao(
        &self,
        expr: &Expr,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Valor, ErroExecucao> {
        let Expr::Variavel(lvalue) = expr else {
            return self.avaliar_expr(expr, ambiente, console);
        };

        // Caso 1: sub-rotina solta — 'SOMATORIO' sozinho, sem acessos,
        // e o nome de fato existe como sub-rotina (não variável).
        if lvalue.acessos.is_empty() && lvalue.qualificador_base.is_none() {
            if self.sub_rotinas.contains_key(&lvalue.nome.to_lowercase()) {
                return Ok(Valor::ReferenciaFuncao { nome: lvalue.nome.clone(), receptor: None });
            }
            return self.avaliar_expr(expr, ambiente, console);
        }

        // Caso 2: método de instância — 'OBJETO.MÉTODO' (exatamente um
        // acesso, Acesso::Campo — não chamada). Resolve a célula de
        // OBJETO, confirma que é uma instância de classe, e que
        // 'MÉTODO' de fato existe como método nela (considerando
        // herança, Fase 6) — senão, é só um campo comum, caminho usual.
        if let [Acesso::Campo(nome_membro)] = lvalue.acessos.as_slice() {
            let lvalue_objeto = LValue {
                qualificador_base: lvalue.qualificador_base.clone(),
                nome: lvalue.nome.clone(),
                acessos: vec![],
                linha: lvalue.linha,
            };
            let celula_objeto = self.resolver_celula(&lvalue_objeto, ambiente)?;
            let valor_objeto = celula_objeto.borrow().clone();
            if let Valor::Objeto { classe, .. } = &valor_objeto {
                let classe_origem = lvalue.qualificador_base.as_deref().unwrap_or(classe.as_str());
                if self.buscar_candidatos_metodo(classe_origem, nome_membro, &mut Vec::new()).is_some() {
                    return Ok(Valor::ReferenciaFuncao {
                        nome: nome_membro.clone(),
                        receptor: Some(Box::new(valor_objeto)),
                    });
                }
            }
        }

        self.avaliar_expr(expr, ambiente, console)
    }

    fn avaliar_expr(
        &self,
        expr: &Expr,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Valor, ErroExecucao> {
        match expr {
            Expr::Inteiro(n) => Ok(Valor::Inteiro(*n)),
            Expr::Real(n) => Ok(Valor::Real(*n)),
            Expr::Texto(s) => Ok(Valor::Cadeia(s.clone())),
            Expr::Caractere(c) => Ok(Valor::Caractere(*c)),
            Expr::Logico(b) => Ok(Valor::Logico(*b)),

            Expr::Variavel(lvalue) => {
                // Caso especial (seção 10.4): se o ÚLTIMO acesso da cadeia
                // é uma chamada de método, não há "célula" a resolver
                // (uma chamada de método não é um lugar de memória) — em
                // vez disso, resolve a célula do RECEPTOR (tudo antes do
                // último acesso) e executa o método sobre ela. O checker
                // já garante que isso só aparece em contexto de
                // expressão quando o método tem retorno (caso contrário,
                // teria sido rejeitado como "procedimento usado como
                // valor").
                if let Some(Acesso::Metodo { nome: nome_metodo, argumentos }) = lvalue.acessos.last() {
                    let receptor = LValue {
                        qualificador_base: lvalue.qualificador_base.clone(),
                        nome: lvalue.nome.clone(),
                        acessos: lvalue.acessos[..lvalue.acessos.len() - 1].to_vec(),
                        linha: lvalue.linha,
                    };
                    let celula_objeto = self.resolver_celula(&receptor, ambiente)?;
                    // O qualificador de escopo (Fase 6) só vale para o
                    // PRIMEIRO acesso da cadeia (mesma regra do checker,
                    // 'Verificador::tipo_de_lvalue') — só se aplica
                    // aqui se a chamada de método for, ela mesma, o
                    // único acesso (índice 0 = último, já que o vetor
                    // tem tamanho 1).
                    let qualificador_para_este_acesso = if lvalue.acessos.len() == 1 {
                        lvalue.qualificador_base.as_deref()
                    } else {
                        None
                    };
                    let resultado = self.avaliar_metodo(
                        &celula_objeto,
                        qualificador_para_este_acesso,
                        nome_metodo,
                        argumentos,
                        lvalue.linha,
                        ambiente,
                        console,
                    )?;
                    return resultado.ok_or_else(|| {
                        erro(
                            lvalue.linha,
                            format!(
                                "'{nome_metodo}' é um procedimento e não pode ser usado \
                                 em uma expressão (bug do checker?)"
                            ),
                        )
                    });
                }
                let celula = self.resolver_celula(lvalue, ambiente)?;
                let valor = celula.borrow().clonar_por_valor();
                Ok(valor)
            }

            Expr::Chamada { nome, argumentos, linha } => {
                // Constantes pré-definidas usadas com parênteses vazios
                // (defensivo — normalmente chegam como Expr::Variavel).
                self.avaliar_chamada(nome, argumentos, *linha, ambiente, console)
            }

            Expr::Binaria { op, esquerda, direita, linha } => {
                let ve = self.avaliar_expr(esquerda, ambiente, console)?;
                let vd = self.avaliar_expr(direita, ambiente, console)?;
                avaliar_operador_binario(*op, ve, vd, *linha)
            }

            Expr::Unaria { op, expr, linha } => {
                let v = self.avaliar_expr(expr, ambiente, console)?;
                avaliar_operador_unario(*op, v, *linha)
            }

            Expr::Cast { tipo, expr, linha } => {
                let v = self.avaliar_expr(expr, ambiente, console)?;
                converter_cast(*tipo, v, *linha)
            }
        }
    }

    /// `NOME(arg1, arg2, ...)` como expressão (função) — também usado por
    /// [`Self::executar_comando`] para `ChamadaProcedimento`, via
    /// [`Self::chamar_sub_rotina`] diretamente (procedimentos não têm
    /// valor de retorno a propagar).
    fn avaliar_chamada(
        &self,
        nome: &str,
        argumentos: &[Expr],
        linha: usize,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Valor, ErroExecucao> {
        // Chamada INDIRETA através de uma variável de tipo função
        // (seção 10.5.3) — 'RESPOSTA(args)' onde 'RESPOSTA' guarda uma
        // referência, não o nome de uma sub-rotina declarada. Despacha
        // para Self::chamar_referencia_funcao, que por sua vez delega
        // para chamar_sub_rotina (sub-rotina solta) ou avaliar_metodo
        // (método — preservando dispatch dinâmico via a instância
        // capturada no momento da atribuição).
        if let Some(celula) = ambiente.buscar(nome) {
            let referencia = celula.borrow().clone();
            if let Valor::ReferenciaFuncao { nome: nome_ref, receptor } = referencia {
                return self.chamar_referencia_funcao(
                    &nome_ref,
                    receptor.as_deref(),
                    argumentos,
                    linha,
                    ambiente,
                    console,
                );
            }
        }

        if let Some(v) = avaliar_predefinida(nome, argumentos, linha, self, ambiente, console)? {
            return Ok(v);
        }

        match self.chamar_sub_rotina(nome, argumentos, linha, ambiente, console)? {
            Some(v) => Ok(v),
            None => Err(erro(
                linha,
                format!("'{nome}' é um procedimento e não pode ser usado em uma expressão"),
            )),
        }
    }

    /// Invoca a função/método referenciado por um [`Valor::ReferenciaFuncao`]
    /// (seção 10.5.3) — `receptor = None` despacha para
    /// [`Self::chamar_sub_rotina`] (sub-rotina solta); `receptor =
    /// Some(objeto)` despacha para [`Self::avaliar_metodo`] sobre uma
    /// célula construída a partir do objeto capturado, preservando
    /// dispatch dinâmico (seção 10.6) exatamente como uma chamada
    /// direta `OBJETO.MÉTODO(...)` — `avaliar_metodo` sempre resolve
    /// pela classe real guardada no `Valor::Objeto`, nunca pelo tipo
    /// declarado de quem capturou a referência.
    fn chamar_referencia_funcao(
        &self,
        nome: &str,
        receptor: Option<&Valor>,
        argumentos: &[Expr],
        linha: usize,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Valor, ErroExecucao> {
        match receptor {
            None => match self.chamar_sub_rotina(nome, argumentos, linha, ambiente, console)? {
                Some(v) => Ok(v),
                None => Err(erro(
                    linha,
                    format!("'{nome}' é um procedimento e não pode ser usado em uma expressão"),
                )),
            },
            Some(objeto) => {
                let celula_objeto = nova_celula(objeto.clone());
                match self.avaliar_metodo(&celula_objeto, None, nome, argumentos, linha, ambiente, console)? {
                    Some(v) => Ok(v),
                    None => Err(erro(
                        linha,
                        format!("'{nome}' é um procedimento e não pode ser usado em uma expressão"),
                    )),
                }
            }
        }
    }

    /// Executa a sub-rotina `nome` com `argumentos` já avaliados no
    /// `ambiente` do chamador. Retorna `Some(valor)` para `função` (o valor
    /// final da célula que representa o nome da função dentro do seu
    /// próprio escopo, seção 9.2) ou `None` para `procedimento`.
    fn chamar_sub_rotina(
        &self,
        nome: &str,
        argumentos: &[Expr],
        linha: usize,
        ambiente_chamador: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Option<Valor>, ErroExecucao> {
        let candidatos = self.sub_rotinas.get(&nome.to_lowercase()).ok_or_else(|| {
            erro(linha, format!("'{nome}' não foi declarado (bug do checker?)"))
        })?;

        // Avalia cada argumento no escopo do CHAMADOR antes de entrar no
        // escopo da sub-rotina (parâmetros não veem o próprio escopo da
        // sub-rotina sendo chamada). Avalia **por valor primeiro**, só
        // para resolver qual sobrecarga usar (seção 10.5) — seguro mesmo
        // para um argumento cujo parâmetro real seja 'ref', porque o
        // checker já garante que esse argumento é sempre uma
        // 'Expr::Variavel' (ler uma variável não tem efeito colateral a
        // duplicar).
        let mut valores_para_resolver: Vec<Valor> = Vec::with_capacity(argumentos.len());
        for arg_expr in argumentos {
            valores_para_resolver.push(self.avaliar_expr(arg_expr, ambiente_chamador, console)?);
        }
        let sub: &'p SubRotina = *self.resolver_sobrecarga_runtime(candidatos, &valores_para_resolver);

        let parametros_expandidos: Vec<&Parametro> = sub
            .parametros
            .iter()
            .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
            .collect();
        let nomes_expandidos: Vec<&str> = sub
            .parametros
            .iter()
            .flat_map(|p| p.nomes.iter().map(|n| n.as_str()))
            .collect();

        let mut celulas_argumentos: Vec<Celula> = Vec::with_capacity(argumentos.len());
        for (i, arg_expr) in argumentos.iter().enumerate() {
            let por_referencia = parametros_expandidos.get(i).map(|p| p.por_referencia).unwrap_or(false);
            if por_referencia {
                // 'ref': o argumento DEVE ser um lvalue — compartilha a
                // célula com o chamador (seção 9.3).
                let Expr::Variavel(lvalue) = arg_expr else {
                    return Err(erro(
                        linha,
                        format!(
                            "o argumento {} de '{nome}' é 'ref' e exige uma variável, \
                             não uma expressão (bug do checker?)",
                            i + 1
                        ),
                    ));
                };
                celulas_argumentos.push(self.resolver_celula(lvalue, ambiente_chamador)?);
            } else if let Expr::Variavel(lvalue) = arg_expr {
                // Se o argumento é uma variável de tipo Conjunto, usa a
                // célula diretamente (sem clonar_por_valor), para que
                // mutações dentro da sub-rotina sejam visíveis no chamador
                // (passagem por referência implícita de conjuntos, como em
                // Lua e outras linguagens — seção 9.3).
                let celula_original = self.resolver_celula(lvalue, ambiente_chamador)?;
                let e_conjunto = matches!(&*celula_original.borrow(), Valor::Conjunto { .. });
                if e_conjunto {
                    celulas_argumentos.push(celula_original);
                } else {
                    let v = valores_para_resolver[i].clone();
                    celulas_argumentos.push(nova_celula(v.clonar_por_valor()));
                }
            } else {
                // Já avaliado acima (para resolver a sobrecarga) — reusa
                // o valor em vez de avaliar a expressão de novo, o que
                // duplicaria efeitos colaterais (ex.: uma chamada de
                // função usada como argumento).
                let v = valores_para_resolver[i].clone();
                celulas_argumentos.push(nova_celula(v.clonar_por_valor()));
            }
        }

        // Monta o ambiente da sub-rotina: parte do ESCOPO DE FECHAMENTO
        // (seção 9.6, estilo Pascal) já capturado por
        // `Self::declarar_topo` em `self.fechamentos` — um snapshot
        // (clone raso, mesmas células) de tudo que era visível no ponto
        // exato em que 'sub' foi declarada. Sem entrada (sub-rotina sem
        // nenhuma var/const antes dela em seu bloco, caso comum): vazio
        // é o fechamento correto, não um erro. Empilha um escopo NOVO
        // por cima, só para os parâmetros desta chamada.
        let mut ambiente_sub = self
            .fechamentos
            .borrow()
            .get(&(sub as *const SubRotina))
            .cloned()
            .unwrap_or_else(Ambiente::novo);
        ambiente_sub.entrar_escopo();
        for (nome_param, celula) in nomes_expandidos.iter().zip(celulas_argumentos.into_iter()) {
            ambiente_sub.declarar(nome_param, celula);
        }

        let mut tipo_retorno_celula = None;
        if sub.categoria == CategoriaSubRotina::Funcao {
            let tipo_retorno = sub
                .tipo_retorno
                .as_ref()
                .expect("função sempre tem tipo_retorno (garantido pelo parser)");
            let v0 = self.valor_padrao(tipo_retorno, &ambiente_sub)?;
            let c = nova_celula(v0);
            ambiente_sub.declarar(&sub.nome, c.clone());
            tipo_retorno_celula = Some(c);
        }

        self.declarar_topo(&sub.declaracoes_locais, &mut ambiente_sub)?;
        if let Some(FluxoControle::SaltarPara(rotulo)) =
            self.executar_bloco(&sub.corpo, &mut ambiente_sub, console)?
        {
            return Err(erro(
                sub.linha,
                format!(
                    "'ir_para {rotulo}' não encontrou o rótulo em nenhum bloco \
                     alcançável dentro de '{}' (bug do checker, ou rótulo dentro \
                     de uma estrutura aninhada inacessível — ver nota em \
                     'executar_bloco')",
                    sub.nome
                ),
            ));
        }

        Ok(tipo_retorno_celula.map(|c| c.borrow().clonar_por_valor()))
    }

    /// Executa o método `nome_metodo` (encontrado subindo a cadeia de
    /// herança e resolvendo sobrecarga, seção 10.1/10.4/10.5 — ver
    /// [`Self::resolver_sobrecarga_runtime`]) sobre a instância guardada
    /// em `celula_objeto`, com `argumentos` avaliados no ambiente do
    /// chamador. Retorna `Some(valor)` para método-função, `None` para
    /// método-procedimento — mesma convenção de [`Self::chamar_sub_rotina`].
    ///
    /// Diferente de uma sub-rotina solta: o ambiente do método começa com
    /// `este` (a própria instância) e **as mesmas células** de cada campo
    /// do objeto já declaradas diretamente por nome (seção 10.3) — não
    /// cópias. Mutar um campo dentro do método (`MÉDIA ← SOMA / 4`)
    /// escreve na célula que vive dentro do `Valor::Objeto`, visível para
    /// qualquer outra referência à mesma instância depois da chamada
    /// retornar (mesma semântica de referência do `Valor::Objeto` em si).
    fn avaliar_metodo(
        &self,
        celula_objeto: &Celula,
        qualificador_base: Option<&str>,
        nome_metodo: &str,
        argumentos: &[Expr],
        linha: usize,
        ambiente_chamador: &Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> Result<Option<Valor>, ErroExecucao> {
        let (nome_classe, campos_objeto) = match &*celula_objeto.borrow() {
            Valor::Objeto { classe, campos } => (classe.clone(), campos.clone()),
            outro => {
                return Err(erro(
                    linha,
                    format!(
                        "não é possível chamar método em '{}' — não é uma instância de \
                         classe (bug do checker?)",
                        outro.nome_tipo()
                    ),
                ))
            }
        };

        // Qualificador de escopo (Fase 6 — seção 10.1/10.6.1): se
        // presente, a busca começa diretamente na classe-base indicada
        // em vez da classe real da instância — o checker já validou
        // que essa base é de fato uma ancestral, e que a busca a partir
        // dela não é ambígua (ou rejeitou o programa antes de chegar
        // aqui).
        let classe_partida = qualificador_base.unwrap_or(nome_classe.as_str());
        let candidatos = self
            .buscar_candidatos_metodo(classe_partida, nome_metodo, &mut Vec::new())
            .ok_or_else(|| {
                erro(
                    linha,
                    format!(
                        "a classe '{classe_partida}' não tem método '{nome_metodo}' \
                         (bug do checker?)"
                    ),
                )
            })?;

        // Avalia todos os argumentos **por valor** primeiro, só para
        // saber seus tipos e resolver qual sobrecarga usar (seção
        // 10.5) — seguro mesmo para um argumento cujo parâmetro real
        // seja 'ref', porque o checker já garante que esse argumento é
        // sempre uma 'Expr::Variavel' (ler uma variável não tem efeito
        // colateral a duplicar).
        let mut valores_para_resolver: Vec<Valor> = Vec::with_capacity(argumentos.len());
        for arg_expr in argumentos {
            valores_para_resolver.push(self.avaliar_expr(arg_expr, ambiente_chamador, console)?);
        }
        let sub = *self.resolver_sobrecarga_runtime(candidatos, &valores_para_resolver);

        let parametros_expandidos: Vec<&Parametro> = sub
            .parametros
            .iter()
            .flat_map(|p| std::iter::repeat(p).take(p.nomes.len()))
            .collect();
        let nomes_expandidos: Vec<&str> = sub
            .parametros
            .iter()
            .flat_map(|p| p.nomes.iter().map(|n| n.as_str()))
            .collect();

        let mut celulas_argumentos: Vec<Celula> = Vec::with_capacity(argumentos.len());
        for (i, arg_expr) in argumentos.iter().enumerate() {
            let por_referencia = parametros_expandidos.get(i).map(|p| p.por_referencia).unwrap_or(false);
            if por_referencia {
                let Expr::Variavel(lvalue) = arg_expr else {
                    return Err(erro(
                        linha,
                        format!(
                            "o argumento {} de '{nome_metodo}' é 'ref' e exige uma \
                             variável, não uma expressão (bug do checker?)",
                            i + 1
                        ),
                    ));
                };
                celulas_argumentos.push(self.resolver_celula(lvalue, ambiente_chamador)?);
            } else if let Expr::Variavel(lvalue) = arg_expr {
                // Conjunto: usa a célula diretamente (referência implícita).
                let celula_original = self.resolver_celula(lvalue, ambiente_chamador)?;
                let e_conjunto = matches!(&*celula_original.borrow(), Valor::Conjunto { .. });
                if e_conjunto {
                    celulas_argumentos.push(celula_original);
                } else {
                    let v = valores_para_resolver[i].clone();
                    celulas_argumentos.push(nova_celula(v.clonar_por_valor()));
                }
            } else {
                let v = valores_para_resolver[i].clone();
                celulas_argumentos.push(nova_celula(v.clonar_por_valor()));
            }
        }
        let mut ambiente_sub = Ambiente::novo();
        // 'este' (seção 10.3/10.4): a própria instância, mesma célula que
        // o chamador usa — permite 'este.CAMPO' funcionar como qualquer
        // outro acesso a campo de objeto.
        ambiente_sub.declarar("este", celula_objeto.clone());
        // Campos do objeto diretamente por nome, mesmas células de dentro
        // do HashMap compartilhado — ver doc da função.
        for (nome_campo, celula_campo) in campos_objeto.borrow().iter() {
            ambiente_sub.declarar(nome_campo, celula_campo.clone());
        }
        // Parâmetros DEPOIS dos campos, no mesmo escopo: como
        // 'Ambiente::declarar' permite redeclarar (sobrescrevendo) no
        // mesmo escopo (diferente da tabela de símbolos do checker, que
        // usa dois escopos aninhados para permitir shadowing sem erro de
        // "já declarado") — um parâmetro com nome igual a um campo
        // simplesmente substitui a entrada do campo no mapa do ambiente,
        // dando o mesmo resultado de shadowing que o checker já validou
        // como correto.
        for (nome_param, celula) in nomes_expandidos.iter().zip(celulas_argumentos.into_iter()) {
            ambiente_sub.declarar(nome_param, celula);
        }

        let mut tipo_retorno_celula = None;
        if sub.categoria == CategoriaSubRotina::Funcao {
            let tipo_retorno = sub
                .tipo_retorno
                .as_ref()
                .expect("função sempre tem tipo_retorno (garantido pelo parser)");
            let v0 = self.valor_padrao(tipo_retorno, &ambiente_sub)?;
            let c = nova_celula(v0);
            ambiente_sub.declarar(&sub.nome, c.clone());
            tipo_retorno_celula = Some(c);
        }

        self.declarar_topo(&sub.declaracoes_locais, &mut ambiente_sub)?;
        if let Some(FluxoControle::SaltarPara(rotulo)) =
            self.executar_bloco(&sub.corpo, &mut ambiente_sub, console)?
        {
            return Err(erro(
                sub.linha,
                format!(
                    "'ir_para {rotulo}' não encontrou o rótulo em nenhum bloco \
                     alcançável dentro do método '{}' (bug do checker?)",
                    sub.nome
                ),
            ));
        }

        Ok(tipo_retorno_celula.map(|c| c.borrow().clonar_por_valor()))
    }


    /// Executa `bloco` do início ao fim, e trata `ir_para` (seção 8): se
    /// um comando filho sinaliza [`FluxoControle::SaltarPara`] e o rótulo
    /// referenciado está em `bloco` (em qualquer posição), a execução
    /// "salta" reiniciando a partir do índice desse rótulo — para a frente
    /// ou para trás, ambos suportados. Se o rótulo não está em `bloco`, o
    /// sinal é repassado ao chamador (até encontrar o bloco que o contém —
    /// o checker garante que existe algum, dentro da mesma sub-rotina).
    ///
    /// **Limitação conhecida:** a busca por `posicao_do_rotulo` olha apenas
    /// os comandos **diretos** de `bloco`, não dentro de `se`/laços/`caso`
    /// aninhados. Ou seja, `ir_para` consegue saltar para um rótulo no
    /// mesmo nível em que está (ou em um bloco mais externo, propagando
    /// para cima), mas não para um rótulo "descendo" dentro de uma
    /// estrutura condicional/laço aninhada a partir de fora dela — uso que
    /// não aparece no material de origem (rótulos lá sempre marcam um
    /// ponto no fluxo sequencial principal, seção 8). Caso isso se mostre
    /// necessário, o checker precisaria também rastrear o caminho exato
    /// (não só a existência) de cada rótulo para validar — refinamento
    /// futuro, não implementado aqui.
    ///
    /// Limite de segurança: nenhum aqui além do natural — um `ir_para` que
    /// nunca termina é um loop infinito intencional (ou bug) do programa
    /// do aluno, que deve rodar indefinidamente como em qualquer outra
    /// linguagem; não há nenhuma rede de segurança artificial.
    fn executar_bloco(
        &self,
        bloco: &Bloco,
        ambiente: &mut Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> ResultadoExecucao {
        let mut indice = 0;
        while indice < bloco.len() {
            match self.executar_comando(&bloco[indice], ambiente, console)? {
                None => indice += 1,
                Some(FluxoControle::SaltarPara(rotulo)) => {
                    match posicao_do_rotulo(bloco, &rotulo) {
                        Some(novo_indice) => indice = novo_indice,
                        None => return Ok(Some(FluxoControle::SaltarPara(rotulo))),
                    }
                }
                Some(fluxo) => return Ok(Some(fluxo)),
            }
        }
        Ok(None)
    }

    fn executar_comando(
        &self,
        comando: &Comando,
        ambiente: &mut Ambiente,
        console: &mut dyn ConsoleIO,
    ) -> ResultadoExecucao {
        match comando {
            Comando::Atribuicao { destino, valor, .. } => {
                let celula = self.resolver_celula(destino, ambiente)?;
                // Se o destino já é uma referência a função (seção
                // 10.5.3) — toda 'var X : FUNC1' começa como
                // Valor::ReferenciaFuncao via valor_padrao — avalia o
                // lado direito no modo contextual (intercepta nome de
                // sub-rotina solta / OBJETO.MÉTODO sem chamar). Em
                // qualquer outro caso, comportamento normal.
                let eh_destino_funcao = matches!(&*celula.borrow(), Valor::ReferenciaFuncao { .. });
                let v = if eh_destino_funcao {
                    self.avaliar_expr_como_referencia_funcao(valor, ambiente, console)?
                } else {
                    self.avaliar_expr(valor, ambiente, console)?
                };
                *celula.borrow_mut() = v.clonar_por_valor();
                Ok(None)
            }

            Comando::Leia { variaveis, linha } => {
                for v in variaveis {
                    let celula = self.resolver_celula(v, ambiente)?;
                    let linha_lida = console.ler_linha();
                    let novo_valor = converter_entrada(&celula.borrow(), &linha_lida, *linha)?;
                    *celula.borrow_mut() = novo_valor;
                }
                Ok(None)
            }

            Comando::LeiaSeco { variavel, linha } => {
                let celula = self.resolver_celula(variavel, ambiente)?;
                let linha_lida = console.ler_linha_sem_eco();
                let novo_valor = converter_entrada(&celula.borrow(), &linha_lida, *linha)?;
                *celula.borrow_mut() = novo_valor;
                Ok(None)
            }

            Comando::Escreva { itens, quebra_linha, .. } => {
                for item in itens {
                    let v = self.avaliar_expr(&item.expressao, ambiente, console)?;
                    let largura = match &item.largura {
                        Some(e) => Some(self.avaliar_expr(e, ambiente, console)?),
                        None => None,
                    };
                    let decimais = match &item.decimais {
                        Some(e) => Some(self.avaliar_expr(e, ambiente, console)?),
                        None => None,
                    };
                    let texto = formatar_item_escreva(&v, largura.as_ref(), decimais.as_ref());
                    console.escrever(&texto);
                }
                if *quebra_linha {
                    console.escrever("\n");
                }
                Ok(None)
            }

            Comando::Se { condicao, entao, senao, linha }
            | Comando::ExcetoSe { condicao, entao, senao, linha } => {
                let mut cond = self.avaliar_logico(condicao, ambiente, console, *linha)?;
                if matches!(comando, Comando::ExcetoSe { .. }) {
                    cond = !cond;
                }
                if cond {
                    self.executar_bloco(entao, ambiente, console)
                } else if let Some(senao) = senao {
                    self.executar_bloco(senao, ambiente, console)
                } else {
                    Ok(None)
                }
            }

            Comando::Caso { expressao, ramos, senao, .. } => {
                let valor = self.avaliar_expr(expressao, ambiente, console)?;
                for ramo in ramos {
                    let valor_ramo = self.avaliar_expr(&ramo.valor, ambiente, console)?;
                    if valores_iguais(&valor, &valor_ramo) {
                        return self.executar_bloco(&ramo.corpo, ambiente, console);
                    }
                }
                match senao {
                    Some(senao) => self.executar_bloco(senao, ambiente, console),
                    None => Ok(None),
                }
            }

            Comando::Enquanto { condicao, corpo, linha } => {
                while self.avaliar_logico(condicao, ambiente, console, *linha)? {
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => continue,
                        _ => {}
                    }
                }
                Ok(None)
            }

            Comando::AteSeja { condicao, corpo, linha } => {
                while !self.avaliar_logico(condicao, ambiente, console, *linha)? {
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => continue,
                        _ => {}
                    }
                }
                Ok(None)
            }

            Comando::Repita { corpo, condicao, linha } => {
                loop {
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => {}  // cai na verificação da condição
                        _ => {}
                    }
                    if self.avaliar_logico(condicao, ambiente, console, *linha)? {
                        break;
                    }
                }
                Ok(None)
            }

            Comando::Execute { corpo, condicao, linha } => {
                loop {
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => {}  // cai na verificação da condição
                        _ => {}
                    }
                    if !self.avaliar_logico(condicao, ambiente, console, *linha)? {
                        break;
                    }
                }
                Ok(None)
            }

            Comando::Laco { corpo, .. } => {
                loop {
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => continue,
                        _ => {}
                    }
                }
                Ok(None)
            }

            Comando::Para { variavel, inicio, fim, passo, corpo, linha } => {
                let v_inicio = self.avaliar_numerico(inicio, ambiente, console, *linha)?;
                let v_fim = self.avaliar_numerico(fim, ambiente, console, *linha)?;
                let v_passo = match passo {
                    Some(p) => self.avaliar_numerico(p, ambiente, console, *linha)?,
                    None => 1.0,
                };
                if v_passo == 0.0 {
                    return Err(erro(*linha, "o 'passo' do 'para' não pode ser zero"));
                }

                let celula_controle = match ambiente.buscar(variavel) {
                    Some(c) => c,
                    None => {
                        return Err(erro(
                            *linha,
                            format!("'{variavel}' não foi declarado (bug do checker?)"),
                        ))
                    }
                };

                let usa_inteiro = matches!(&*celula_controle.borrow(), Valor::Inteiro(_));
                let mut atual = v_inicio;
                loop {
                    let continuar = if v_passo > 0.0 { atual <= v_fim } else { atual >= v_fim };
                    if !continuar {
                        break;
                    }
                    *celula_controle.borrow_mut() = if usa_inteiro {
                        Valor::Inteiro(atual as i64)
                    } else {
                        Valor::Real(atual)
                    };
                    match self.executar_bloco(corpo, ambiente, console)? {
                        Some(FluxoControle::Interromper) => break,
                        Some(FluxoControle::Continuar) => {}  // cai no incremento abaixo
                        _ => {}
                    }
                    atual += v_passo;
                }
                Ok(None)
            }

            Comando::Dimensione { variavel, dimensoes, linha } => {
                let celula = match ambiente.buscar(variavel) {
                    Some(c) => c,
                    None => {
                        return Err(erro(
                            *linha,
                            format!("'{variavel}' não foi declarado (bug do checker?)"),
                        ))
                    }
                };

                // O "molde" do elemento viaja com o Valor::Conjunto desde
                // sua criação (valor_padrao) — funciona corretamente
                // mesmo quando o array está vazio (dimensão dinâmica nunca
                // dimensionada antes), sem depender de inspecionar
                // elementos existentes nem de assumir 'inteiro' como
                // fallback.
                let elemento_modelo = match &*celula.borrow() {
                    Valor::Conjunto { elemento_padrao, .. } => elemento_padrao.clonar_por_valor(),
                    outro => {
                        return Err(erro(
                            *linha,
                            format!(
                                "'{variavel}' não é um 'conjunto' (é '{}', bug do checker?)",
                                outro.nome_tipo()
                            ),
                        ))
                    }
                };

                let mut limites = Vec::with_capacity(dimensoes.len());
                for (ini_expr, fim_expr) in dimensoes {
                    let ini = self.avaliar_numerico(ini_expr, ambiente, console, *linha)? as i64;
                    let fim = self.avaliar_numerico(fim_expr, ambiente, console, *linha)? as i64;
                    limites.push((ini, fim));
                }
                let total: i64 = limites.iter().map(|(i, f)| (f - i + 1).max(0)).product();
                let mut elementos = Vec::with_capacity(total.max(0) as usize);
                for _ in 0..total.max(0) {
                    elementos.push(nova_celula(elemento_modelo.clonar_por_valor()));
                }
                *celula.borrow_mut() = Valor::Conjunto {
                    dimensoes: limites,
                    elementos,
                    elemento_padrao: Box::new(elemento_modelo),
                };
                Ok(None)
            }

            Comando::ChamadaProcedimento { nome, argumentos, linha } => {
                // 'RESPOSTA()' como comando solto (seção 10.5.3) — se
                // 'nome' for uma variável de referência a função,
                // descarta o retorno (mesma permissividade de
                // 'OBJETO.MÉTODO()' como comando, seção 10.4) em vez de
                // tentar achar uma sub-rotina chamada 'RESPOSTA'.
                if let Some(celula) = ambiente.buscar(nome) {
                    let referencia = celula.borrow().clone();
                    if let Valor::ReferenciaFuncao { nome: nome_ref, receptor } = referencia {
                        self.chamar_referencia_funcao(
                            &nome_ref,
                            receptor.as_deref(),
                            argumentos,
                            *linha,
                            ambiente,
                            console,
                        )?;
                        return Ok(None);
                    }
                }
                self.chamar_sub_rotina(nome, argumentos, *linha, ambiente, console)?;
                Ok(None)
            }

            Comando::ChamadaMetodo { alvo, linha } => {
                // 'OBJETO.MÉTODO()' como comando solto (seção 10.4):
                // ignora o valor de retorno, se houver (válido tanto
                // para 'procedimento' quanto para 'função' usada apenas
                // pelo efeito colateral). O parser garante que o ÚLTIMO
                // acesso de 'alvo' é sempre 'Acesso::Metodo'.
                let Some(Acesso::Metodo { nome: nome_metodo, argumentos }) = alvo.acessos.last() else {
                    return Err(erro(
                        *linha,
                        "comando de chamada de método malformado (bug interno do parser)"
                            .to_string(),
                    ));
                };
                let receptor = LValue {
                    qualificador_base: alvo.qualificador_base.clone(),
                    nome: alvo.nome.clone(),
                    acessos: alvo.acessos[..alvo.acessos.len() - 1].to_vec(),
                    linha: alvo.linha,
                };
                let celula_objeto = self.resolver_celula(&receptor, ambiente)?;
                let qualificador_para_este_acesso = if alvo.acessos.len() == 1 {
                    alvo.qualificador_base.as_deref()
                } else {
                    None
                };
                self.avaliar_metodo(
                    &celula_objeto,
                    qualificador_para_este_acesso,
                    nome_metodo,
                    argumentos,
                    *linha,
                    ambiente,
                    console,
                )?;
                Ok(None)
            }

            Comando::Rotulo { .. } => Ok(None),

            Comando::IrPara { rotulo, .. } => Ok(Some(FluxoControle::SaltarPara(rotulo.clone()))),

            Comando::Interrompa { .. } => Ok(Some(FluxoControle::Interromper)),
            Comando::Continue { .. } => Ok(Some(FluxoControle::Continuar)),

            Comando::SaiaCaso { condicao, linha } => {
                if self.avaliar_logico(condicao, ambiente, console, *linha)? {
                    Ok(Some(FluxoControle::Interromper))
                } else {
                    Ok(None)
                }
            }

            Comando::Limpar { .. } => {
                console.limpar();
                Ok(None)
            }
            Comando::LimparLinha { coluna, linha } => {
                let c = match coluna {
                    Some(e) => Some(self.avaliar_numerico(e, ambiente, console, *linha)? as i64),
                    None => None,
                };
                console.limpar_linha(c);
                Ok(None)
            }
            Comando::Posicionar { coluna, linha_destino, linha } => {
                let c = self.avaliar_numerico(coluna, ambiente, console, *linha)? as i64;
                let l = self.avaliar_numerico(linha_destino, ambiente, console, *linha)? as i64;
                console.posicionar(c, l);
                Ok(None)
            }
            Comando::CorFundo { cor, linha } => {
                let c = self.avaliar_numerico(cor, ambiente, console, *linha)? as i64;
                console.cor_fundo(c);
                Ok(None)
            }
            Comando::CorFrente { cor, linha } => {
                let c = self.avaliar_numerico(cor, ambiente, console, *linha)? as i64;
                console.cor_frente(c);
                Ok(None)
            }
            Comando::Pausa { .. } => {
                console.pausar();
                Ok(None)
            }
        }
    }

    /// Avalia `expr` e garante que o resultado é `lógico`, convertendo o
    /// erro de tipo (que não deveria ocorrer pós-checker) em
    /// [`ErroExecucao`] em vez de pânico.
    fn avaliar_logico(
        &self,
        expr: &Expr,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
        linha: usize,
    ) -> Result<bool, ErroExecucao> {
        match self.avaliar_expr(expr, ambiente, console)? {
            Valor::Logico(b) => Ok(b),
            outro => {
                Err(erro(linha, format!("esperava 'lógico', encontrei '{}' (bug do checker?)", outro.nome_tipo())))
            }
        }
    }

    /// Avalia `expr` e converte para `f64` (aceita `inteiro` ou `real`) —
    /// usado em `para`/`dimensione`/CONIO, que aceitam ambos.
    fn avaliar_numerico(
        &self,
        expr: &Expr,
        ambiente: &Ambiente,
        console: &mut dyn ConsoleIO,
        linha: usize,
    ) -> Result<f64, ErroExecucao> {
        match self.avaliar_expr(expr, ambiente, console)? {
            Valor::Inteiro(n) => Ok(n as f64),
            Valor::Real(n) => Ok(n),
            outro => Err(erro(
                linha,
                format!("esperava número, encontrei '{}' (bug do checker?)", outro.nome_tipo()),
            )),
        }
    }
}

/// Procura `Comando::Rotulo { nome, .. }` em `bloco` cujo nome (case-
/// insensitive — seção 1.3) seja igual a `rotulo`, e retorna seu índice
/// dentro do `Vec<Comando>`. Usado por [`Interpretador::executar_bloco`]
/// para implementar `ir_para` (seção 8).
fn posicao_do_rotulo(bloco: &Bloco, rotulo: &str) -> Option<usize> {
    bloco.iter().position(|c| matches!(c, Comando::Rotulo { nome, .. } if nome.eq_ignore_ascii_case(rotulo)))
}

/// Converte um índice multidimensional (`[i, j, ...]`) em uma posição
/// linear dentro do `Vec<Celula>` achatado de um `Valor::Conjunto`
/// (*row-major* — a última dimensão varia mais rápido).
fn indice_linear(
    dimensoes: &[(i64, i64)],
    indices: &[i64],
    linha: usize,
) -> Result<usize, ErroExecucao> {
    if dimensoes.len() != indices.len() {
        return Err(erro(
            linha,
            format!(
                "esperava {} índice(s), recebi {} (bug do checker?)",
                dimensoes.len(),
                indices.len()
            ),
        ));
    }
    let mut posicao: i64 = 0;
    for ((ini, fim), idx) in dimensoes.iter().zip(indices.iter()) {
        if idx < ini || idx > fim {
            return Err(erro(
                linha,
                format!(
                    "índice {idx} fora dos limites [{ini}..{fim}] (seção 15 — erro de execução)"
                ),
            ));
        }
        let tamanho_dim = fim - ini + 1;
        posicao = posicao * tamanho_dim + (idx - ini);
    }
    Ok(posicao as usize)
}

// =====================================================================================
// Operadores (seção 5) e *casts* (seção 10.5.1)
// =====================================================================================

fn avaliar_operador_binario(
    op: OpBinario,
    esquerda: Valor,
    direita: Valor,
    linha: usize,
) -> Result<Valor, ErroExecucao> {
    use OpBinario::*;
    use Valor::*;

    // Promove inteiro/real para uma base comum quando necessário.
    let como_f64 = |v: &Valor| -> Option<f64> {
        match v {
            Inteiro(n) => Some(*n as f64),
            Real(n) => Some(*n),
            _ => None,
        }
    };

    match op {
        Soma => match (&esquerda, &direita) {
            (Inteiro(a), Inteiro(b)) => Ok(Inteiro(a + b)),
            (Cadeia(a), Cadeia(b)) => Ok(Cadeia(format!("{a}{b}"))),
            (Cadeia(a), Caractere(b)) => Ok(Cadeia(format!("{a}{b}"))),
            (Caractere(a), Cadeia(b)) => Ok(Cadeia(format!("{a}{b}"))),
            (Caractere(a), Caractere(b)) => Ok(Cadeia(format!("{a}{b}"))),
            _ => match (como_f64(&esquerda), como_f64(&direita)) {
                (Some(a), Some(b)) => Ok(Real(a + b)),
                _ => Err(erro_operador_runtime("+", &esquerda, &direita, linha)),
            },
        },
        Subtracao => binario_numerico(esquerda, direita, linha, "-", |a, b| a - b),
        Multiplicacao => binario_numerico(esquerda, direita, linha, "*", |a, b| a * b),
        Divisao => match (como_f64(&esquerda), como_f64(&direita)) {
            (Some(_), Some(b)) if b == 0.0 => {
                Err(erro(linha, "divisão por zero (seção 15 — erro de execução)"))
            }
            (Some(a), Some(b)) => Ok(Real(a / b)),
            _ => Err(erro_operador_runtime("/", &esquerda, &direita, linha)),
        },
        Div => match (&esquerda, &direita) {
            (Inteiro(_), Inteiro(0)) => {
                Err(erro(linha, "divisão por zero ('div', seção 15 — erro de execução)"))
            }
            (Inteiro(a), Inteiro(b)) => Ok(Inteiro(a.div_euclid(*b))),
            _ => Err(erro_operador_runtime("div", &esquerda, &direita, linha)),
        },
        Mod => match (&esquerda, &direita) {
            (Inteiro(_), Inteiro(0)) => {
                Err(erro(linha, "divisão por zero ('mod', seção 15 — erro de execução)"))
            }
            (Inteiro(a), Inteiro(b)) => Ok(Inteiro(a.rem_euclid(*b))),
            _ => Err(erro_operador_runtime("mod", &esquerda, &direita, linha)),
        },
        Potencia => match (como_f64(&esquerda), como_f64(&direita)) {
            (Some(a), Some(b)) => Ok(Real(a.powf(b))),
            _ => Err(erro_operador_runtime("^", &esquerda, &direita, linha)),
        },

        Igual => Ok(Logico(valores_iguais(&esquerda, &direita))),
        Diferente => Ok(Logico(!valores_iguais(&esquerda, &direita))),
        Menor | Maior | MenorIgual | MaiorIgual => {
            comparar_relacional(op, esquerda, direita, linha)
        }

        E => binario_logico(esquerda, direita, linha, "e", |a, b| a && b),
        Ou => binario_logico(esquerda, direita, linha, "ou", |a, b| a || b),
        Xou => binario_logico(esquerda, direita, linha, "xou", |a, b| a ^ b),
    }
}

fn binario_numerico(
    esquerda: Valor,
    direita: Valor,
    linha: usize,
    simbolo: &str,
    op: fn(f64, f64) -> f64,
) -> Result<Valor, ErroExecucao> {
    match (&esquerda, &direita) {
        (Valor::Inteiro(a), Valor::Inteiro(b)) => {
            Ok(Valor::Inteiro(op(*a as f64, *b as f64) as i64))
        }
        _ => {
            let como_f64 = |v: &Valor| match v {
                Valor::Inteiro(n) => Some(*n as f64),
                Valor::Real(n) => Some(*n),
                _ => None,
            };
            match (como_f64(&esquerda), como_f64(&direita)) {
                (Some(a), Some(b)) => Ok(Valor::Real(op(a, b))),
                _ => Err(erro_operador_runtime(simbolo, &esquerda, &direita, linha)),
            }
        }
    }
}

fn binario_logico(
    esquerda: Valor,
    direita: Valor,
    linha: usize,
    simbolo: &str,
    op: fn(bool, bool) -> bool,
) -> Result<Valor, ErroExecucao> {
    match (&esquerda, &direita) {
        (Valor::Logico(a), Valor::Logico(b)) => Ok(Valor::Logico(op(*a, *b))),
        _ => Err(erro_operador_runtime(simbolo, &esquerda, &direita, linha)),
    }
}

fn comparar_relacional(
    op: OpBinario,
    esquerda: Valor,
    direita: Valor,
    linha: usize,
) -> Result<Valor, ErroExecucao> {
    use std::cmp::Ordering;
    let ord = ordem_parcial(&esquerda, &direita).ok_or_else(|| {
        erro_operador_runtime(crate::tipos::simbolo_op_binario(op), &esquerda, &direita, linha)
    })?;
    let resultado = match op {
        OpBinario::Menor => ord == Ordering::Less,
        OpBinario::Maior => ord == Ordering::Greater,
        OpBinario::MenorIgual => ord != Ordering::Greater,
        OpBinario::MaiorIgual => ord != Ordering::Less,
        _ => unreachable!("comparar_relacional só é chamada para <, >, <=, >="),
    };
    Ok(Valor::Logico(resultado))
}

fn ordem_parcial(a: &Valor, b: &Valor) -> Option<std::cmp::Ordering> {
    use Valor::*;
    match (a, b) {
        (Inteiro(x), Inteiro(y)) => x.partial_cmp(y),
        (Real(x), Real(y)) => x.partial_cmp(y),
        (Inteiro(x), Real(y)) => (*x as f64).partial_cmp(y),
        (Real(x), Inteiro(y)) => x.partial_cmp(&(*y as f64)),
        (Cadeia(x), Cadeia(y)) => x.partial_cmp(y),
        (Caractere(x), Caractere(y)) => x.partial_cmp(y),
        (Cadeia(x), Caractere(y)) => x.as_str().partial_cmp(y.to_string().as_str()),
        (Caractere(x), Cadeia(y)) => x.to_string().as_str().partial_cmp(y.as_str()),
        _ => None,
    }
}

/// Igualdade estrutural usada por `=`, `<>` e `caso`/`seja` (seção 7.3) —
/// compara por valor, incluindo coerção numérica básica (`1 = 1.0`).
fn valores_iguais(a: &Valor, b: &Valor) -> bool {
    use Valor::*;
    match (a, b) {
        (Inteiro(x), Inteiro(y)) => x == y,
        (Real(x), Real(y)) => x == y,
        (Inteiro(x), Real(y)) | (Real(y), Inteiro(x)) => (*x as f64) == *y,
        (Cadeia(x), Cadeia(y)) => x == y,
        (Caractere(x), Caractere(y)) => x == y,
        (Cadeia(x), Caractere(y)) | (Caractere(y), Cadeia(x)) => x.as_str() == y.to_string(),
        (Logico(x), Logico(y)) => x == y,
        _ => false,
    }
}

fn erro_operador_runtime(simbolo: &str, esquerda: &Valor, direita: &Valor, linha: usize) -> ErroExecucao {
    erro(
        linha,
        format!(
            "operador '{simbolo}' não pôde ser aplicado a '{}' e '{}' em tempo de execução \
             (bug do checker?)",
            esquerda.nome_tipo(),
            direita.nome_tipo()
        ),
    )
}

fn avaliar_operador_unario(op: OpUnario, operando: Valor, linha: usize) -> Result<Valor, ErroExecucao> {
    match op {
        OpUnario::Negativo => match operando {
            Valor::Inteiro(n) => Ok(Valor::Inteiro(-n)),
            Valor::Real(n) => Ok(Valor::Real(-n)),
            outro => Err(erro(
                linha,
                format!("'-' não pôde ser aplicado a '{}' (bug do checker?)", outro.nome_tipo()),
            )),
        },
        OpUnario::Nao => match operando {
            Valor::Logico(b) => Ok(Valor::Logico(!b)),
            outro => Err(erro(
                linha,
                format!(
                    "'.não.' não pôde ser aplicado a '{}' (bug do checker?)",
                    outro.nome_tipo()
                ),
            )),
        },
    }
}

/// *Cast* explícito (seção 10.5.1) — ambas as sintaxes (`tipo(x)` e
/// `(tipo) x`) já chegam aqui como o mesmo nó `Expr::Cast`.
fn converter_cast(tipo: TipoPrimitivo, valor: Valor, linha: usize) -> Result<Valor, ErroExecucao> {
    use TipoPrimitivo::*;
    match (tipo, &valor) {
        (Inteiro, Valor::Inteiro(_)) => Ok(valor),
        (Inteiro, Valor::Real(n)) => Ok(Valor::Inteiro(*n as i64)),
        (Inteiro, Valor::Cadeia(s)) => s
            .trim()
            .parse::<i64>()
            .map(Valor::Inteiro)
            .map_err(|_| erro(linha, format!("não foi possível converter \"{s}\" para 'inteiro'"))),

        (Real, Valor::Real(_)) => Ok(valor),
        (Real, Valor::Inteiro(n)) => Ok(Valor::Real(*n as f64)),
        (Real, Valor::Cadeia(s)) => s
            .trim()
            .parse::<f64>()
            .map(Valor::Real)
            .map_err(|_| erro(linha, format!("não foi possível converter \"{s}\" para 'real'"))),

        (Cadeia, Valor::Cadeia(_)) => Ok(valor),
        (Cadeia, Valor::Caractere(_)) => Ok(Valor::Cadeia(valor.to_string())),
        (Cadeia, Valor::Inteiro(_)) => Ok(Valor::Cadeia(valor.to_string())),
        (Cadeia, Valor::Real(_)) => Ok(Valor::Cadeia(valor.to_string())),

        (Caractere, Valor::Caractere(_)) => Ok(valor),
        (Caractere, Valor::Cadeia(s)) if s.chars().count() == 1 => {
            Ok(Valor::Caractere(s.chars().next().unwrap()))
        }
        (Caractere, Valor::Cadeia(s)) => Err(erro(
            linha,
            format!(
                "não foi possível converter \"{s}\" para 'caractere' — \
                 precisa ter exatamente 1 caractere (tem {})",
                s.chars().count()
            ),
        )),

        (Logico, Valor::Logico(_)) => Ok(valor),

        (destino, origem) => Err(erro(
            linha,
            format!(
                "não é possível converter '{}' para '{:?}' (bug do checker?)",
                origem.nome_tipo(),
                destino
            ),
        )),
    }
}

/// Converte a linha lida de `leia`/`leia_seco` (seção 6.1) para o tipo
/// atual da célula de destino — a PEPPE não tem "leia tipado" explícito:
/// o tipo já foi fixado na declaração de `var`, e a conversão usa o mesmo
/// parsing de `converter_cast` quando aplicável.
fn converter_entrada(valor_atual: &Valor, linha_lida: &str, linha: usize) -> Result<Valor, ErroExecucao> {
    match valor_atual {
        Valor::Inteiro(_) => linha_lida
            .trim()
            .parse::<i64>()
            .map(Valor::Inteiro)
            .map_err(|_| erro(linha, format!("entrada \"{linha_lida}\" não é um 'inteiro' válido"))),
        Valor::Real(_) => linha_lida
            .trim()
            .replace(',', ".")
            .parse::<f64>()
            .map(Valor::Real)
            .map_err(|_| erro(linha, format!("entrada \"{linha_lida}\" não é um 'real' válido"))),
        Valor::Cadeia(_) => Ok(Valor::Cadeia(linha_lida.to_string())),
        Valor::Caractere(_) => {
            let mut chars = linha_lida.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(Valor::Caractere(c)),
                (Some(c), Some(_)) => Ok(Valor::Caractere(c)), // pega só o 1º (tolerante)
                (None, _) => Err(erro(linha, "entrada vazia não é um 'caractere' válido")),
            }
        }
        Valor::Logico(_) => {
            let normalizado = linha_lida.trim().to_lowercase();
            match normalizado.as_str() {
                ".v." | ".verdadeiro." => Ok(Valor::Logico(true)),
                ".f." | ".falso." => Ok(Valor::Logico(false)),
                _ => Err(erro(
                    linha,
                    format!(
                        "entrada \"{linha_lida}\" não é um 'lógico' válido \
                         (use .v./.verdadeiro. ou .f./.falso.)"
                    ),
                )),
            }
        }
        Valor::Registro(_) | Valor::Conjunto { .. } => Err(erro(
            linha,
            "'leia' não pode ser usado diretamente em 'registro'/'conjunto' inteiros \
             — leia campo a campo ou elemento a elemento (bug do checker?)",
        )),
        Valor::Objeto { .. } => Err(erro(
            linha,
            "'leia' não pode ser usado diretamente em uma instância de classe inteira \
             — leia campo a campo (ex.: 'leia ESTUDANTE.NOME') (bug do checker?)",
        )),
        Valor::ReferenciaFuncao { .. } => Err(erro(
            linha,
            "'leia' não pode ser usado em uma variável de tipo função (seção 10.5.3) \
             — atribua a referência diretamente (ex.: 'RESPOSTA <- SOMATORIO') \
             (bug do checker?)",
        )),
    }
}

/// Formata um item de `escreva` aplicando os especificadores opcionais
/// `:largura` e `:largura:decimais` (seção 6.2.1) — só `inteiro`/`real`
/// suportam largura/decimais; para outros tipos, os especificadores são
/// ignorados nesta implementação (o checker já impede `:decimais` fora de
/// `real`, seção 6.2.1).
fn formatar_item_escreva(valor: &Valor, largura: Option<&Valor>, decimais: Option<&Valor>) -> String {
    let largura_n = largura.and_then(|v| match v {
        Valor::Inteiro(n) => Some(*n as usize),
        _ => None,
    });

    let texto = match (valor, decimais) {
        (Valor::Real(n), Some(Valor::Inteiro(d))) => format!("{:.*}", (*d).max(0) as usize, n),
        _ => valor.to_string(),
    };

    match largura_n {
        Some(largura) if texto.chars().count() < largura => {
            let espacos = largura - texto.chars().count();
            format!("{}{}", " ".repeat(espacos), texto)
        }
        _ => texto,
    }
}

// =====================================================================================
// Funções matemáticas pré-definidas (seção 5.6)
// =====================================================================================

/// Se `nome` corresponde a uma função/constante pré-definida (seção 5.6),
/// avalia os argumentos e retorna `Ok(Some(valor))`. Caso contrário,
/// `Ok(None)` (o chamador tenta como sub-rotina do usuário). Funções
/// pré-definidas são sempre "por valor" (nenhuma aceita `ref`).
fn avaliar_predefinida(
    nome: &str,
    argumentos: &[Expr],
    linha: usize,
    interp: &Interpretador,
    ambiente: &Ambiente,
    console: &mut dyn ConsoleIO,
) -> Result<Option<Valor>, ErroExecucao> {
    let chave = nome.to_lowercase();

    // Constantes (sem parênteses, mas o parser as trata como Expr::Variavel
    // quando usadas sem '()' — esta função só é chamada para Expr::Chamada,
    // ou seja, com parênteses; constantes puras são tratadas em
    // Self::resolver_celula caso o checker/parser as resolva como
    // identificador simples. Mantemos aqui apenas as funções com aridade.)

    let mut args = Vec::with_capacity(argumentos.len());
    for a in argumentos {
        args.push(interp.avaliar_expr(a, ambiente, console)?);
    }

    let como_f64 = |v: &Valor, linha: usize| -> Result<f64, ErroExecucao> {
        match v {
            Valor::Inteiro(n) => Ok(*n as f64),
            Valor::Real(n) => Ok(*n),
            outro => Err(erro(
                linha,
                format!("esperava número, encontrei '{}' (bug do checker?)", outro.nome_tipo()),
            )),
        }
    };

    let resultado = match chave.as_str() {
        "raizq" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.sqrt()),
        "raizc" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.cbrt()),
        "raize" if args.len() == 2 || args.len() == 3 => {
            let x = como_f64(&args[0], linha)?;
            let n = como_f64(&args[1], linha)?;
            let m = if args.len() == 3 { como_f64(&args[2], linha)? } else { 1.0 };
            Valor::Real(x.powf(m / n))
        }
        "abs" if args.len() == 1 => match &args[0] {
            Valor::Inteiro(n) => Valor::Inteiro(n.abs()),
            Valor::Real(n) => Valor::Real(n.abs()),
            outro => return Err(erro(linha, format!("'abs' não aceita '{}'", outro.nome_tipo()))),
        },
        "potência" if args.len() == 2 => {
            Valor::Real(como_f64(&args[0], linha)?.powf(como_f64(&args[1], linha)?))
        }
        "seno" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.sin()),
        "cosseno" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.cos()),
        "tangente" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.tan()),
        "arco_seno" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.asin()),
        "arco_cosseno" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.acos()),
        "arco_tangente" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.atan()),
        "graus_para_radianos" if args.len() == 1 => {
            Valor::Real(como_f64(&args[0], linha)?.to_radians())
        }
        "radianos_para_graus" if args.len() == 1 => {
            Valor::Real(como_f64(&args[0], linha)?.to_degrees())
        }
        "log" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.ln()),
        "log10" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.log10()),
        "exp" if args.len() == 1 => Valor::Real(como_f64(&args[0], linha)?.exp()),
        "piso" if args.len() == 1 => Valor::Inteiro(como_f64(&args[0], linha)?.floor() as i64),
        "teto" if args.len() == 1 => Valor::Inteiro(como_f64(&args[0], linha)?.ceil() as i64),
        "arredonda" if args.len() == 1 => {
            Valor::Inteiro(como_f64(&args[0], linha)?.round() as i64)
        }
        "trunca" if args.len() == 1 => Valor::Inteiro(como_f64(&args[0], linha)?.trunc() as i64),
        "máximo" if args.len() == 2 => {
            let a = como_f64(&args[0], linha)?;
            let b = como_f64(&args[1], linha)?;
            if matches!((&args[0], &args[1]), (Valor::Inteiro(_), Valor::Inteiro(_))) {
                Valor::Inteiro(a.max(b) as i64)
            } else {
                Valor::Real(a.max(b))
            }
        }
        "mínimo" if args.len() == 2 => {
            let a = como_f64(&args[0], linha)?;
            let b = como_f64(&args[1], linha)?;
            if matches!((&args[0], &args[1]), (Valor::Inteiro(_), Valor::Inteiro(_))) {
                Valor::Inteiro(a.min(b) as i64)
            } else {
                Valor::Real(a.min(b))
            }
        }
        "sinal" if args.len() == 1 => Valor::Inteiro(como_f64(&args[0], linha)?.signum() as i64),
        "aleatório" if args.is_empty() => {
            Valor::Real(pseudo_aleatorio_01())
        }
        "aleatório" if args.len() == 2 => {
            let min = como_f64(&args[0], linha)?.round() as i64;
            let max = como_f64(&args[1], linha)?.round() as i64;
            if max < min {
                return Err(erro(linha, "'aleatório(min, max)': min não pode ser maior que max"));
            }
            let amplitude = (max - min + 1).max(1);
            Valor::Inteiro(min + (pseudo_aleatorio_01() * amplitude as f64) as i64)
        }

        // -- Operações de texto (seção 20.2) ---------------------------------------
        "tamanho" if args.len() == 1 => {
            Valor::Inteiro(como_cadeia(&args[0], linha)?.chars().count() as i64)
        }
        "cópia" if args.len() == 3 => {
            let texto = como_cadeia(&args[0], linha)?;
            let inicio = como_f64(&args[1], linha)?.round() as i64;
            let quantidade = como_f64(&args[2], linha)?.round() as i64;
            Valor::Cadeia(copiar_trecho(&texto, inicio, quantidade, linha)?)
        }
        "posição" if args.len() == 2 => {
            let sub = como_cadeia(&args[0], linha)?;
            let texto = como_cadeia(&args[1], linha)?;
            Valor::Inteiro(buscar_posicao(&texto, &sub))
        }

        // -- Funções de cadeia adicionais ------------------------------------------
        "aparar" if args.len() == 1 => {
            Valor::Cadeia(como_cadeia(&args[0], linha)?.trim().to_string())
        }
        "maiúsculo" if args.len() == 1 => {
            Valor::Cadeia(como_cadeia(&args[0], linha)?.to_uppercase())
        }
        "minúsculo" if args.len() == 1 => {
            Valor::Cadeia(como_cadeia(&args[0], linha)?.to_lowercase())
        }
        "concatenar" if args.len() == 2 => {
            let mut s = como_cadeia(&args[0], linha)?;
            s.push_str(&como_cadeia(&args[1], linha)?);
            Valor::Cadeia(s)
        }

        // -- Funções de caractere (seção 5.6) --------------------------------------
        "ord" if args.len() == 1 => {
            let c = match &args[0] {
                Valor::Caractere(c) => *c,
                Valor::Cadeia(s) if s.chars().count() == 1 => s.chars().next().unwrap(),
                outro => return Err(erro(linha, format!("'ord' espera 'caractere', encontrei '{}'", outro.nome_tipo()))),
            };
            Valor::Inteiro(c as i64)
        }
        "chr" if args.len() == 1 => {
            let n = como_f64(&args[0], linha)?.round() as u32;
            let c = char::from_u32(n).ok_or_else(|| erro(linha, format!("'chr({n})': código inválido")))?;
            Valor::Caractere(c)
        }
        "succ" if args.len() == 1 => match &args[0] {
            Valor::Inteiro(n) => Valor::Inteiro(n + 1),
            Valor::Caractere(c) => {
                let next = char::from_u32(*c as u32 + 1)
                    .ok_or_else(|| erro(linha, "'succ': não há caractere seguinte"))?;
                Valor::Caractere(next)
            }
            outro => return Err(erro(linha, format!("'succ' espera inteiro ou caractere, encontrei '{}'", outro.nome_tipo()))),
        },
        "pred" if args.len() == 1 => match &args[0] {
            Valor::Inteiro(n) => Valor::Inteiro(n - 1),
            Valor::Caractere(c) => {
                let n = *c as u32;
                if n == 0 { return Err(erro(linha, "'pred': não há caractere anterior")); }
                Valor::Caractere(char::from_u32(n - 1).unwrap())
            }
            outro => return Err(erro(linha, format!("'pred' espera inteiro ou caractere, encontrei '{}'", outro.nome_tipo()))),
        },

        _ => return Ok(None),
    };

    Ok(Some(resultado))
}

/// Converte `v` para `String`, aceitando `cadeia` ou `caractere` (seção
/// 20.2 — as operações de texto aceitam ambos, por conveniência, igual à
/// concatenação com `+`, seção 10.5.2).
fn como_cadeia(v: &Valor, linha: usize) -> Result<String, ErroExecucao> {
    match v {
        Valor::Cadeia(s) => Ok(s.clone()),
        Valor::Caractere(c) => Ok(c.to_string()),
        outro => Err(erro(
            linha,
            format!("esperava 'cadeia' ou 'caractere', encontrei '{}'", outro.nome_tipo()),
        )),
    }
}

/// `copia(S, inicio, quantidade)` (seção 20.2) — `inicio` é 1-based
/// (convenção PEPPE, igual aos índices de `conjunto`, seção 4.5). Erro de
/// execução se `inicio` estiver fora dos limites de `S`; `quantidade` é
/// automaticamente limitada ao que resta em `S` a partir de `inicio` (não
/// é erro pedir mais caracteres do que existem — comportamento comum em
/// Pascal/BASIC, mais tolerante para o aluno).
fn copiar_trecho(s: &str, inicio: i64, quantidade: i64, linha: usize) -> Result<String, ErroExecucao> {
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len() as i64;
    if inicio < 1 || inicio > total.max(1) {
        return Err(erro(
            linha,
            format!(
                "'copia': início {inicio} fora dos limites — \"{s}\" tem {total} caractere(s) \
                 (seção 15 — erro de execução)"
            ),
        ));
    }
    if quantidade < 0 {
        return Err(erro(linha, "'copia': quantidade não pode ser negativa"));
    }
    let inicio_idx = (inicio - 1) as usize;
    let fim_idx = ((inicio - 1 + quantidade) as usize).min(chars.len());
    Ok(chars[inicio_idx..fim_idx].iter().collect())
}

/// `posicao(SUB, S)` (seção 20.2) — posição (1-based) da primeira
/// ocorrência de `sub` dentro de `texto`, ou `0` se não encontrada
/// (convenção Pascal `Pos`, mais simples para o aluno do que `-1` ou um
/// tipo `Option`).
fn buscar_posicao(texto: &str, sub: &str) -> i64 {
    if sub.is_empty() {
        return 0;
    }
    let chars: Vec<char> = texto.chars().collect();
    let sub_chars: Vec<char> = sub.chars().collect();
    if sub_chars.len() > chars.len() {
        return 0;
    }
    for i in 0..=(chars.len() - sub_chars.len()) {
        if chars[i..i + sub_chars.len()] == sub_chars[..] {
            return (i + 1) as i64;
        }
    }
    0
}

/// Gerador pseudo-aleatório simples baseado no relógio do sistema —
/// Gerador de números pseudo-aleatórios com estado persistente entre
/// chamadas (LCG — Linear Congruential Generator, parâmetros de Knuth/MMIX).
/// Inicializado uma única vez com o tempo do sistema; chamadas subsequentes
/// avançam o estado interno, produzindo sequências diferentes a cada execução.
fn pseudo_aleatorio_01() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};

    thread_local! {
        static ESTADO: Cell<u64> = Cell::new(0);
        static INICIADO: Cell<bool> = Cell::new(false);
    }

    INICIADO.with(|ini| {
        if !ini.get() {
            let semente = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            ESTADO.with(|s| s.set(semente ^ 0x9E3779B97F4A7C15));
            ini.set(true);
        }
    });

    let novo = ESTADO.with(|s| {
        // LCG com parâmetros de Knuth (MMIX)
        let v = s.get().wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s.set(v);
        v
    });

    // Usa os 53 bits superiores para precisão dupla em [0, 1)
    (novo >> 11) as f64 / (1u64 << 53) as f64
}

// =====================================================================================
// Testes
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clonar_por_valor_registro_nao_compartilha_celulas() {
        let mut campos = HashMap::new();
        campos.insert("X".to_string(), nova_celula(Valor::Inteiro(10)));
        let original = Valor::Registro(campos);

        let copia = original.clonar_por_valor();

        // Alterar a célula da cópia não deve afetar o original.
        if let Valor::Registro(campos_copia) = &copia {
            *campos_copia.get("X").unwrap().borrow_mut() = Valor::Inteiro(99);
        }
        if let Valor::Registro(campos_original) = &original {
            assert_eq!(*campos_original.get("X").unwrap().borrow(), Valor::Inteiro(10));
        }
    }

    #[test]
    fn ambiente_case_insensitive() {
        let mut amb = Ambiente::novo();
        amb.declarar("NOTA", nova_celula(Valor::Real(8.5)));
        let c = amb.buscar("nota").expect("busca case-insensitive deveria funcionar");
        assert_eq!(*c.borrow(), Valor::Real(8.5));
    }

    #[test]
    fn ambiente_escopos_aninhados_com_shadowing() {
        let mut amb = Ambiente::novo();
        amb.declarar("X", nova_celula(Valor::Inteiro(1)));
        amb.entrar_escopo();
        amb.declarar("X", nova_celula(Valor::Inteiro(2)));
        assert_eq!(*amb.buscar("X").unwrap().borrow(), Valor::Inteiro(2));
        amb.sair_escopo();
        assert_eq!(*amb.buscar("X").unwrap().borrow(), Valor::Inteiro(1));
    }

    #[test]
    fn celulas_compartilhadas_refletem_mutacao() {
        // Simula o efeito de 'ref': duas "variáveis" apontando para a
        // mesma célula devem ver a mesma mutação.
        let mut amb = Ambiente::novo();
        let celula = nova_celula(Valor::Inteiro(0));
        amb.declarar("A", celula.clone());
        amb.declarar("B", celula.clone());

        *amb.buscar("A").unwrap().borrow_mut() = Valor::Inteiro(42);
        assert_eq!(*amb.buscar("B").unwrap().borrow(), Valor::Inteiro(42));
    }

    // =================================================================================
    // Testes de integração: lexer -> parser -> interpreter
    // =================================================================================

    /// Tokeniza, analisa e executa `fonte` com a `entrada` fornecida
    /// (uma linha por chamada de `leia`/`leia_seco`), retornando a saída
    /// acumulada via `escreva`. Falha o teste em caso de erro léxico,
    /// sintático ou de execução.
    fn executar(fonte: &str, entrada: &[&str]) -> String {
        let tokens = crate::lexer::tokenizar(fonte).expect("erro léxico inesperado");
        let programa = crate::parser::parsear(tokens).expect("erro sintático inesperado");
        let mut console = ConsoleMemoria::com_entrada(entrada);
        interpretar(&programa, &mut console).expect("erro de execução inesperado");
        console.saida
    }

    #[test]
    fn adicao_numeros_completo() {
        let saida = executar(
            r#"programa ADIÇÃO_NÚMEROS
var
  X, A, B : inteiro
início
  leia A
  leia B
  X <- A + B
  escreva "Resultado = ", X, "\n"
fim"#,
            &["10", "32"],
        );
        assert_eq!(saida, "Resultado = 42\n");
    }

    #[test]
    fn escreva_sem_quebra_automatica() {
        // ✅ v0.4: 'escreva' não adiciona '\n' automaticamente.
        let saida = executar(
            r#"programa P
início
  escreva "A"
  escreva "B"
  escreva "C"
fim"#,
            &[],
        );
        assert_eq!(saida, "ABC");
    }

    #[test]
    fn escreva_com_formatacao_largura_e_decimais() {
        let saida = executar(
            r#"programa P
var
  R : real
  N : inteiro
início
  R <- 3.14159
  N <- 8
  escreva R : 10 : 2, "\n"
  escreva N : 5, "\n"
fim"#,
            &[],
        );
        assert_eq!(saida, "      3.14\n    8\n");
    }

    #[test]
    fn escreva_ln_adiciona_quebra_de_linha_apos_todos_os_itens() {
        // 'escreva_ln A, B, C' imprime A, B, C e UMA quebra de linha ao
        // final — não uma quebra por item (seção 6.2.2, estilo Pascal
        // 'writeln').
        let saida = executar(
            r#"programa P
var
  A, B, C : inteiro
início
  A <- 1
  B <- 2
  C <- 3
  escreva_ln A, B, C
  escreva "FIM"
fim"#,
            &[],
        );
        assert_eq!(saida, "123\nFIM");
    }

    #[test]
    fn escreva_ln_sozinho_imprime_apenas_quebra_de_linha() {
        let saida = executar(
            r#"programa P
início
  escreva "A"
  escreva_ln
  escreva "B"
fim"#,
            &[],
        );
        assert_eq!(saida, "A\nB");
    }

    #[test]
    fn escreva_ln_preserva_especificadores_de_formatacao() {
        let saida = executar(
            r#"programa P
var
  R : real
início
  R <- 3.14159
  escreva_ln R : 10 : 2
  escreva "FIM"
fim"#,
            &[],
        );
        assert_eq!(saida, "      3.14\nFIM");
    }

    #[test]
    fn varios_escreva_ln_em_sequencia_cada_um_com_sua_propria_quebra() {
        let saida = executar(
            r#"programa P
início
  escreva_ln "linha 1"
  escreva_ln "linha 2"
fim"#,
            &[],
        );
        assert_eq!(saida, "linha 1\nlinha 2\n");
    }

    #[test]
    fn saida_logica_com_pontos_maiusculo() {
        let saida = executar(
            r#"programa P
var
  ATIVO : lógico
início
  ATIVO <- .verdadeiro.
  escreva ATIVO, "\n"
  ATIVO <- .f.
  escreva ATIVO
fim"#,
            &[],
        );
        assert_eq!(saida, ".VERDADEIRO.\n.FALSO.");
    }

    #[test]
    fn se_senao() {
        let saida = executar(
            r#"programa P
var
  N : inteiro
início
  leia N
  se (N > 0) então
    escreva "positivo"
  senão
    escreva "não positivo"
  fim_se
fim"#,
            &["5"],
        );
        assert_eq!(saida, "positivo");

        let saida = executar(
            r#"programa P
var
  N : inteiro
início
  leia N
  se (N > 0) então
    escreva "positivo"
  senão
    escreva "não positivo"
  fim_se
fim"#,
            &["-3"],
        );
        assert_eq!(saida, "não positivo");
    }

    #[test]
    fn caso_com_e_sem_correspondencia() {
        let saida = executar(
            r#"programa P
var
  N : inteiro
início
  leia N
  caso N
    seja 1 faça
      escreva "um"
    seja 2 faça
      escreva "dois"
  senão
    escreva "outro"
  fim_caso
fim"#,
            &["2"],
        );
        assert_eq!(saida, "dois");
    }

    #[test]
    fn enquanto_conta_ate_cinco() {
        let saida = executar(
            r#"programa P
var
  I : inteiro
início
  I <- 1
  enquanto (I <= 5) faça
    escreva I
    I <- I + 1
  fim_enquanto
fim"#,
            &[],
        );
        assert_eq!(saida, "12345");
    }

    #[test]
    fn para_com_passo_positivo_e_negativo() {
        let saida = executar(
            r#"programa P
var
  I : inteiro
início
  para I de 1 até 5 passo 1 faça
    escreva I
  fim_para
  para I de 5 até 1 passo -1 faça
    escreva I
  fim_para
fim"#,
            &[],
        );
        assert_eq!(saida, "1234554321");
    }

    #[test]
    fn fatorial_recursivo_com_funcao() {
        let saida = executar(
            r#"programa P
  função FATORIAL(N : inteiro) : inteiro
  início
    se (N <= 1) então
      FATORIAL <- 1
    senão
      FATORIAL <- N * FATORIAL(N - 1)
    fim_se
  fim
início
  escreva FATORIAL(5)
fim"#,
            &[],
        );
        assert_eq!(saida, "120");
    }

    #[test]
    fn conjunto_dinamico_passado_por_referencia_recursivamente() {
        // Padrão do material de origem (MOSTRA_MAIOR_VALOR_DA_MATRIZ):
        // função recursiva que recebe um 'conjunto' por 'ref' (parâmetro
        // declarado com tipo dinâmico, mas o argumento real é estático —
        // compatibilidade estrutural por número de dimensões, não pelos
        // limites exatos).
        let saida = executar(
            r#"programa P
  função MAIOR(ref A : conjunto [] de inteiro; TAM : inteiro) : inteiro
  var
    MAX : inteiro
  início
    se (TAM = 1) então
      MAIOR <- A[1]
    senão
      MAX <- MAIOR(A, TAM - 1)
      se (MAX > A[TAM]) então
        MAIOR <- MAX
      senão
        MAIOR <- A[TAM]
      fim_se
    fim_se
  fim
var
  MAT : conjunto [1..5] de inteiro
  I : inteiro
início
  MAT[1] <- 3
  MAT[2] <- 7
  MAT[3] <- 2
  MAT[4] <- 9
  MAT[5] <- 5
  escreva MAIOR(MAT, 5)
fim"#,
            &[],
        );
        assert_eq!(saida, "9");
    }

    #[test]
    fn procedimento_com_parametro_por_referencia() {
        let saida = executar(
            r#"programa P
  procedimento DOBRA(ref X : inteiro)
  início
    X <- X * 2
  fim
var
  N : inteiro
início
  N <- 21
  DOBRA(N)
  escreva N
fim"#,
            &[],
        );
        assert_eq!(saida, "42");
    }

    #[test]
    fn parametro_por_valor_nao_afeta_chamador() {
        let saida = executar(
            r#"programa P
  procedimento TENTA_DOBRAR(X : inteiro)
  início
    X <- X * 2
  fim
var
  N : inteiro
início
  N <- 21
  TENTA_DOBRAR(N)
  escreva N
fim"#,
            &[],
        );
        assert_eq!(saida, "21");
    }

    #[test]
    fn interrompa_para_laco_indefinido() {
        let saida = executar(
            r#"programa P
var
  I : inteiro
início
  I <- 0
  laço
    I <- I + 1
    escreva I
    saia_caso (I >= 3)
  fim_laço
fim"#,
            &[],
        );
        assert_eq!(saida, "123");
    }

    #[test]
    fn continue_pula_iteracao_no_para() {
        // continue dentro de para: I=5 é pulado, os demais são impressos
        let saida = executar(
            r#"programa P
var
  I : inteiro
início
  para I de 1 até 8 passo 1 faça
    se I = 5 então
      continue
    fim_se
    escreva I
  fim_para
fim"#,
            &[],
        );
        assert_eq!(saida, "1234678");
    }

    #[test]
    fn registro_e_acesso_a_campo() {
        let saida = executar(
            r#"programa P
tipo
  CAD_ALUNO = registro
                NOME : cadeia
                NOTA : real
              fim_registro
var
  ALUNO : cad_aluno
início
  ALUNO.NOME <- "Maria"
  ALUNO.NOTA <- 9.5
  escreva ALUNO.NOME, " - ", ALUNO.NOTA : 4 : 1
fim"#,
            &[],
        );
        assert_eq!(saida, "Maria -  9.5");
    }

    #[test]
    fn registro_copia_por_valor() {
        let saida = executar(
            r#"programa P
tipo
  CAD = registro
          X : inteiro
        fim_registro
var
  A, B : cad
início
  A.X <- 1
  B <- A
  B.X <- 99
  escreva A.X, " ", B.X
fim"#,
            &[],
        );
        // Atribuir um registro a outro deve COPIAR — alterar B.X não pode
        // afetar A.X (seção 10.5, semântica de valor para 'registro').
        assert_eq!(saida, "1 99");
    }

    #[test]
    fn conjunto_estatico_leitura_e_escrita() {
        let saida = executar(
            r#"programa P
var
  NOTAS : conjunto [1..3] de real
  I : inteiro
início
  para I de 1 até 3 passo 1 faça
    NOTAS[I] <- I * 1.0
  fim_para
  escreva NOTAS[1] : 3 : 1, " ", NOTAS[2] : 3 : 1, " ", NOTAS[3] : 3 : 1
fim"#,
            &[],
        );
        assert_eq!(saida, "1.0 2.0 3.0");
    }

    #[test]
    fn conjunto_com_limites_cardinais_zero_a_n_menos_um() {
        // Composição cardinal (0-based), estilo Pascal/BASIC — os limites
        // não precisam começar em 1 (seção 4.5): aqui [0..4] para 5
        // posições.
        let saida = executar(
            r#"programa P
var
  V : conjunto [0..4] de inteiro
  I : inteiro
início
  para I de 0 até 4 passo 1 faça
    V[I] <- I * I
  fim_para
  para I de 0 até 4 passo 1 faça
    escreva V[I], " "
  fim_para
fim"#,
            &[],
        );
        assert_eq!(saida, "0 1 4 9 16 ");
    }

    #[test]
    fn conjunto_com_limites_deslocados() {
        // Limites arbitrários, nem 0-based nem 1-based — [2..6] para 5
        // posições.
        let saida = executar(
            r#"programa P
var
  V : conjunto [2..6] de inteiro
  I : inteiro
início
  para I de 2 até 6 passo 1 faça
    V[I] <- I * 10
  fim_para
  para I de 2 até 6 passo 1 faça
    escreva V[I], " "
  fim_para
fim"#,
            &[],
        );
        assert_eq!(saida, "20 30 40 50 60 ");
    }

    #[test]
    fn conjunto_indice_fora_do_limite_cardinal_e_erro() {
        // [0..4]: índice 5 está fora (só 0,1,2,3,4 são válidos).
        let tokens = crate::lexer::tokenizar(
            r#"programa P
var
  V : conjunto [0..4] de inteiro
início
  V[5] <- 1
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("fora dos limites"));
    }

    #[test]
    fn conjunto_dinamico_com_dimensione() {
        let saida = executar(
            r#"programa P
var
  N, I : inteiro
  A : conjunto [] de inteiro
início
  leia N
  dimensione A[1..N]
  para I de 1 até N passo 1 faça
    A[I] <- I * I
  fim_para
  para I de 1 até N passo 1 faça
    escreva A[I], " "
  fim_para
fim"#,
            &["4"],
        );
        assert_eq!(saida, "1 4 9 16 ");
    }

    #[test]
    fn matriz_2d_totalmente_dinamica() {
        // 'conjunto [,] de <tipo>' (questão #7 da seção 13) — ambas as
        // dimensões são definidas só em tempo de execução, via
        // 'dimensione M[1..L, 1..C]'.
        let saida = executar(
            r#"programa P
var
  L, C, I, J : inteiro
  M : conjunto [,] de inteiro
início
  L <- 2
  C <- 3
  dimensione M[1..L, 1..C]
  para I de 1 até L passo 1 faça
    para J de 1 até C passo 1 faça
      M[I, J] <- I * 10 + J
    fim_para
  fim_para
  para I de 1 até L passo 1 faça
    para J de 1 até C passo 1 faça
      escreva M[I, J], " "
    fim_para
  fim_para
fim"#,
            &[],
        );
        assert_eq!(saida, "11 12 13 21 22 23 ");
    }

    #[test]
    fn indice_fora_dos_limites_e_erro_de_execucao() {
        let tokens = crate::lexer::tokenizar(
            r#"programa P
var
  A : conjunto [1..3] de inteiro
início
  A[5] <- 1
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("fora dos limites"));
    }

    #[test]
    fn divisao_por_zero_e_erro_de_execucao() {
        let tokens = crate::lexer::tokenizar(
            r#"programa P
var
  A, B, R : inteiro
início
  A <- 10
  B <- 0
  R <- A div B
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("divisão por zero"));
    }

    #[test]
    fn divisao_real_por_zero_nao_e_erro_fatal_aqui() {
        // '/' (divisão real) com divisor 0.0 é tratado como erro de
        // execução também, por consistência didática (evitar 'inf'/'NaN'
        // silenciosos em um contexto pedagógico).
        let tokens = crate::lexer::tokenizar(
            r#"programa P
var
  A, B, R : real
início
  A <- 10.0
  B <- 0.0
  R <- A / B
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("divisão por zero"));
    }

    #[test]
    fn cast_explicito_funciona() {
        let saida = executar(
            r#"programa P
var
  N : inteiro
  R : real
início
  R <- 3.99
  N <- inteiro(R)
  escreva N, " ", cadeia(N)
fim"#,
            &[],
        );
        assert_eq!(saida, "3 3");
    }

    #[test]
    fn pausa_consome_uma_linha_de_entrada() {
        let saida = executar(
            r#"programa P
var
  N : inteiro
início
  pausa
  leia N
  escreva N
fim"#,
            &["", "7"],
        );
        assert_eq!(saida, "7");
    }

    #[test]
    fn funcoes_matematicas_predefinidas() {
        let saida = executar(
            r#"programa P
var
  X : real
início
  X <- raizq(16.0)
  escreva X : 4 : 1
fim"#,
            &[],
        );
        assert_eq!(saida, " 4.0");
    }

    #[test]
    fn tamanho_de_cadeia() {
        let saida = executar(
            r#"programa P
var
  NOME : cadeia
  N : inteiro
início
  NOME <- "Maria"
  N <- tamanho(NOME)
  escreva N
fim"#,
            &[],
        );
        assert_eq!(saida, "5");
    }

    #[test]
    fn copia_trecho_do_meio() {
        let saida = executar(
            r#"programa P
início
  escreva cópia("PROGRAMAÇÃO", 1, 4)
fim"#,
            &[],
        );
        assert_eq!(saida, "PROG");
    }

    #[test]
    fn copia_quantidade_maior_que_o_restante_e_tolerante() {
        let saida = executar(
            r#"programa P
início
  escreva cópia("AB", 1, 100)
fim"#,
            &[],
        );
        assert_eq!(saida, "AB");
    }

    #[test]
    fn copia_inicio_fora_dos_limites_e_erro_de_execucao() {
        let tokens = crate::lexer::tokenizar(
            r#"programa P
início
  escreva cópia("AB", 5, 1)
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("fora dos limites"));
    }

    #[test]
    fn posicao_encontra_subcadeia() {
        let saida = executar(
            r#"programa P
início
  escreva posição("ana", "banana")
fim"#,
            &[],
        );
        // "banana": b-a-n-a-n-a — "ana" começa na posição 2 (1-based).
        assert_eq!(saida, "2");
    }

    #[test]
    fn posicao_nao_encontrada_retorna_zero() {
        let saida = executar(
            r#"programa P
início
  escreva posição("xyz", "banana")
fim"#,
            &[],
        );
        assert_eq!(saida, "0");
    }

    #[test]
    fn operacoes_de_string_combinadas_em_validacao() {
        // Caso de uso típico (seção 20.2): validar se uma cadeia contém
        // uma subcadeia e extrair um trecho dela.
        let saida = executar(
            r#"programa P
var
  NOME : cadeia
  ENCONTRADO : inteiro
início
  NOME <- "Maria Silva"
  ENCONTRADO <- posição(" ", NOME)
  se (ENCONTRADO > 0) então
    escreva cópia(NOME, 1, ENCONTRADO - 1)
  senão
    escreva NOME
  fim_se
fim"#,
            &[],
        );
        assert_eq!(saida, "Maria");
    }

    #[test]
    fn ir_para_salta_para_a_frente() {
        // (Nome do rótulo não pode ser 'FIM': é a palavra-chave 'fim' em
        // qualquer grafia, PEPPE é case-insensitive — seção 1.3.)
        let saida = executar(
            r#"programa P
início
  escreva "a"
  ir_para TERMINO
  escreva "isto não deveria aparecer"
  TERMINO:
    escreva "b"
fim"#,
            &[],
        );
        assert_eq!(saida, "ab");
    }

    #[test]
    fn ir_para_salta_para_tras_simulando_laco() {
        let saida = executar(
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
            &[],
        );
        assert_eq!(saida, "123");
    }

    #[test]
    fn ir_para_rotulo_inalcancavel_e_erro_de_execucao() {
        // O checker aceita (rótulo visível em toda a sub-rotina, seção 8),
        // mas o interpretador não consegue saltar PARA DENTRO de um bloco
        // 'se' a partir de fora dele (limitação documentada em
        // 'executar_bloco') — deve falhar com erro claro, não travar nem
        // silenciosamente pular o resto do programa.
        let tokens = crate::lexer::tokenizar(
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
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::com_entrada(&["1"]);
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("ir_para DENTRO"));
    }

    // =====================================================================================
    // Programação Orientada a Objetos (seção 10) — Fase 1: classe sem herança
    // =====================================================================================

    #[test]
    fn classe_com_metodo_interno_executa_corretamente() {
        // Equivalente a CLASSE_OBJETO_MÉTODO_INTERNO do material de origem.
        let saida = executar(
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
  escreva ESTUDANTE.NOME, "\n"
  escreva ESTUDANTE.MÉDIA
fim"#,
            &["Ana", "10", "8", "6", "4"],
        );
        assert_eq!(saida, "Ana\n7");
    }

    #[test]
    fn classe_com_metodo_externo_executa_corretamente() {
        // Equivalente a CLASSE_OBJETO_MÉTODO_EXTERNO do material de origem.
        let saida = executar(
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

var
  I : inteiro

início
  para I de 1 até 4 passo 1 faça
    leia ESTUDANTE.NOTAS[I]
  fim_para
  ESTUDANTE.CALCMÉDIA()
  escreva ESTUDANTE.MÉDIA
fim"#,
            &["10", "8", "6", "4"],
        );
        assert_eq!(saida, "7");
    }

    #[test]
    fn encapsulamento_com_metodos_get_set_e_parametro_sombreando_campo() {
        // Equivalente ao núcleo de ENCAPSULAMENTO: PÕENOME(NOME) com campo
        // NOME — parâmetro sombreia campo; este.NOME acessa o campo.
        let saida = executar(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              procedimento PÕENOME(NOME : cadeia)
              função PEGANOME() : cadeia
            seção_privada
              NOME : cadeia
          fim_classe

  procedimento Aluno..PÕENOME(NOME : cadeia)
  início
    este.NOME <- NOME
  fim

  função Aluno..PEGANOME() : cadeia
  início
    PEGANOME <- NOME
  fim

objeto
  ESTUDANTE : Aluno

início
  ESTUDANTE.PÕENOME("Ana")
  escreva ESTUDANTE.PEGANOME()
fim"#,
            &[],
        );
        assert_eq!(saida, "Ana");
    }

    #[test]
    fn objeto_e_var_compartilham_a_mesma_semantica_de_referencia() {
        // 'REFERENCIA <- OBJ2' faz REFERENCIA e OBJ2 apontarem para a
        // MESMA instância — mutar via um é visível através do outro
        // (seção 10.4). Nome não pode ser 'REF': colide com a palavra-
        // chave reservada 'ref' (seção 9.3, case-insensitive).
        let saida = executar(
            r#"programa P
tipo
  Aluno = classe
            seção_pública
              NOME : cadeia
          fim_classe

objeto
  REFERENCIA : Aluno

var
  OBJ2 : Aluno

início
  OBJ2.NOME <- "Ana"
  REFERENCIA <- OBJ2
  REFERENCIA.NOME <- "Beatriz"
  escreva OBJ2.NOME
fim"#,
            &[],
        );
        assert_eq!(saida, "Beatriz");
    }

    #[test]
    fn heranca_simples_atribuicao_de_derivada_para_base() {
        // Núcleo de POLIFORMISMO_UNIVERSAL_INCLUSÃO (sem dispatch
        // dinâmico ainda — Fase 4): 'REFERENCIA <- OBJ2' (Pai <- Filho)
        // deve funcionar, e os campos herdados de 'Filho' devem incluir
        // os de 'Pai'. Nome não pode ser 'REF': colide com a
        // palavra-chave reservada 'ref' (seção 9.3, case-insensitive).
        let saida = executar(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            NOME : cadeia
        fim_classe

  Filho = classe herança de Pai
            seção_pública
              IDADE : inteiro
          fim_classe

objeto
  REFERENCIA : Pai

var
  OBJ2 : Filho

início
  OBJ2.NOME <- "Ana"
  OBJ2.IDADE <- 20
  REFERENCIA <- OBJ2
  escreva REFERENCIA.NOME
fim"#,
            &[],
        );
        assert_eq!(saida, "Ana");
    }

    #[test]
    fn virtual_sobrepor_dispatch_dinamico_via_atribuicao_de_referencia() {
        // Núcleo de POLIFORMISMO_UNIVERSAL_INCLUSÃO (seção 10.6, exemplo
        // completo da especificação): 'OBJ_BASE.EXECUTA()' deve executar
        // a versão de 'Filho' depois de 'OBJ_BASE <- OBJ2' (Pai <-
        // Filho), e voltar a executar a de 'Pai' depois de 'OBJ_BASE <-
        // OBJ1'. Dispatch dinâmico decorre da classe REAL da instância
        // (armazenada em 'Valor::Objeto'), não do tipo declarado da
        // variável.
        let saida = executar(
            r#"programa P
tipo
  Pai = classe
          seção_pública
            virtual procedimento EXECUTA()
        fim_classe

  procedimento Pai..EXECUTA()
  início
    escreva "Pai"
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor procedimento EXECUTA()
          fim_classe

  procedimento Filho..EXECUTA()
  início
    escreva "Filho"
  fim

objeto
  OBJ_BASE : Pai

var
  OBJ1 : Pai
  OBJ2 : Filho

início
  OBJ1.EXECUTA()
  OBJ2.EXECUTA()

  OBJ_BASE <- OBJ2
  OBJ_BASE.EXECUTA()

  OBJ_BASE <- OBJ1
  OBJ_BASE.EXECUTA()
fim"#,
            &[],
        );
        assert_eq!(saida, "PaiFilhoFilhoPai");
    }

    #[test]
    fn sobrecarga_de_subrotina_solta_executa_a_versao_certa_por_aridade_e_tipo() {
        // Núcleo do exemplo CALCULAR do material (seção 10.5): três
        // sobrecargas, cada chamada deve executar a versão certa.
        let saida = executar(
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

início
  escreva CALCULAR(5), "\n"
  escreva CALCULAR(2.0, 3.0), "\n"
  escreva CALCULAR(1, 2, 3)
fim"#,
            &[],
        );
        assert_eq!(saida, "10\n6\n6");
    }

    #[test]
    fn sobrecarga_de_metodo_executa_a_versao_certa_por_aridade() {
        let saida = executar(
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

início
  escreva CALC.CALCULAR(5), "\n"
  escreva CALC.CALCULAR(2.0, 3.0)
fim"#,
            &[],
        );
        assert_eq!(saida, "10\n6");
    }

    #[test]
    fn heranca_multipla_metodo_executa_corretamente_sem_ambiguidade() {
        // Exemplo de referência do autor: CLS_ALUNO herda de CLS_SALA e
        // CLS_TURMA, sem colisão de nome — método herdado de qualquer
        // uma das duas bases deve executar normalmente.
        let saida = executar(
            r#"programa P
tipo
  CLS_SALA = classe
               seção_pública
                 SALA : inteiro
                 função PEGA_SALA() : inteiro
             fim_classe

  função CLS_SALA..PEGA_SALA() : inteiro
  início
    PEGA_SALA <- SALA
  fim

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

início
  ALUNO.SALA <- 12
  escreva ALUNO.PEGA_SALA()
fim"#,
            &[],
        );
        assert_eq!(saida, "12");
    }

    #[test]
    fn heranca_multipla_qualificador_de_base_executa_a_versao_certa() {
        // Duas bases com método de mesmo nome e assinatura — ambíguo
        // sem qualificador (rejeitado pelo checker); aqui testamos que,
        // com o qualificador 'CLS_A..', o interpretador de fato executa
        // a versão de CLS_A, e com 'CLS_B..', a de CLS_B.
        let saida = executar(
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

início
  escreva CLS_A..OBJ.PEGA(), "\n"
  escreva CLS_B..OBJ.PEGA()
fim"#,
            &[],
        );
        assert_eq!(saida, "1\n2");
    }

    // =====================================================================================
    // Funções como valores de primeira classe (seção 10.5.3)
    // =====================================================================================

    #[test]
    fn chamada_indireta_de_subrotina_solta_executa_corretamente() {
        // Núcleo de POLIFORMISMO_ADHOC_SOBRECARGA_2 do material de origem:
        // RESPOSTA guarda ora SOMATORIO, ora FATORIAL, e a chamada
        // indireta despacha para a função certa em cada caso.
        let saida = executar(
            r#"programa P
tipo
  FUNC1 = função(inteiro)

  função SOMATORIO(N : inteiro) : inteiro
  var
    I, SOMA : inteiro
  início
    SOMA <- 0
    para I de 1 até N passo 1 faça
      SOMA <- SOMA + I
    fim_para
    SOMATORIO <- SOMA
  fim

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
  RESPOSTA : FUNC1

início
  RESPOSTA <- SOMATORIO
  escreva RESPOSTA(4), "\n"
  RESPOSTA <- FATORIAL
  escreva RESPOSTA(4)
fim"#,
            &[],
        );
        // SOMATORIO(4) = 1+2+3+4 = 10; FATORIAL(4) = 1*2*3*4 = 24
        assert_eq!(saida, "10\n24");
    }

    #[test]
    fn chamada_indireta_de_metodo_respeita_dispatch_dinamico() {
        // RESPOSTA captura ESTUDANTE.EXECUTA enquanto ESTUDANTE é, na
        // real, uma instância de 'Filho' (Fase 4, dispatch dinâmico) —
        // a chamada indireta deve executar a versão sobreposta, não a
        // do tipo declarado da variável que originou a captura.
        let saida = executar(
            r#"programa P
tipo
  FUNC1 = função()

  Pai = classe
          seção_pública
            virtual função EXECUTA() : inteiro
        fim_classe

  função Pai..EXECUTA() : inteiro
  início
    EXECUTA <- 1
  fim

  Filho = classe herança de Pai
            seção_pública
              sobrepor função EXECUTA() : inteiro
          fim_classe

  função Filho..EXECUTA() : inteiro
  início
    EXECUTA <- 2
  fim

objeto
  REFERENCIA : Pai

var
  OBJ2 : Filho
  RESPOSTA : FUNC1

início
  REFERENCIA <- OBJ2
  RESPOSTA <- REFERENCIA.EXECUTA
  escreva RESPOSTA()
fim"#,
            &[],
        );
        assert_eq!(saida, "2");
    }

    #[test]
    fn chamada_indireta_como_comando_solto_descarta_retorno() {
        // 'RESPOSTA()' sozinho, como comando (não dentro de uma
        // expressão) — mesma permissividade de 'OBJETO.MÉTODO()' como
        // comando (seção 10.4): executa pelo efeito colateral, descarta
        // o valor de retorno.
        let saida = executar(
            r#"programa P
tipo
  FUNC1 = função()

  função MARCA() : inteiro
  início
    escreva "executei"
    MARCA <- 99
  fim

var
  RESPOSTA : FUNC1

início
  RESPOSTA <- MARCA
  RESPOSTA()
fim"#,
            &[],
        );
        assert_eq!(saida, "executei");
    }

    // =====================================================================================
    // Escopo léxico no estilo Pascal — visibilidade por ordem de declaração (seção 9.6)
    // =====================================================================================

    #[test]
    fn subrotina_ve_variavel_global_declarada_antes_dela() {
        // Caso real do material de origem (CALCULADORA_V4): uma
        // sub-rotina sem parâmetros próprios lê/escreve uma variável
        // global declarada antes dela no texto, sem precisar passá-la
        // como parâmetro nem declará-la localmente.
        let saida = executar(
            r#"programa P
var
  A : inteiro

  procedimento PONUM()
  início
    A <- 42
  fim

início
  PONUM()
  escreva A
fim"#,
            &[],
        );
        assert_eq!(saida, "42");
    }

    #[test]
    fn subrotina_nao_ve_variavel_global_declarada_depois_dela() {
        // X é declarada antes de A (A vê X); Y só é declarada depois de
        // A (A NÃO vê Y) — visibilidade estritamente por ordem textual,
        // não "todo o nível de topo de uma vez".
        let tokens = crate::lexer::tokenizar(
            r#"programa P
var
  X : inteiro

  procedimento A()
  início
    escreva Y
  fim

var
  Y : inteiro

início
  A()
fim"#,
        )
        .unwrap();
        let programa = crate::parser::parsear(tokens).unwrap();
        let mut console = ConsoleMemoria::default();
        let erro = interpretar(&programa, &mut console).unwrap_err();
        assert!(erro.mensagem.contains("'Y' não foi declarado"));
    }

    #[test]
    fn subrotina_declarada_depois_ve_todas_as_globais_anteriores() {
        // B é declarada depois de X e de Y — vê as duas.
        let saida = executar(
            r#"programa P
var
  X : inteiro

  procedimento A()
  início
    escreva X
  fim

var
  Y : inteiro

  procedimento B()
  início
    escreva X
    escreva Y
  fim

início
  X <- 1
  Y <- 2
  A()
  B()
fim"#,
            &[],
        );
        assert_eq!(saida, "112");
    }

    #[test]
    fn subrotina_aninhada_ve_variavel_local_da_externa_declarada_antes() {
        // Aninhamento pleno (seção 9.6): INTERNO, declarada dentro de
        // EXTERNO, vê a variável local I de EXTERNO (declarada antes de
        // INTERNO no texto), e consegue mutá-la — mudança visível de
        // volta em EXTERNO após a chamada (mesma célula compartilhada).
        let saida = executar(
            r#"programa P
  procedimento EXTERNO()
  var
    I : inteiro

    procedimento INTERNO()
    início
      I <- 99
    fim

  início
    I <- 1
    INTERNO()
    escreva I
  fim

início
  EXTERNO()
fim"#,
            &[],
        );
        assert_eq!(saida, "99");
    }

    #[test]
    fn conjunto_passado_como_argumento_e_referencia_implicita() {
        // Conjuntos são passados por referência implícita (sem precisar de
        // 'ref') — mutação dentro da sub-rotina é visível no chamador.
        let saida = executar(
            r#"programa P
  procedimento DOBRA(V : conjunto [1..3] de inteiro)
  var
    I : inteiro
  início
    para I de 1 até 3 passo 1 faça
      V[I] <- V[I] * 2
    fim_para
  fim
var
  A : conjunto [1..3] de inteiro
início
  A[1] <- 1
  A[2] <- 2
  A[3] <- 3
  DOBRA(A)
  escreva A[1]
  escreva A[2]
  escreva A[3]
fim"#,
            &[],
        );
        assert_eq!(saida, "246");
    }

    #[test]
    fn literal_caractere_atribuido_a_variavel_caractere_sem_cast() {
        // 'S' (aspas simples) já é 'caractere' — atribuição direta, sem
        // precisar de caractere(...) explícito (seção 3, diferente de um
        // literal "S" entre aspas duplas, que é sempre 'cadeia').
        let saida = executar(
            r#"programa P
var
  RESP : caractere
início
  RESP <- 'S'
  enquanto (RESP = 'S') faça
    escreva "ok"
    RESP <- 'N'
  fim_enquanto
fim"#,
            &[],
        );
        assert_eq!(saida, "ok");
    }
}
