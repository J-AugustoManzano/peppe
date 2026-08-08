//! Definição dos tokens da linguagem PEPPE.
//!
//! Cada `Token` carrega seu `TokenKind` e a posição (linha/coluna, 1-based)
//! onde o token começa no código-fonte.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ---- Literais -------------------------------------------------------
    /// Literal inteiro (ex.: `42`)
    Inteiro(i64),
    /// Literal real (ex.: `3.14159265`)
    Real(f64),
    /// Literal de `cadeia` entre aspas duplas (ex.: `"texto"`)
    Texto(String),
    /// Literal de `caractere` entre aspas simples (ex.: `'S'`) — exatamente
    /// um caractere, nunca zero nem mais de um (seção 3).
    Caractere(char),
    /// Literal lógico: `.v.` `.f.` `.verdadeiro.` `.falso.` (seção 3)
    Logico(bool),
    /// Identificador de usuário (variável, tipo, sub-rotina, etc.)
    Identificador(String),

    // ---- Estrutura do programa (seção 1) ---------------------------------
    Programa,
    Const,
    Tipo,
    Var,
    Ref,
    Vlr,
    Objeto,
    Inicio,
    Fim,

    // ---- Estruturas condicionais (seção 7) --------------------------------
    Se,
    Entao,
    Senao,
    FimSe,
    ExcetoSe,
    FimExcetoSe,
    Caso,
    Seja,
    Faca,
    FimCaso,

    // ---- Estruturas de repetição (seção 8) --------------------------------
    Enquanto,
    FimEnquanto,
    AteSeja,
    Efetue,
    FimAteSeja,
    Repita,
    AteQue,
    Execute,
    EnquantoFor,
    Laco,
    SaiaCaso,
    FimLaco,
    Interrompa,
    Continue,
    Para,
    De,
    Ate,
    Passo,
    FimPara,
    IrPara,

    // ---- Entrada/Saída (seção 6) -------------------------------------------
    Leia,
    Escreva,
    EscrevaLn,
    LeiaSeco,
    Pausa,

    // ---- Comandos de console — estilo CONIO (seção 6.3) --------------------
    Limpar,
    LimparLinha,
    Posicionar,
    CorFundo,
    CorFrente,

    // ---- Estruturas de dados (seção 4.4/4.5) -------------------------------
    Registro,
    FimRegistro,
    Conjunto,
    Dimensione,

    // ---- Sub-rotinas (seção 9) ---------------------------------------------
    Procedimento,
    Funcao,

    // ---- Programação Orientada a Objetos (seção 10) ------------------------
    Classe,
    FimClasse,
    Heranca,
    Virtual,
    Sobrepor,
    SecaoPublica,
    SecaoProtegida,
    SecaoPrivada,
    Este,

    // ---- Tipos primitivos (seção 3) ----------------------------------------
    TipoInteiro,
    TipoReal,
    TipoCadeia,
    TipoCaractere,
    TipoLogico,
    Generico,

    // ---- Operadores aritméticos (seção 5.2) --------------------------------
    /// `+` (adição ou concatenação de cadeia, seção 10.5.2)
    Mais,
    /// `-` ou `–` (en-dash, sinônimo aceito — seção 5.2)
    Menos,
    /// `*`
    Asterisco,
    /// `/`
    Barra,
    /// `div` — divisão inteira
    Div,
    /// `mod` — resto da divisão
    Mod,
    /// `↑` ou `^` (sinônimo aceito — seção 5.2)
    Potencia,

    // ---- Operadores lógicos (seção 5.4) ------------------------------------
    /// `.e.`
    E,
    /// `.ou.`
    Ou,
    /// `.não.` ou `.nao.` (sinônimo sem acento aceito)
    Nao,
    /// `.xou.`
    Xou,

    // ---- Operadores relacionais (seção 5.3) --------------------------------
    Igual,
    Diferente,
    Menor,
    Maior,
    MenorIgual,
    MaiorIgual,

    // ---- Atribuição (seção 5.1) --------------------------------------------
    /// `←` ou `<-` (sinônimo aceito)
    Seta,

    // ---- Pontuação ----------------------------------------------------------
    AbreParen,
    FechaParen,
    AbreColchete,
    FechaColchete,
    Virgula,
    /// `:` — separador de tipo, especificador de formatação (seção 6.2.1)
    DoisPontos,
    /// `.` — acesso a campo (`REGISTRO.CAMPO`)
    Ponto,
    /// `..` — intervalo (`[1..10]`) ou resolução de escopo (`Classe..Método`)
    PontoPonto,

    /// Fim do arquivo de origem
    FimDeArquivo,
}

impl fmt::Display for TokenKind {
    /// Mostra cada token na grafia real da PEPPE (ex.: `início`, `então`,
    /// `.não.`, `←`), usado nas mensagens de erro do parser (seção 15.3),
    /// para que "esperava 'fim_se'" seja legível por um aluno, não
    /// "esperava 'FimSe'".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TokenKind::*;
        match self {
            // Literais
            Inteiro(n) => write!(f, "{n}"),
            Real(n) => write!(f, "{n}"),
            Texto(s) => write!(f, "\"{s}\""),
            Caractere(c) => write!(f, "'{c}'"),
            Logico(true) => write!(f, ".verdadeiro."),
            Logico(false) => write!(f, ".falso."),
            Identificador(s) => write!(f, "{s}"),

            // Estrutura do programa
            Programa => write!(f, "programa"),
            Const => write!(f, "const"),
            Tipo => write!(f, "tipo"),
            Var => write!(f, "var"),
            Ref => write!(f, "ref"),
            Vlr => write!(f, "vlr"),
            Objeto => write!(f, "objeto"),
            Inicio => write!(f, "início"),
            Fim => write!(f, "fim"),

            // Condicionais
            Se => write!(f, "se"),
            Entao => write!(f, "então"),
            Senao => write!(f, "senão"),
            FimSe => write!(f, "fim_se"),
            ExcetoSe => write!(f, "exceto_se"),
            FimExcetoSe => write!(f, "fim_exceto_se"),
            Caso => write!(f, "caso"),
            Seja => write!(f, "seja"),
            Faca => write!(f, "faça"),
            FimCaso => write!(f, "fim_caso"),

            // Laços
            Enquanto => write!(f, "enquanto"),
            FimEnquanto => write!(f, "fim_enquanto"),
            AteSeja => write!(f, "até_seja"),
            Efetue => write!(f, "efetue"),
            FimAteSeja => write!(f, "fim_até_seja"),
            Repita => write!(f, "repita"),
            AteQue => write!(f, "até_que"),
            Execute => write!(f, "execute"),
            EnquantoFor => write!(f, "enquanto_for"),
            Laco => write!(f, "laço"),
            SaiaCaso => write!(f, "saia_caso"),
            FimLaco => write!(f, "fim_laço"),
            Interrompa => write!(f, "interrompa"),
            Continue => write!(f, "continue"),
            Para => write!(f, "para"),
            De => write!(f, "de"),
            Ate => write!(f, "até"),
            Passo => write!(f, "passo"),
            FimPara => write!(f, "fim_para"),
            IrPara => write!(f, "ir_para"),

            // E/S
            Leia => write!(f, "leia"),
            Escreva => write!(f, "escreva"),
            EscrevaLn => write!(f, "escreva_ln"),
            LeiaSeco => write!(f, "leia_seco"),
            Pausa => write!(f, "pausa"),

            // CONIO
            Limpar => write!(f, "limpar"),
            LimparLinha => write!(f, "limpar_linha"),
            Posicionar => write!(f, "posicionar"),
            CorFundo => write!(f, "cor_fundo"),
            CorFrente => write!(f, "cor_frente"),

            // Estruturas de dados
            Registro => write!(f, "registro"),
            FimRegistro => write!(f, "fim_registro"),
            Conjunto => write!(f, "conjunto"),
            Dimensione => write!(f, "dimensione"),

            // Sub-rotinas
            Procedimento => write!(f, "procedimento"),
            Funcao => write!(f, "função"),

            // OOP
            Classe => write!(f, "classe"),
            FimClasse => write!(f, "fim_classe"),
            Heranca => write!(f, "herança"),
            Virtual => write!(f, "virtual"),
            Sobrepor => write!(f, "sobrepor"),
            SecaoPublica => write!(f, "seção_pública"),
            SecaoProtegida => write!(f, "seção_protegida"),
            SecaoPrivada => write!(f, "seção_privada"),
            Este => write!(f, "este"),

            // Tipos primitivos
            TipoInteiro => write!(f, "inteiro"),
            TipoReal => write!(f, "real"),
            TipoCadeia => write!(f, "cadeia"),
            TipoCaractere => write!(f, "caractere"),
            TipoLogico => write!(f, "lógico"),
            Generico => write!(f, "generico"),

            // Operadores aritméticos
            Mais => write!(f, "+"),
            Menos => write!(f, "-"),
            Asterisco => write!(f, "*"),
            Barra => write!(f, "/"),
            Div => write!(f, "div"),
            Mod => write!(f, "mod"),
            Potencia => write!(f, "^"),

            // Operadores lógicos
            E => write!(f, ".e."),
            Ou => write!(f, ".ou."),
            Nao => write!(f, ".não."),
            Xou => write!(f, ".xou."),

            // Operadores relacionais
            Igual => write!(f, "="),
            Diferente => write!(f, "<>"),
            Menor => write!(f, "<"),
            Maior => write!(f, ">"),
            MenorIgual => write!(f, "<="),
            MaiorIgual => write!(f, ">="),
            Seta => write!(f, "<-"),

            // Pontuação
            AbreParen => write!(f, "("),
            FechaParen => write!(f, ")"),
            AbreColchete => write!(f, "["),
            FechaColchete => write!(f, "]"),
            Virgula => write!(f, ","),
            DoisPontos => write!(f, ":"),
            Ponto => write!(f, "."),
            PontoPonto => write!(f, ".."),

            FimDeArquivo => write!(f, "<fim de arquivo>"),
        }
    }
}

/// Um token reconhecido pelo lexer, com sua posição de origem (1-based).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub linha: usize,
    pub coluna: usize,
}

impl Token {
    pub fn new(kind: TokenKind, linha: usize, coluna: usize) -> Self {
        Token { kind, linha, coluna }
    }
}
