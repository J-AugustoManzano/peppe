//! Árvore Sintática Abstrata (AST) da linguagem PEPPE — núcleo estrutural
//! (seções 1–9 da especificação). Programação Orientada a Objetos (seção 10)
//! fica para uma fase posterior (`ast_oop`, futuramente).
//!
//! Convenções:
//! - Todo nó que pode originar uma mensagem de erro carrega `linha: usize`
//!   (1-based), para o formato de diagnóstico da seção 15.3.
//! - `Bloco` é simplesmente uma sequência de [`Comando`]s.
//! - Declarações de nível superior (`const`/`tipo`/`var`/sub-rotinas) podem
//!   aparecer intercaladas e em qualquer ordem (seção 1.1) — por isso
//!   [`DeclaracaoTopo`] é um enum, e [`Programa`]/[`SubRotina`] guardam
//!   `Vec<DeclaracaoTopo>`.

// =====================================================================================
// Programa
// =====================================================================================

/// Um programa PEPPE completo: `programa <NOME> ... início ... fim` (seção 1.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Programa {
    pub nome: String,
    /// Declarações de nível superior (`const`, `tipo`, `var`, sub-rotinas),
    /// na ordem em que aparecem no código-fonte.
    pub declaracoes: Vec<DeclaracaoTopo>,
    pub bloco_principal: Bloco,
}

/// Uma declaração de nível superior — de um programa ou do corpo de uma
/// sub-rotina (seção 9.6, sub-rotinas aninhadas).
#[derive(Debug, Clone, PartialEq)]
pub enum DeclaracaoTopo {
    Const(DeclaracaoConst),
    Tipo(DeclaracaoTipo),
    Var(DeclaracaoVar),
    SubRotina(SubRotina),
    /// `função <Classe>..<MÉTODO>(...) [: tipo] ... início ... fim`, ou a
    /// forma `procedimento` equivalente (seção 10.3, implementação
    /// externa). A assinatura precisa corresponder a uma
    /// `ItemClasse::AssinaturaMetodo` dentro da declaração de `<Classe>`
    /// (validado pelo verificador semântico).
    MetodoExterno { classe: String, metodo: SubRotina },
}

// =====================================================================================
// Declarações (seção 4)
// =====================================================================================

/// `const NOME = <literal>` (seção 4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaracaoConst {
    pub nome: String,
    /// Sempre um literal (seção 4.1) — `Expr::Inteiro`, `Expr::Real`,
    /// `Expr::Texto`, `Expr::Caractere` ou `Expr::Logico`.
    pub valor: Expr,
    pub linha: usize,
}

/// `tipo NOME = <definição>` (seção 4.3/4.4/4.5).
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaracaoTipo {
    pub nome: String,
    pub definicao: Tipo,
    pub linha: usize,
}

/// `var NOME1, NOME2, ... : <tipo>` (seção 4.2). Uma linha de `var` com
/// vários nomes do mesmo tipo é representada por **uma** `DeclaracaoVar`
/// com `nomes.len() > 1`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaracaoVar {
    pub nomes: Vec<String>,
    pub tipo: Tipo,
    pub linha: usize,
}

/// Um tipo PEPPE — primitivo, alias (`tipo`), `registro` ou `conjunto`
/// (seções 3/4.3/4.4/4.5).
#[derive(Debug, Clone, PartialEq)]
pub enum Tipo {
    Primitivo(TipoPrimitivo),
    /// Tipo `generico` — polimorfismo paramétrico (seção 10.5, fase 2).
    Generico,
    /// Referência a um tipo definido via `tipo NOME = ...` (alias, registro,
    /// conjunto ou — fase 2 — classe). Resolvido pelo verificador semântico.
    Nomeado(String),
    /// `registro <campos> fim_registro` (seção 4.4).
    Registro(Vec<DeclaracaoVar>),
    /// `conjunto [<dim1>, <dim2>, ...] de <tipo>` (seção 4.5).
    ///
    /// Cada dimensão é `Some((inicio, fim))` para um array estático
    /// (`[1..8]`) ou `None` para a dimensão vazia de um array dinâmico
    /// (`conjunto [] de cadeia`, seção 4.5.1).
    Conjunto {
        dimensoes: Vec<Option<(Expr, Expr)>>,
        elemento: Box<Tipo>,
    },
    /// `classe [herança de <ClasseBase1>[, de <ClasseBase2>, ...]]
    /// <seções de membros> fim_classe` (seção 10.1). Herança múltipla
    /// (Fase 6) é suportada — `heranca` é a lista de bases **diretas**,
    /// na ordem declarada (vazia = sem herança, um elemento = herança
    /// simples, dois ou mais = múltipla). PEPPE não tem herança virtual
    /// (decisão do autor): se duas bases diretas compartilham, por sua
    /// vez, uma base comum mais acima ("diamond problem"), cada caminho
    /// de herança duplica essa base — igual C++ sem a palavra-chave
    /// `virtual`. Colisão de nome entre bases (ou herdado por mais de
    /// um caminho) é erro de ambiguidade ao acessar sem qualificação;
    /// desambigua-se com `CLS_BASE..NOME` (mesmo operador `..` usado em
    /// método externo, seção 10.3) — ver `Verificador::achatar_classe`.
    Classe {
        heranca: Vec<String>,
        membros: Vec<MembroClasse>,
    },
    /// `função(tipo1, tipo2, ...)` (seção 10.5.3) — tipo de uma
    /// referência a função de primeira classe. Só fixa os tipos dos
    /// **parâmetros** (`parametros`); o tipo de retorno é livre — uma
    /// variável deste tipo aceita qualquer função cujos parâmetros
    /// tenham esses tipos, na ordem, com qualquer retorno. Só aceita
    /// **funções** (sub-rotinas com retorno), nunca procedimentos —
    /// validado pelo verificador semântico, não na própria AST.
    /// Sempre usado através de um alias nomeado (`tipo FUNC1 =
    /// função(inteiro)`), nunca como tipo anônimo numa declaração de
    /// `var` — mesma convenção de `classe`/`registro`.
    Funcao {
        parametros: Vec<Tipo>,
    },
}

/// Os cinco tipos primitivos da PEPPE (seção 3), também usados como destino
/// de *cast* (seção 10.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipoPrimitivo {
    Inteiro,
    Real,
    Cadeia,
    Caractere,
    Logico,
}

// =====================================================================================
// Classes (seção 10) — declaração de membros
// =====================================================================================

/// Um membro de classe, com sua seção de visibilidade (seção 10.1/10.4).
#[derive(Debug, Clone, PartialEq)]
pub struct MembroClasse {
    pub visibilidade: Visibilidade,
    pub item: ItemClasse,
}

/// `seção_pública` / `seção_protegida` / `seção_privada` (seção 10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibilidade {
    Publica,
    Protegida,
    Privada,
}

/// Modificador de dispatch de um método (seção 10.6) — `virtual` na
/// classe-base habilita sobrescrita; `sobrepor` na classe derivada
/// redefine um método `virtual` correspondente. `Nenhum` é o padrão
/// (*binding* estático, resolvido pelo tipo declarado da variável, não
/// pela classe real da instância).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Modificador {
    #[default]
    Nenhum,
    Virtual,
    Sobrepor,
}

/// O que pode aparecer dentro de uma seção de visibilidade de uma classe
/// (seção 10.1).
#[derive(Debug, Clone, PartialEq)]
pub enum ItemClasse {
    /// Um campo de dados — `NOME1, NOME2, ... : <tipo>` (mesma forma de
    /// `var`/campo de `registro`).
    Campo(DeclaracaoVar),
    /// Assinatura de método **sem** corpo aqui — a implementação aparece
    /// em outro lugar: como `MetodoInterno` em outra seção da mesma
    /// classe, ou como [`DeclaracaoTopo::MetodoExterno`] fora da classe
    /// (seção 10.3). É erro semântico se a assinatura nunca for
    /// implementada em nenhum dos dois lugares.
    AssinaturaMetodo {
        categoria: CategoriaSubRotina,
        nome: String,
        parametros: Vec<Parametro>,
        tipo_retorno: Option<Tipo>,
        modificador: Modificador,
        linha: usize,
    },
    /// Implementação completa de um método **dentro** da declaração da
    /// classe (seção 10.3, forma "interna").
    MetodoInterno(SubRotina, Modificador),
}

// =====================================================================================
// Sub-rotinas (seção 9)
// =====================================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoriaSubRotina {
    Procedimento,
    Funcao,
}

/// `procedimento NOME(...) ... início ... fim` ou
/// `função NOME(...) : <tipo> ... início ... fim` (seção 9.1/9.2).
///
/// Sub-rotinas aninhadas (seção 9.6) aparecem dentro de
/// `declaracoes_locais` de sua sub-rotina "pai", como mais um
/// [`DeclaracaoTopo::SubRotina`].
#[derive(Debug, Clone, PartialEq)]
pub struct SubRotina {
    pub categoria: CategoriaSubRotina,
    pub nome: String,
    pub parametros: Vec<Parametro>,
    /// `Some(tipo)` apenas para `função`; `None` para `procedimento`.
    pub tipo_retorno: Option<Tipo>,
    /// `const`/`tipo`/`var`/sub-rotinas locais, na ordem declarada.
    pub declaracoes_locais: Vec<DeclaracaoTopo>,
    pub corpo: Bloco,
    pub linha: usize,
}

/// Um grupo de parâmetros: `[var] NOME1, NOME2, ... : <tipo>` — separado de
/// outros grupos por `;` (Padrão A, seção 9.3).
#[derive(Debug, Clone, PartialEq)]
pub struct Parametro {
    pub nomes: Vec<String>,
    pub tipo: Tipo,
    /// `true` se o grupo foi declarado com `var` (passagem por referência).
    pub por_referencia: bool,
}

// =====================================================================================
// Comandos (seções 6, 7, 8 e 9.7)
// =====================================================================================

pub type Bloco = Vec<Comando>;

#[derive(Debug, Clone, PartialEq)]
pub enum Comando {
    /// `<lvalue> ← <expr>` (seção 5.1).
    Atribuicao { destino: LValue, valor: Expr, linha: usize },

    /// `leia <lvalue> {, <lvalue>}` (seção 6.1).
    Leia { variaveis: Vec<LValue>, linha: usize },

    /// `leia_seco <lvalue>` — leitura sem eco (seção 6.3).
    LeiaSeco { variavel: LValue, linha: usize },

    /// `escreva <item> {, <item>}` (seção 6.2/6.2.1), ou `escreva_ln
    /// <item> {, <item>}` (seção 6.2.2) quando `quebra_linha` é `true` —
    /// mesma sintaxe e especificadores de formatação (`:largura:decimais`)
    /// de `escreva`, mas com um `\n` automático ao final de toda a lista
    /// (estilo Pascal `writeln`). `escreva_ln` sem nenhum item é válido e
    /// imprime apenas a quebra de linha.
    Escreva { itens: Vec<ItemEscreva>, quebra_linha: bool, linha: usize },

    /// `pausa` — interrompe a execução até o usuário pressionar `<Enter>`
    /// (seção 6.4). Lê e descarta uma linha de `stdin`, sem armazenar o
    /// valor em nenhuma variável.
    Pausa { linha: usize },

    /// `se (<cond>) então <bloco> [senão <bloco>] fim_se` (seção 7.1).
    Se {
        condicao: Expr,
        entao: Bloco,
        senao: Option<Bloco>,
        linha: usize,
    },

    /// `exceto_se (<cond>) então <bloco> [senão <bloco>] fim_exceto_se`
    /// (seção 7.2) — semântica de `se` com a condição invertida.
    ExcetoSe {
        condicao: Expr,
        entao: Bloco,
        senao: Option<Bloco>,
        linha: usize,
    },

    /// `caso <expr> { seja <valor> faça <bloco> } [senão <bloco>] fim_caso`
    /// (seção 7.3). `senão` é opcional (✅ v0.7).
    Caso {
        expressao: Expr,
        ramos: Vec<RamoCaso>,
        senao: Option<Bloco>,
        linha: usize,
    },

    /// `enquanto (<cond>) faça <bloco> fim_enquanto` — pré-teste, executa
    /// enquanto verdadeiro (seção 8).
    Enquanto { condicao: Expr, corpo: Bloco, linha: usize },

    /// `até_seja (<cond>) efetue <bloco> fim_até_seja` — pré-teste, executa
    /// enquanto falso (seção 8).
    AteSeja { condicao: Expr, corpo: Bloco, linha: usize },

    /// `repita <bloco> até_que (<cond>)` — pós-teste, executa enquanto falso
    /// (seção 8).
    Repita { corpo: Bloco, condicao: Expr, linha: usize },

    /// `execute <bloco> enquanto_for (<cond>)` — pós-teste, executa enquanto
    /// verdadeiro (seção 8).
    Execute { corpo: Bloco, condicao: Expr, linha: usize },

    /// `laço <bloco_com_saia> fim_laço` — laço indefinido (seção 8).
    /// `saia_caso`/`interrompa` aparecem como [`Comando::SaiaCaso`] /
    /// [`Comando::Interrompa`] dentro do `corpo`.
    Laco { corpo: Bloco, linha: usize },

    /// `para <var> de <ini> até <fim> [passo <passo>] faça <bloco> fim_para`
    /// (seção 8). `passo` ausente equivale a `1`.
    Para {
        variavel: String,
        inicio: Expr,
        fim: Expr,
        passo: Option<Expr>,
        corpo: Bloco,
        linha: usize,
    },

    /// `dimensione VAR[<ini1>..<fim1> {, <ini2>..<fim2>}]` (seção 4.5.1).
    Dimensione {
        variavel: String,
        dimensoes: Vec<(Expr, Expr)>,
        linha: usize,
    },

    /// Chamada de `procedimento` como comando (seção 9.7):
    /// `NOME` ou `NOME(arg1, arg2, ...)`.
    ChamadaProcedimento {
        nome: String,
        argumentos: Vec<Expr>,
        linha: usize,
    },

    /// Chamada de método como comando, ignorando o valor de retorno se
    /// houver (seção 10.4): `OBJETO.MÉTODO()`, `OBJETO.MÉTODO(args)`. O
    /// último acesso de `alvo` é sempre `Acesso::Metodo` (garantido pelo
    /// parser) — `alvo` é um `LValue` completo (não só um nome) para
    /// suportar receptores encadeados (ex.: `MATRIZ[I].MÉTODO()`).
    ChamadaMetodo { alvo: LValue, linha: usize },

    /// `RÓTULO:` (seção 8, desvio incondicional).
    Rotulo { nome: String, linha: usize },

    /// `ir_para RÓTULO` (seção 8).
    IrPara { rotulo: String, linha: usize },

    /// `interrompa` — *break* universal (seção 8, ✅ v0.3).
    Interrompa { linha: usize },
    /// `continue` — pula para a próxima iteração do laço mais interno
    /// (equivalente ao `continue` do C/Pascal). Só válido dentro de laço
    /// (`enquanto`, `repita`, `execute`, `para`, `laço`).
    Continue { linha: usize },

    /// `saia_caso (<cond>)` — específico do `laço` (seção 8); equivalente a
    /// `se (<cond>) então interrompa fim_se`.
    SaiaCaso { condicao: Expr, linha: usize },

    // -- Comandos de console — estilo CONIO (seção 6.3) --------------------------
    /// `limpar`.
    Limpar { linha: usize },
    /// `limpar_linha` ou `limpar_linha(<col>)`.
    LimparLinha { coluna: Option<Expr>, linha: usize },
    /// `posicionar(<col>, <lin>)`.
    Posicionar { coluna: Expr, linha_destino: Expr, linha: usize },
    /// `cor_fundo(<n>)`.
    CorFundo { cor: Expr, linha: usize },
    /// `cor_frente(<n>)`.
    CorFrente { cor: Expr, linha: usize },
}

/// Um ramo `seja <valor> faça <bloco>` de um `caso` (seção 7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct RamoCaso {
    /// Sempre um literal (seção 7.3): inteiro, texto, etc.
    pub valor: Expr,
    pub corpo: Bloco,
    pub linha: usize,
}

/// Um item de `escreva`, com especificador de formatação opcional
/// `[: <largura> [: <decimais>]]` (seção 6.2.1).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemEscreva {
    pub expressao: Expr,
    pub largura: Option<Expr>,
    /// Só tem sentido quando `expressao` é `real`; ignorado/erro semântico
    /// para outros tipos (seção 6.2.1).
    pub decimais: Option<Expr>,
}

// =====================================================================================
// L-values: variáveis, campos de registro e elementos de conjunto
// =====================================================================================

/// Um "lugar" que pode receber uma atribuição ou ser lido por `leia`:
/// `NOME`, `NOME.CAMPO`, `NOME[i]`, `NOME[i,j]`, `ALUNO[I].NOTAS[J]`, etc.
/// (seção 1.2 / EBNF `lvalue`).
#[derive(Debug, Clone, PartialEq)]
pub struct LValue {
    /// Qualificação de escopo opcional (`CLS_BASE..NOME...`, Fase 6 —
    /// seção 10.1/10.6.1): desambigua um campo/método de `nome` que
    /// existe em mais de uma classe-base direta (herança múltipla sem
    /// `virtual`), indicando explicitamente a partir de qual base
    /// resolver o **primeiro** acesso da cadeia — acessos subsequentes
    /// (`.CAMPO`, `.MÉTODO()`) continuam resolvendo normalmente a
    /// partir do tipo resultante. Reaproveita o mesmo operador `..` já
    /// usado para método externo (seção 10.3), mas em posição de
    /// expressão/lvalue em vez de declaração de topo.
    pub qualificador_base: Option<String>,
    pub nome: String,
    /// Sequência de acessos aplicados a `nome`, na ordem em que aparecem.
    pub acessos: Vec<Acesso>,
    pub linha: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Acesso {
    /// `.CAMPO` — acesso a campo de `registro` (seção 4.4), ou a um campo
    /// de instância de `classe` (seção 10.4).
    Campo(String),
    /// `[i]` ou `[i, j]` — acesso a elemento de `conjunto` (seção 4.5).
    Indice(Vec<Expr>),
    /// `.MÉTODO(args)` — chamada de método sobre uma instância de classe
    /// (seção 10.4). Sempre o **último** acesso de uma cadeia (não há
    /// `.CAMPO` ou `.MÉTODO()` válido depois de uma chamada de método —
    /// validado pelo verificador semântico, já que o valor de retorno não
    /// é um "lugar" encadeável). Usado tanto como comando solto
    /// (`ESTUDANTE.CALCMÉDIA()`, ignorando o retorno) quanto dentro de uma
    /// expressão (`escreva ESTUDANTE.PEGANOME()`).
    Metodo { nome: String, argumentos: Vec<Expr> },
}

// =====================================================================================
// Expressões (seção 5)
// =====================================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Inteiro(i64),
    Real(f64),
    Texto(String),
    /// Literal `caractere` entre aspas simples (ex.: `'S'`, seção 3) —
    /// exatamente um símbolo, sempre tipado como `Caractere`, nunca
    /// `Cadeia` (diferente de `Expr::Texto`, que é sempre `Cadeia` mesmo
    /// com um único caractere dentro das aspas duplas).
    Caractere(char),
    Logico(bool),

    /// Referência a uma variável (possivelmente com acessos a
    /// campo/índice) — também usada para identificadores pré-definidos
    /// (`p_pi`, `p_euler`, `p_infinito`, seção 5.6) antes da resolução
    /// semântica.
    Variavel(LValue),

    /// Chamada de função ou identificador pré-definido com argumentos:
    /// `NOME(arg1, arg2, ...)` — inclui funções matemáticas embutidas
    /// (seção 5.6) e chamadas de `função` do usuário.
    Chamada {
        nome: String,
        argumentos: Vec<Expr>,
        linha: usize,
    },

    /// Operação binária — aritmética, relacional ou lógica (seções
    /// 5.2/5.3/5.4), incluindo concatenação de `cadeia` com `+`
    /// (seção 10.5.2).
    Binaria {
        op: OpBinario,
        esquerda: Box<Expr>,
        direita: Box<Expr>,
        linha: usize,
    },

    /// Operação unária: `-<expr>` ou `.não. <expr>` (seção 5.5).
    Unaria { op: OpUnario, expr: Box<Expr>, linha: usize },

    /// *Cast* explícito — estilo função (`inteiro(X)`) ou estilo C
    /// (`(inteiro) X`), ambos equivalentes (seção 10.5.1).
    Cast {
        tipo: TipoPrimitivo,
        expr: Box<Expr>,
        linha: usize,
    },
}

/// Operadores binários (seções 5.2/5.3/5.4), em uma única enumeração — a
/// precedência (seção 5.5) é responsabilidade do *parser*, não da AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpBinario {
    // Aritméticos (seção 5.2)
    /// `+` — soma (`inteiro`/`real`) ou concatenação (`cadeia`, seção 10.5.2)
    Soma,
    /// `-` ou `–` (en-dash, sinônimo aceito pelo lexer)
    Subtracao,
    Multiplicacao,
    /// `/` — divisão real (seção 5.2)
    Divisao,
    /// `div` — divisão inteira
    Div,
    /// `mod` — resto da divisão
    Mod,
    /// `^` ou `↑` — potenciação, associativa à direita (seção 5.2/5.5)
    Potencia,

    // Relacionais (seção 5.3)
    Igual,
    Diferente,
    Menor,
    Maior,
    MenorIgual,
    MaiorIgual,

    // Lógicos (seção 5.4)
    /// `.e.`
    E,
    /// `.ou.`
    Ou,
    /// `.xou.`
    Xou,
}

/// Operadores unários (seção 5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpUnario {
    /// `-<expr>` — negação aritmética
    Negativo,
    /// `.não./.nao. <expr>` — negação lógica
    Nao,
}

// =====================================================================================
// Testes de "fumaça" — construção manual da AST de ADIÇÃO_NÚMEROS
// =====================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// Constrói manualmente a AST de:
    /// ```text
    /// programa ADIÇÃO_NÚMEROS
    /// var
    ///   X, A, B : inteiro
    /// início
    ///   leia A
    ///   leia B
    ///   X ← A + B
    ///   escreva X
    /// fim
    /// ```
    /// Não testa o *parser* (ainda não existe) — apenas garante que a AST
    /// consegue representar este programa real do livro, e serve de
    /// referência de uso para quem for escrever o parser a seguir.
    #[test]
    fn ast_adicao_numeros() {
        let lv = |nome: &str, linha: usize| LValue {
            qualificador_base: None,
            nome: nome.to_string(),
            acessos: vec![],
            linha,
        };

        let programa = Programa {
            nome: "ADIÇÃO_NÚMEROS".to_string(),
            declaracoes: vec![DeclaracaoTopo::Var(DeclaracaoVar {
                nomes: vec!["X".into(), "A".into(), "B".into()],
                tipo: Tipo::Primitivo(TipoPrimitivo::Inteiro),
                linha: 3,
            })],
            bloco_principal: vec![
                Comando::Leia { variaveis: vec![lv("A", 5)], linha: 5 },
                Comando::Leia { variaveis: vec![lv("B", 6)], linha: 6 },
                Comando::Atribuicao {
                    destino: lv("X", 7),
                    valor: Expr::Binaria {
                        op: OpBinario::Soma,
                        esquerda: Box::new(Expr::Variavel(lv("A", 7))),
                        direita: Box::new(Expr::Variavel(lv("B", 7))),
                        linha: 7,
                    },
                    linha: 7,
                },
                Comando::Escreva {
                    itens: vec![ItemEscreva {
                        expressao: Expr::Variavel(lv("X", 8)),
                        largura: None,
                        decimais: None,
                    }],
                    quebra_linha: false,
                    linha: 8,
                },
            ],
        };

        assert_eq!(programa.nome, "ADIÇÃO_NÚMEROS");
        assert_eq!(programa.bloco_principal.len(), 4);
        match &programa.declaracoes[0] {
            DeclaracaoTopo::Var(d) => assert_eq!(d.nomes.len(), 3),
            _ => panic!("esperava DeclaracaoTopo::Var"),
        }
    }

    /// AST de `R ← inteiro(A) ↑ (1 / 2)` — garante que `Cast`, operadores
    /// binários e aninhamento via `Box<Expr>` funcionam.
    #[test]
    fn ast_expressao_com_potencia_e_cast() {
        let expr = Expr::Binaria {
            op: OpBinario::Potencia,
            esquerda: Box::new(Expr::Cast {
                tipo: TipoPrimitivo::Inteiro,
                expr: Box::new(Expr::Variavel(LValue {
                    qualificador_base: None,
                    nome: "A".into(),
                    acessos: vec![],
                    linha: 1,
                })),
                linha: 1,
            }),
            direita: Box::new(Expr::Binaria {
                op: OpBinario::Divisao,
                esquerda: Box::new(Expr::Inteiro(1)),
                direita: Box::new(Expr::Inteiro(2)),
                linha: 1,
            }),
            linha: 1,
        };

        let Expr::Binaria { op, esquerda, direita, .. } = &expr else {
            panic!("esperava Expr::Binaria");
        };
        assert_eq!(*op, OpBinario::Potencia);

        let Expr::Cast { tipo, .. } = esquerda.as_ref() else {
            panic!("esperava Expr::Cast no lado esquerdo");
        };
        assert_eq!(*tipo, TipoPrimitivo::Inteiro);

        let Expr::Binaria { op: op2, .. } = direita.as_ref() else {
            panic!("esperava Expr::Binaria no lado direito");
        };
        assert_eq!(*op2, OpBinario::Divisao);
    }

    /// AST de um acesso encadeado `ALUNO[I].NOTAS[J]`.
    #[test]
    fn ast_acesso_encadeado() {
        let lv = LValue {
            qualificador_base: None,
            nome: "ALUNO".into(),
            acessos: vec![
                Acesso::Indice(vec![Expr::Variavel(LValue {
                    qualificador_base: None,
                    nome: "I".into(),
                    acessos: vec![],
                    linha: 1,
                })]),
                Acesso::Campo("NOTAS".into()),
                Acesso::Indice(vec![Expr::Variavel(LValue {
                    qualificador_base: None,
                    nome: "J".into(),
                    acessos: vec![],
                    linha: 1,
                })]),
            ],
            linha: 1,
        };
        assert_eq!(lv.acessos.len(), 3);
    }
}
