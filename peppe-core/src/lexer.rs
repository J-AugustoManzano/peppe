//! Lexer (analisador léxico) da linguagem PEPPE.
//!
//! Responsável por transformar o código-fonte (UTF-8) em uma sequência de
//! [`Token`]s. Implementa, em particular:
//!
//! - identificadores com acentuação (`início`, `não`, `ÁREA_CÍRCULO`...);
//! - sinônimos de operadores: `<-`/`←` (✅ v0.9: '<-' é a forma canônica),
//!   `^`/`↑` (idem, '^' canônico), `-`/`–` (en-dash, seção 5.2);
//! - sequências de escape em literais de texto: `\n`, `\t`, `\"`, `\\` (seção 3);
//! - literal `caractere` entre aspas simples (`'S'`), distinto de `cadeia`
//!   entre aspas duplas (`"S"`) mesmo com um único símbolo — seção 3;
//! - tokens "com pontos": `.e.` `.ou.` `.não./.nao.` `.xou.` `.v.` `.f.`
//!   `.verdadeiro.` `.falso.` (seção 3 / 5.4), distinguindo-os do acesso a
//!   campo (`.`) e do operador de intervalo/escopo (`..`);
//! - comentários de linha (`//`) e de bloco (`{ ... }`), sem aninhamento;
//! - `;` tratado como separador ignorável em qualquer posição (seção 1.4).
//!
//! Variantes de palavras-chave sem acentuação (ex.: `inicio`, `funcao`,
//! `nao`) são aceitas como sinônimos — conveniência para quem digita em
//! teclados/editores sem suporte fácil a acentos.

use crate::token::{Token, TokenKind};

/// Erro léxico, com posição (1-based) e mensagem didática em português,
/// seguindo o formato da seção 15.3 da especificação.
#[derive(Debug, Clone, PartialEq)]
pub struct ErroLexico {
    pub linha: usize,
    pub coluna: usize,
    pub mensagem: String,
}

impl std::fmt::Display for ErroLexico {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Erro de sintaxe, linha {}, coluna {}: {}",
            self.linha, self.coluna, self.mensagem
        )
    }
}

/// Tokeniza o código-fonte `fonte`, retornando a lista completa de tokens
/// (terminada por [`TokenKind::FimDeArquivo`]) ou o primeiro erro léxico
/// encontrado.
pub fn tokenizar(fonte: &str) -> Result<Vec<Token>, ErroLexico> {
    Lexer::new(fonte).tokenizar()
}

pub struct Lexer {
    /// Código-fonte como vetor de `char` — simplifica o acesso posicional
    /// preservando a semântica Unicode (cada `char` é um "scalar value").
    chars: Vec<char>,
    pos: usize,
    linha: usize,
    coluna: usize,
}

impl Lexer {
    pub fn new(fonte: &str) -> Self {
        Lexer {
            chars: fonte.chars().collect(),
            pos: 0,
            linha: 1,
            coluna: 1,
        }
    }

    // -- Utilitários de leitura ---------------------------------------------

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    /// Consome e retorna o caractere atual, atualizando linha/coluna.
    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.linha += 1;
                self.coluna = 1;
            } else {
                self.coluna += 1;
            }
        }
        c
    }

    fn erro(&self, mensagem: impl Into<String>) -> ErroLexico {
        ErroLexico {
            linha: self.linha,
            coluna: self.coluna,
            mensagem: mensagem.into(),
        }
    }

    fn erro_em(&self, linha: usize, coluna: usize, mensagem: impl Into<String>) -> ErroLexico {
        ErroLexico { linha, coluna, mensagem: mensagem.into() }
    }

    // -- Laço principal -------------------------------------------------------

    pub fn tokenizar(&mut self) -> Result<Vec<Token>, ErroLexico> {
        let mut tokens = Vec::new();

        loop {
            self.pular_espacos_e_comentarios()?;

            let linha = self.linha;
            let coluna = self.coluna;

            match self.peek() {
                None => {
                    tokens.push(Token::new(TokenKind::FimDeArquivo, linha, coluna));
                    break;
                }
                Some(c) => {
                    let kind = self.proximo_token(c)?;
                    tokens.push(Token::new(kind, linha, coluna));
                }
            }
        }

        Ok(tokens)
    }

    /// Avança sobre espaços em branco, `;` (separador ignorável — seção 1.4),
    /// comentários de linha (`//`) e comentários de bloco (`{ ... }`).
    fn pular_espacos_e_comentarios(&mut self) -> Result<(), ErroLexico> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() || c == ';' => {
                    self.advance();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                Some('{') => {
                    let linha_abertura = self.linha;
                    let coluna_abertura = self.coluna;
                    self.advance(); // consome '{'
                    loop {
                        match self.advance() {
                            Some('}') => break,
                            Some(_) => continue,
                            None => {
                                return Err(self.erro_em(
                                    linha_abertura,
                                    coluna_abertura,
                                    "comentário de bloco '{' nunca foi fechado com '}'",
                                ))
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Despacha para o leitor apropriado conforme o caractere atual.
    fn proximo_token(&mut self, c: char) -> Result<TokenKind, ErroLexico> {
        if c.is_alphabetic() || c == '_' {
            Ok(self.ler_identificador_ou_palavra_chave())
        } else if c.is_ascii_digit() {
            self.ler_numero()
        } else if c == '"' {
            self.ler_texto()
        } else if c == '\'' {
            self.ler_caractere()
        } else if c == '.' {
            self.ler_ponto()
        } else {
            self.ler_operador_ou_pontuacao(c)
        }
    }

    // -- Identificadores e palavras-chave --------------------------------------

    fn ler_identificador_ou_palavra_chave(&mut self) -> TokenKind {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        palavra_chave(&s).unwrap_or(TokenKind::Identificador(s))
    }

    // -- Números ---------------------------------------------------------------

    /// Lê um literal `inteiro` ou `real` (seção 3). Não trata sinal — números
    /// negativos são `-`/`–` (unário) aplicado a um literal, resolvido no parser
    /// (seção 5.5).
    fn ler_numero(&mut self) -> Result<TokenKind, ErroLexico> {
        let linha = self.linha;
        let coluna = self.coluna;

        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        // Parte decimal: '.' seguido de dígito (distingue de '..' — intervalo,
        // e de acesso a campo).
        if self.peek() == Some('.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            s.push('.');
            self.advance();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    self.advance();
                } else {
                    break;
                }
            }
            let valor: f64 = s
                .parse()
                .map_err(|_| self.erro_em(linha, coluna, format!("literal real inválido: '{s}'")))?;
            return Ok(TokenKind::Real(valor));
        }

        let valor: i64 = s.parse().map_err(|_| {
            self.erro_em(
                linha,
                coluna,
                format!(
                    "literal inteiro '{s}' é grande demais para o tipo 'inteiro' \
                     (máximo {})",
                    i64::MAX
                ),
            )
        })?;
        Ok(TokenKind::Inteiro(valor))
    }

    // -- Literais de texto -------------------------------------------------------

    /// Lê um literal `cadeia` entre aspas duplas, processando as
    /// sequências de escape `\n`, `\t`, `\"` e `\\` (seção 3).
    fn ler_texto(&mut self) -> Result<TokenKind, ErroLexico> {
        let linha_abertura = self.linha;
        let coluna_abertura = self.coluna;

        self.advance(); // consome a aspas de abertura
        let mut s = String::new();

        loop {
            match self.advance() {
                Some('"') => return Ok(TokenKind::Texto(s)),
                Some('\\') => match self.advance() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some(outro) => {
                        return Err(self.erro(format!(
                            "sequência de escape desconhecida '\\{outro}' — use \\n, \\t, \\\" ou \\\\"
                        )))
                    }
                    None => {
                        return Err(self.erro_em(
                            linha_abertura,
                            coluna_abertura,
                            "literal de texto não foi fechado com '\"'",
                        ))
                    }
                },
                Some(c) => s.push(c),
                None => {
                    return Err(self.erro_em(
                        linha_abertura,
                        coluna_abertura,
                        "literal de texto não foi fechado com '\"'",
                    ))
                }
            }
        }
    }

    /// Lê um literal `caractere` entre aspas simples (ex.: `'S'`), com os
    /// mesmos escapes de `Self::ler_texto` (`\n`, `\t`, `\'`, `\\`).
    /// Exige exatamente um caractere — `''` (vazio) e `'AB'` (mais de um)
    /// são erro léxico, já que `caractere` representa um único símbolo
    /// (seção 3); para um literal de texto de tamanho arbitrário (mesmo
    /// que tenha um caractere só), use aspas duplas.
    fn ler_caractere(&mut self) -> Result<TokenKind, ErroLexico> {
        let linha_abertura = self.linha;
        let coluna_abertura = self.coluna;

        self.advance(); // consome a aspas simples de abertura

        let conteudo = match self.advance() {
            Some('\'') => {
                return Err(self.erro_em(
                    linha_abertura,
                    coluna_abertura,
                    "literal de caractere vazio ('') — 'caractere' exige exatamente \
                     um símbolo; use \"\" (aspas duplas) para uma cadeia vazia",
                ))
            }
            Some('\\') => match self.advance() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some('\'') => '\'',
                Some('\\') => '\\',
                Some(outro) => {
                    return Err(self.erro(format!(
                        "sequência de escape desconhecida '\\{outro}' — use \\n, \\t, \\' ou \\\\"
                    )))
                }
                None => {
                    return Err(self.erro_em(
                        linha_abertura,
                        coluna_abertura,
                        "literal de caractere não foi fechado com '\\''",
                    ))
                }
            },
            Some(c) => c,
            None => {
                return Err(self.erro_em(
                    linha_abertura,
                    coluna_abertura,
                    "literal de caractere não foi fechado com '\\''",
                ))
            }
        };

        match self.advance() {
            Some('\'') => Ok(TokenKind::Caractere(conteudo)),
            Some(_) => Err(self.erro_em(
                linha_abertura,
                coluna_abertura,
                format!(
                    "literal de caractere tem mais de um símbolo — 'caractere' exige \
                     exatamente um; use \"...\" (aspas duplas) para uma cadeia com \
                     '{conteudo}' seguido de mais texto"
                ),
            )),
            None => Err(self.erro_em(
                linha_abertura,
                coluna_abertura,
                "literal de caractere não foi fechado com '\\''",
            )),
        }
    }

    // -- Tokens iniciados por '.' -------------------------------------------------

    /// Trata os três casos possíveis para um `.`:
    ///
    /// 1. Literal lógico/operador "com pontos": `.e.` `.ou.` `.não./.nao.`
    ///    `.xou.` `.v.` `.f.` `.verdadeiro.` `.falso.` (seção 3/5.4);
    /// 2. `..` — intervalo (`[1..10]`) ou resolução de escopo (`Classe..Método`);
    /// 3. `.` simples — acesso a campo (`REGISTRO.CAMPO`).
    ///
    /// A tentativa (1) só consome caracteres se encontrar, de fato, uma das
    /// palavras reconhecidas seguida de um `.` de fechamento — caso contrário,
    /// nada é consumido além do primeiro ponto, preservando `.CAMPO` como
    /// `Ponto` + `Identificador`.
    fn ler_ponto(&mut self) -> Result<TokenKind, ErroLexico> {
        self.advance(); // consome o primeiro '.'

        // Tentativa 1: ".palavra." — operador lógico ou literal booleano.
        if matches!(self.peek(), Some(c) if c.is_alphabetic()) {
            let mut palavra = String::new();
            let mut offset = 0;
            while let Some(c) = self.peek_at(offset) {
                if c.is_alphabetic() {
                    palavra.push(c);
                    offset += 1;
                } else {
                    break;
                }
            }

            if self.peek_at(offset) == Some('.') {
                if let Some(kind) = palavra_com_pontos(&palavra) {
                    for _ in 0..offset {
                        self.advance();
                    }
                    self.advance(); // consome o '.' de fechamento
                    return Ok(kind);
                }
            }
        }

        // Tentativa 2: ".." — intervalo ou resolução de escopo.
        if self.peek() == Some('.') {
            self.advance();
            return Ok(TokenKind::PontoPonto);
        }

        // Tentativa 3: '.' simples — acesso a campo.
        Ok(TokenKind::Ponto)
    }

    // -- Operadores e pontuação ----------------------------------------------------

    fn ler_operador_ou_pontuacao(&mut self, c: char) -> Result<TokenKind, ErroLexico> {
        match c {
            // Atribuição: '<-' (forma canônica, ✅ v0.9) ou ← (U+2190, sinônimo
            // tipográfico — seção 5.1/5.7)
            '←' => {
                self.advance();
                Ok(TokenKind::Seta)
            }
            '<' => {
                self.advance();
                match self.peek() {
                    Some('-') => {
                        self.advance();
                        Ok(TokenKind::Seta)
                    }
                    Some('=') => {
                        self.advance();
                        Ok(TokenKind::MenorIgual)
                    }
                    Some('>') => {
                        self.advance();
                        Ok(TokenKind::Diferente)
                    }
                    _ => Ok(TokenKind::Menor),
                }
            }
            '>' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Ok(TokenKind::MaiorIgual)
                } else {
                    Ok(TokenKind::Maior)
                }
            }
            // Potenciação: '^' (forma canônica, ✅ v0.9) ou ↑ (U+2191, sinônimo
            // tipográfico — seção 5.2/5.7)
            '↑' | '^' => {
                self.advance();
                Ok(TokenKind::Potencia)
            }
            '+' => {
                self.advance();
                Ok(TokenKind::Mais)
            }
            // Subtração: '-' ou '–' en-dash U+2013 (seção 5.2)
            '-' | '–' => {
                self.advance();
                Ok(TokenKind::Menos)
            }
            '*' => {
                self.advance();
                Ok(TokenKind::Asterisco)
            }
            '/' => {
                self.advance();
                Ok(TokenKind::Barra)
            }
            '=' => {
                self.advance();
                Ok(TokenKind::Igual)
            }
            '(' => {
                self.advance();
                Ok(TokenKind::AbreParen)
            }
            ')' => {
                self.advance();
                Ok(TokenKind::FechaParen)
            }
            '[' => {
                self.advance();
                Ok(TokenKind::AbreColchete)
            }
            ']' => {
                self.advance();
                Ok(TokenKind::FechaColchete)
            }
            ',' => {
                self.advance();
                Ok(TokenKind::Virgula)
            }
            ':' => {
                self.advance();
                Ok(TokenKind::DoisPontos)
            }
            outro => Err(self.erro(format!("caractere inesperado: '{outro}'"))),
        }
    }
}

/// Reconhece tokens "com pontos" — operadores lógicos e literais booleanos
/// (seção 3/5.4). `palavra` já vem sem os pontos delimitadores.
/// Aceita variantes sem acento (`nao`) como sinônimo de `não`.
fn palavra_com_pontos(palavra: &str) -> Option<TokenKind> {
    match palavra.to_lowercase().as_str() {
        "e" => Some(TokenKind::E),
        "ou" => Some(TokenKind::Ou),
        "não" | "nao" => Some(TokenKind::Nao),
        "xou" => Some(TokenKind::Xou),
        "v" => Some(TokenKind::Logico(true)),
        "f" => Some(TokenKind::Logico(false)),
        "verdadeiro" => Some(TokenKind::Logico(true)),
        "falso" => Some(TokenKind::Logico(false)),
        _ => None,
    }
}

/// Tabela de palavras-chave reservadas (seção 11). A comparação é
/// case-insensitive (seção 1.3); variantes sem acentuação são aceitas como
/// sinônimo de conveniência (ex.: `inicio` por `início`, `nao` por `não`,
/// `funcao` por `função`).
///
/// Identificadores pré-definidos (`p_pi`, `raizq`, etc. — seção 5.6) e
/// funções de *casting* (`inteiro(x)` etc. — seção 10.5.1) **não** entram
/// aqui: são identificadores normais, resolvidos como pré-definidos pelo
/// verificador semântico/interpretador (permitindo *shadowing*).
fn palavra_chave(s: &str) -> Option<TokenKind> {
    use TokenKind::*;

    let lower = s.to_lowercase();
    Some(match lower.as_str() {
        "programa" => Programa,
        "const" => Const,
        "tipo" => Tipo,
        "var" => Var,
        "ref" => Ref,
        "vlr" => Vlr,
        "objeto" => Objeto,
        "início" | "inicio" => Inicio,
        "fim" => Fim,

        "se" => Se,
        "então" | "entao" => Entao,
        "senão" | "senao" => Senao,
        "fim_se" => FimSe,
        "exceto_se" => ExcetoSe,
        "fim_exceto_se" => FimExcetoSe,
        "caso" => Caso,
        "seja" => Seja,
        "faça" | "faca" => Faca,
        "fim_caso" => FimCaso,

        "enquanto" => Enquanto,
        "fim_enquanto" => FimEnquanto,
        "até_seja" | "ate_seja" => AteSeja,
        "efetue" => Efetue,
        "fim_até_seja" | "fim_ate_seja" => FimAteSeja,
        "repita" => Repita,
        "até_que" | "ate_que" => AteQue,
        "execute" => Execute,
        "enquanto_for" => EnquantoFor,
        "laço" | "laco" => Laco,
        "saia_caso" => SaiaCaso,
        "fim_laço" | "fim_laco" => FimLaco,
        "interrompa" => Interrompa,
        "continue" => Continue,
        "para" => Para,
        "de" => De,
        "até" | "ate" => Ate,
        "passo" => Passo,
        "fim_para" => FimPara,
        "ir_para" => IrPara,

        "leia" => Leia,
        "escreva" => Escreva,
        "escreva_ln" => EscrevaLn,
        "leia_seco" => LeiaSeco,
        "pausa" => Pausa,

        "limpar" => Limpar,
        "limpar_linha" => LimparLinha,
        "posicionar" => Posicionar,
        "cor_fundo" => CorFundo,
        "cor_frente" => CorFrente,

        "registro" => Registro,
        "fim_registro" => FimRegistro,
        "conjunto" => Conjunto,
        "dimensione" => Dimensione,

        "procedimento" => Procedimento,
        "função" | "funcao" => Funcao,

        "classe" => Classe,
        "fim_classe" => FimClasse,
        "herança" | "heranca" => Heranca,
        "virtual" => Virtual,
        "sobrepor" => Sobrepor,
        "seção_pública" | "secao_publica" | "seção_publica" | "secao_pública" => SecaoPublica,
        "seção_protegida" | "secao_protegida" => SecaoProtegida,
        "seção_privada" | "secao_privada" => SecaoPrivada,
        "este" => Este,

        "inteiro" => TipoInteiro,
        "real" => TipoReal,
        "cadeia" => TipoCadeia,
        "caractere" => TipoCaractere,
        "lógico" | "logico" => TipoLogico,
        "generico" | "genérico" => Generico,

        "div" => Div,
        "mod" => Mod,

        _ => return None,
    })
}

// =================================================================================
// Testes
// =================================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    /// Atalho: tokeniza e descarta o `FimDeArquivo` final, retornando só os
    /// `TokenKind`s — facilita comparações nos testes.
    fn kinds(fonte: &str) -> Vec<TokenKind> {
        let tokens = tokenizar(fonte).expect("tokenização não deveria falhar");
        let mut kinds: Vec<TokenKind> = tokens.into_iter().map(|t| t.kind).collect();
        assert_eq!(kinds.pop(), Some(FimDeArquivo));
        kinds
    }

    #[test]
    fn programa_minimo() {
        let fonte = "programa OI\nvar\n  X : inteiro\ninício\n  X ← 1\n  escreva X\nfim";
        let k = kinds(fonte);
        assert_eq!(
            k,
            vec![
                Programa,
                Identificador("OI".into()),
                Var,
                Identificador("X".into()),
                DoisPontos,
                TipoInteiro,
                Inicio,
                Identificador("X".into()),
                Seta,
                Inteiro(1),
                Escreva,
                Identificador("X".into()),
                Fim,
            ]
        );
    }

    #[test]
    fn identificadores_acentuados() {
        let k = kinds("ÁREA_CÍRCULO MÊS NÚMERO início função");
        assert_eq!(
            k,
            vec![
                Identificador("ÁREA_CÍRCULO".into()),
                Identificador("MÊS".into()),
                Identificador("NÚMERO".into()),
                Inicio,
                Funcao,
            ]
        );
    }

    #[test]
    fn sinonimos_de_palavras_chave_sem_acento() {
        // 'inicio', 'nao', 'funcao', 'ate_que' devem mapear para os mesmos
        // tokens que 'início', 'não', 'função', 'até_que'.
        assert_eq!(kinds("início")[0], kinds("inicio")[0]);
        assert_eq!(kinds("função")[0], kinds("funcao")[0]);
        assert_eq!(kinds("até_que")[0], kinds("ate_que")[0]);
    }

    #[test]
    fn marcadores_ref_e_vlr_sao_reconhecidos() {
        // ✅ v0.10: 'ref' (passagem por referência) e 'vlr' (passagem por
        // valor, opcional/redundante) — seção 9.3.
        assert_eq!(kinds("ref")[0], Ref);
        assert_eq!(kinds("vlr")[0], Vlr);
        // 'var' continua reservado à seção de declarações, não é mais
        // sinônimo de marcador de referência.
        assert_eq!(kinds("var")[0], Var);
    }

    #[test]
    fn numeros_inteiros_e_reais() {
        let k = kinds("10 0 3.14159265 2.0");
        assert_eq!(k, vec![Inteiro(10), Inteiro(0), Real(3.14159265), Real(2.0)]);
    }

    #[test]
    fn numero_inteiro_grande_demais_e_erro() {
        let fonte = "99999999999999999999999999";
        let erro = tokenizar(fonte).unwrap_err();
        assert!(erro.mensagem.contains("inteiro"));
    }

    #[test]
    fn literal_de_texto_simples() {
        let k = kinds("\"ADIÇÃO DE NÚMEROS\"");
        assert_eq!(k, vec![Texto("ADIÇÃO DE NÚMEROS".into())]);
    }

    #[test]
    fn literal_de_texto_com_escapes() {
        let k = kinds(r#""Resultado = \n\t\"X\"\\""#);
        assert_eq!(k, vec![Texto("Resultado = \n\t\"X\"\\".into())]);
    }

    #[test]
    fn literal_de_texto_nao_fechado_e_erro() {
        let erro = tokenizar("\"abc").unwrap_err();
        assert!(erro.mensagem.contains("fechado"));
    }

    #[test]
    fn literal_de_caractere_simples() {
        let k = kinds("'S'");
        assert_eq!(k, vec![Caractere('S')]);
    }

    #[test]
    fn literal_de_caractere_com_escape() {
        let k = kinds(r"'\n'");
        assert_eq!(k, vec![Caractere('\n')]);
        let k = kinds(r"'\''");
        assert_eq!(k, vec![Caractere('\'')]);
    }

    #[test]
    fn literal_de_caractere_vazio_e_erro() {
        let erro = tokenizar("''").unwrap_err();
        assert!(erro.mensagem.contains("vazio"));
    }

    #[test]
    fn literal_de_caractere_com_mais_de_um_simbolo_e_erro() {
        let erro = tokenizar("'AB'").unwrap_err();
        assert!(erro.mensagem.contains("mais de um"));
    }

    #[test]
    fn literal_de_caractere_nao_fechado_e_erro() {
        let erro = tokenizar("'A").unwrap_err();
        assert!(erro.mensagem.contains("fechado"));
    }

    #[test]
    fn texto_e_caractere_sao_tokens_distintos() {
        // "S" (aspas duplas) é Texto; 'S' (aspas simples) é Caractere —
        // mesmo símbolo, tokens diferentes (seção 3).
        let k = kinds(r#""S" 'S'"#);
        assert_eq!(k, vec![Texto("S".into()), Caractere('S')]);
    }

    #[test]
    fn operadores_logicos_com_pontos() {
        let k = kinds("(A >= 20) .e. (A <= 90) .ou. .não. X .xou. .nao. Y");
        assert_eq!(
            k,
            vec![
                AbreParen,
                Identificador("A".into()),
                MaiorIgual,
                Inteiro(20),
                FechaParen,
                E,
                AbreParen,
                Identificador("A".into()),
                MenorIgual,
                Inteiro(90),
                FechaParen,
                Ou,
                Nao,
                Identificador("X".into()),
                Xou,
                Nao,
                Identificador("Y".into()),
            ]
        );
    }

    #[test]
    fn literais_logicos_extensos_e_reduzidos() {
        let k = kinds(".verdadeiro. .falso. .v. .f. .Verdadeiro. .V.");
        assert_eq!(
            k,
            vec![
                Logico(true),
                Logico(false),
                Logico(true),
                Logico(false),
                Logico(true),
                Logico(true),
            ]
        );
    }

    #[test]
    fn acesso_a_campo_vs_intervalo_vs_operador_logico() {
        // ALUNO.NOME            -> Identificador . Identificador
        // conjunto [1..10]      -> ... Inteiro PontoPonto Inteiro ...
        // X .e. Y               -> ... E ...
        let k = kinds("ALUNO.NOME conjunto [1..10] de inteiro X .e. Y");
        assert_eq!(
            k,
            vec![
                Identificador("ALUNO".into()),
                Ponto,
                Identificador("NOME".into()),
                Conjunto,
                AbreColchete,
                Inteiro(1),
                PontoPonto,
                Inteiro(10),
                FechaColchete,
                De,
                TipoInteiro,
                Identificador("X".into()),
                E,
                Identificador("Y".into()),
            ]
        );
    }

    #[test]
    fn resolucao_de_escopo_classe_metodo() {
        let k = kinds("função Aluno..CALCMÉDIA() : real");
        assert_eq!(
            k,
            vec![
                Funcao,
                Identificador("Aluno".into()),
                PontoPonto,
                Identificador("CALCMÉDIA".into()),
                AbreParen,
                FechaParen,
                DoisPontos,
                TipoReal,
            ]
        );
    }

    #[test]
    fn sinonimos_de_atribuicao_e_potencia() {
        let a = kinds("X ← A ↑ 2");
        let b = kinds("X <- A ^ 2");
        assert_eq!(a, b);
        assert_eq!(a[1], Seta);
        assert_eq!(a[3], Potencia);
    }

    #[test]
    fn sinonimo_de_subtracao_en_dash() {
        // '–' é en-dash (U+2013), usado por engano no material-fonte.
        let a = kinds("A - B");
        let b = kinds("A – B");
        assert_eq!(a, b);
        assert_eq!(a[1], Menos);
    }

    #[test]
    fn operadores_relacionais() {
        let k = kinds("= <> < > <= >=");
        assert_eq!(k, vec![Igual, Diferente, Menor, Maior, MenorIgual, MaiorIgual]);
    }

    #[test]
    fn comentario_de_linha_e_ignorado() {
        let k = kinds("X ← 1 // isto é um comentário\nY ← 2");
        assert_eq!(
            k,
            vec![
                Identificador("X".into()),
                Seta,
                Inteiro(1),
                Identificador("Y".into()),
                Seta,
                Inteiro(2),
            ]
        );
    }

    #[test]
    fn comentario_de_bloco_e_ignorado() {
        let k = kinds("X ← 1 {comentário\nem múltiplas linhas} Y ← 2");
        assert_eq!(
            k,
            vec![
                Identificador("X".into()),
                Seta,
                Inteiro(1),
                Identificador("Y".into()),
                Seta,
                Inteiro(2),
            ]
        );
    }

    #[test]
    fn comentario_de_bloco_nao_fechado_e_erro() {
        let erro = tokenizar("X ← 1 {comentário").unwrap_err();
        assert!(erro.mensagem.contains("'{'"));
    }

    #[test]
    fn ponto_e_virgula_e_ignorado_em_qualquer_posicao() {
        let a = kinds("X ← 1; Y ← 2");
        let b = kinds("X ← 1 Y ← 2");
        assert_eq!(a, b);
    }

    #[test]
    fn especificador_de_formatacao_escreva() {
        // escreva R : 8 : 2
        let k = kinds("escreva R : 8 : 2");
        assert_eq!(
            k,
            vec![
                Escreva,
                Identificador("R".into()),
                DoisPontos,
                Inteiro(8),
                DoisPontos,
                Inteiro(2),
            ]
        );
    }

    #[test]
    fn comandos_conio() {
        let k = kinds(
            "limpar limpar_linha(5) posicionar(10, 2) leia_seco SENHA cor_fundo(1) cor_frente(15) pausa",
        );
        assert_eq!(
            k,
            vec![
                Limpar,
                LimparLinha,
                AbreParen,
                Inteiro(5),
                FechaParen,
                Posicionar,
                AbreParen,
                Inteiro(10),
                Virgula,
                Inteiro(2),
                FechaParen,
                LeiaSeco,
                Identificador("SENHA".into()),
                CorFundo,
                AbreParen,
                Inteiro(1),
                FechaParen,
                CorFrente,
                AbreParen,
                Inteiro(15),
                FechaParen,
                Pausa,
            ]
        );
    }

    #[test]
    fn cast_estilo_funcao_e_estilo_c() {
        let k = kinds("inteiro(X) (inteiro) X");
        assert_eq!(
            k,
            vec![
                TipoInteiro,
                AbreParen,
                Identificador("X".into()),
                FechaParen,
                AbreParen,
                TipoInteiro,
                FechaParen,
                Identificador("X".into()),
            ]
        );
    }

    #[test]
    fn posicoes_linha_coluna() {
        let tokens = tokenizar("programa X\nvar\n  A : inteiro").unwrap();
        // 'programa' na linha 1, coluna 1
        assert_eq!((tokens[0].linha, tokens[0].coluna), (1, 1));
        // 'X' na linha 1, coluna 10
        assert_eq!((tokens[1].linha, tokens[1].coluna), (1, 10));
        // 'var' na linha 2, coluna 1
        assert_eq!((tokens[2].linha, tokens[2].coluna), (2, 1));
        // 'A' na linha 3, coluna 3
        assert_eq!((tokens[3].linha, tokens[3].coluna), (3, 3));
    }

    #[test]
    fn caractere_inesperado_e_erro() {
        let erro = tokenizar("X ← 1 @ Y").unwrap_err();
        assert!(erro.mensagem.contains('@'));
    }

    #[test]
    fn programa_completo_adicao_numeros() {
        let fonte = r#"programa ADIÇÃO_NÚMEROS
var
  X : inteiro
  A : inteiro
  B : inteiro
início
  leia A
  leia B
  X ← A + B
  escreva X
fim"#;
        // Não deve haver erro léxico, e o número de tokens deve ser razoável.
        let tokens = tokenizar(fonte).expect("não deveria falhar");
        assert_eq!(tokens.last().unwrap().kind, FimDeArquivo);
        assert!(tokens.len() > 20);
        assert_eq!(tokens[0].kind, Programa);
        assert_eq!(tokens[1].kind, Identificador("ADIÇÃO_NÚMEROS".into()));
    }
}
