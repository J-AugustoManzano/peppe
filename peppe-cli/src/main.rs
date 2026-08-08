//! Programa ...: PEPPE (Português Estruturado Para Programação Educacional)
//! Autor ......: Augusto Manzano
//! Data .......: agosto de 2026
//! Versão .....: 0.1.0
//! Release ....: beta
//!
//! `peppe-cli` — interface de linha de comando do interpretador PEPPE.
//!
//! O CLI roda a pipeline completa sobre o arquivo `.pe` informado: lexer ->
//! parser -> verificador semântico (`checker`) -> interpretador. Se a
//! análise (lexer/parser/checker) encontrar qualquer erro, a execução não
//! começa — todos os erros são mostrados de uma vez. Se a
//! análise passar, o programa é executado de fato, lendo `leia`/`leia_seco`
//! de `stdin` e escrevendo `escreva` em `stdout`.
//!
//! Uso:
//! ```text
//! peppe <arquivo.pe>            analisa e executa o programa
//! peppe --analisar <arquivo.pe> apenas analisa (lexer + parser + checker), sem executar
//! peppe --tokens <arquivo.pe>   apenas tokeniza e lista os tokens reconhecidos
//! ```

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use peppe_core::{interpretar, parsear, tokenizar, verificar, ConsoleIO};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    let (modo, caminho) = match args.as_slice() {
        [flag, caminho] if flag == "--tokens" => (Modo::Tokens, caminho.clone()),
        [flag, caminho] if flag == "--analisar" => (Modo::Analisar, caminho.clone()),
        [caminho] => (Modo::Executar, caminho.clone()),
        _ => {
            exibir_tela_credito();
            return ExitCode::FAILURE;
        }
    };

    let fonte = match fs::read(&caminho) {
        Ok(bytes) => {
            // Tenta UTF-8 primeiro (padrão moderno). Se falhar, trata como
            // Windows-1252 (ANSI, padrão do Bloco de Notas no Windows) —
            // cada byte 0x00-0x7F é idêntico ao UTF-8; bytes 0x80-0xFF são
            // mapeados para os caracteres Windows-1252 correspondentes via
            // a tabela padrão (ISO 8859-1 + extensão da faixa 0x80-0x9F).
            // Isso garante que arquivos salvos em qualquer codificação comum
            // no Windows funcionem sem que o aluno precise configurar nada.
            match String::from_utf8(bytes) {
                Ok(conteudo) => conteudo,
                Err(e) => {
                    // Converte Windows-1252 → UTF-8 mapeando cada byte
                    // individualmente pelo mapa oficial do W3C/Unicode.
                    let bytes = e.into_bytes();
                    bytes.iter().map(|&b| cp1252_para_char(b)).collect()
                }
            }
        }
        Err(e) => {
            eprintln!("Erro ao ler '{caminho}': {e}");
            return ExitCode::FAILURE;
        }
    };

    // -- Etapa 1: análise léxica -----------------------------------------------
    let tokens = match tokenizar(&fonte) {
        Ok(tokens) => tokens,
        Err(erro) => {
            eprintln!("{erro}");
            return ExitCode::FAILURE;
        }
    };

    if modo == Modo::Tokens {
        for token in &tokens {
            println!("{:>4}:{:<3}  {}", token.linha, token.coluna, token.kind);
        }
        println!("\n{} tokens reconhecidos.", tokens.len());
        return ExitCode::SUCCESS;
    }

    // -- Etapa 2: análise sintática ----------------------------------------------
    let programa = match parsear(tokens) {
        Ok(programa) => programa,
        Err(erro) => {
            eprintln!("{erro}");
            return ExitCode::FAILURE;
        }
    };

    // -- Etapa 3: análise semântica -----------------------------------------------
    let resultado = verificar(&programa);
    if !resultado.erros.is_empty() {
        eprintln!(
            "'{}': {} erro(s) semântico(s) encontrado(s):\n",
            programa.nome,
            resultado.erros.len()
        );
        for erro in &resultado.erros {
            eprintln!("{erro}");
        }
        return ExitCode::FAILURE;
    }

    if modo == Modo::Analisar {
        println!("'{}': nenhum erro encontrado.", programa.nome);
        return ExitCode::SUCCESS;
    }

    // -- Etapa 4: execução -------------------------------------------------------
    let mut console = ConsoleTerminal::novo();
    match interpretar(&programa, &mut console) {
        Ok(()) => ExitCode::SUCCESS,
        Err(erro) => {
            // Garante que toda saída pendente (sem '\n' final) apareça
            // antes da mensagem de erro.
            let _ = io::stdout().flush();
            eprintln!("\n{erro}");
            ExitCode::FAILURE
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Modo {
    Tokens,
    Analisar,
    Executar,
}

/// Implementação de [`ConsoleIO`] sobre o terminal real (stdin/stdout).
///
/// Os comandos CONIO usam sequências de escape ANSI, que
/// funcionam no Windows Terminal/PowerShell modernos sem dependências
/// extras; um backend mais robusto via `crossterm` é um refinamento futuro
/// caso sequências ANSI se mostrem insuficientes (ex.: Prompt de Comando
/// antigo sem suporte a ANSI habilitado).
struct ConsoleTerminal {
    stdin_eh_terminal: bool,
}

impl ConsoleTerminal {
    fn novo() -> Self {
        ConsoleTerminal { stdin_eh_terminal: io::stdin().is_terminal() }
    }
}

impl ConsoleIO for ConsoleTerminal {
    fn escrever(&mut self, texto: &str) {
        print!("{texto}");
        let _ = io::stdout().flush();
    }

    fn ler_linha(&mut self) -> String {
        let mut linha = String::new();
        match io::stdin().lock().read_line(&mut linha) {
            Ok(0) => String::new(), // fim de entrada (Ctrl+Z/Ctrl+D)
            Ok(_) => linha.trim_end_matches(['\r', '\n']).to_string(),
            Err(_) => String::new(),
        }
    }

    fn ler_linha_sem_eco(&mut self) -> String {
        // Leitura sem eco real exigiria desabilitar o modo "echo" do
        // terminal (via crossterm ou WinAPI/termios) — fora de escopo nesta
        // etapa. Por ora comporta-se como 'leia' normal (com eco), tanto em
        // terminal interativo quanto com entrada via pipe/redirecionamento.
        let _ = self.stdin_eh_terminal; // reservado para a implementação futura
        self.ler_linha()
    }

    fn pausar(&mut self) {
        let _ = io::stdout().flush();
        self.ler_linha();
    }

    fn limpar(&mut self) {
        print!("\x1b[2J\x1b[H");
        let _ = io::stdout().flush();
    }

    fn limpar_linha(&mut self, coluna: Option<i64>) {
        match coluna {
            Some(c) => print!("\x1b[{c}G\x1b[K"),
            None => print!("\x1b[K"),
        }
        let _ = io::stdout().flush();
    }

    fn posicionar(&mut self, coluna: i64, linha: i64) {
        print!("\x1b[{linha};{coluna}H");
        let _ = io::stdout().flush();
    }

    fn cor_fundo(&mut self, cor: i64) {
        print!("\x1b[{}m", codigo_ansi_fundo(cor));
        let _ = io::stdout().flush();
    }

    fn cor_frente(&mut self, cor: i64) {
        print!("\x1b[{}m", codigo_ansi_frente(cor));
        let _ = io::stdout().flush();
    }
}

/// Converte a paleta PEPPE (0–15, estilo Turbo Pascal CRT ) para
/// o código ANSI de cor de texto correspondente (30–37 normais, 90–97
/// "bright", seguindo a mesma ordem de matiz 0–7).
fn codigo_ansi_frente(cor: i64) -> i64 {
    let c = cor.clamp(0, 15);
    if c < 8 {
        30 + c
    } else {
        90 + (c - 8)
    }
}

fn codigo_ansi_fundo(cor: i64) -> i64 {
    let c = cor.clamp(0, 15);
    if c < 8 {
        40 + c
    } else {
        100 + (c - 8)
    }
}

/// Converte um byte Windows-1252 para o `char` Unicode correspondente.
/// A faixa 0x00–0x7F é idêntica ao ASCII/UTF-8. A faixa 0xA0–0xFF é
/// idêntica ao Latin-1 (ISO 8859-1). Só a faixa 0x80–0x9F difere —
/// ela contém caracteres tipográficos do Windows que não existem no
/// Latin-1 puro (€, †, ‡, …, etc.). Tabela oficial: <https://www.w3.org/TR/encoding/#names-and-labels>
fn cp1252_para_char(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // '
        0x92 => '\u{2019}', // '
        0x93 => '\u{201C}', // "
        0x94 => '\u{201D}', // "
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        // 0x81, 0x8D, 0x8F, 0x90, 0x9D são indefinidos em CP1252;
        // substitui por U+FFFD (caractere de substituição) por segurança.
        0x81 | 0x8D | 0x8F | 0x90 | 0x9D => '\u{FFFD}',
        // 0x00–0x7F e 0xA0–0xFF: idênticos ao Unicode (Latin-1).
        b => b as char,
    }
}

/// Exibe a tela de crédito/ajuda com logo ASCII em degradê azul escuro → ciano → branco.
/// Usa cores ANSI RGB de 24 bits (`\x1b[38;2;R;G;Bm`) — funciona em qualquer
/// terminal moderno (Windows Terminal, VS Code, ConEmu, etc.).
fn exibir_tela_credito() {
    // Cada caractere da linha recebe uma cor interpolada da esquerda (azul escuro)
    // para a direita (quase branco), passando pelo ciano.
    // Azul escuro: R=20  G=60  B=180
    // Ciano:       R=0   G=200 B=220
    // Branco:      R=220 G=240 B=255
    let linhas_logo = [
        ":::::::::   ::::::::::  :::::::::   :::::::::   ::::::::::",
        "+:+    +:+  +:+         +:+    +:+  +:+    +:+  +:+            Português  ",
        "+:+    +:+  +:+         +:+    +:+  +:+    +:+  +:+            Estruturado",
        "+#++:++#+   +#++::++#   +#++:++#+   +#++:++#+   +#++::++#      Para",
        "+#+         +#+         +#+         +#+         +#+            Programação",
        "#+#         #+#         #+#         #+#         #+#            Educacional",
        "###         ##########  ###         ###         ##########",
    ];

    fn cor(col: usize, total: usize) -> (u8, u8, u8) {
        let t = if total == 0 { 0.0 } else { col as f64 / total as f64 };
        if t < 0.5 {
            // azul escuro → ciano
            let s = t * 2.0;
            let r = (20.0  + s * (0.0   - 20.0))  as u8;
            let g = (60.0  + s * (200.0 - 60.0))  as u8;
            let b = (180.0 + s * (220.0 - 180.0)) as u8;
            (r, g, b)
        } else {
            // ciano → branco
            let s = (t - 0.5) * 2.0;
            let r = (0.0   + s * (220.0 - 0.0))   as u8;
            let g = (200.0 + s * (240.0 - 200.0)) as u8;
            let b = (220.0 + s * (255.0 - 220.0)) as u8;
            (r, g, b)
        }
    }

    fn linha_colorida(texto: &str) -> String {
        let chars: Vec<char> = texto.chars().collect();
        let total = chars.len();
        let mut out = String::new();
        for (i, c) in chars.iter().enumerate() {
            let (r, g, b) = cor(i, total.saturating_sub(1));
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{c}"));
        }
        out.push_str("\x1b[0m"); // reset de cor
        out
    }

    let sep = linha_colorida("---------------------------------------------------------------------------------");
    eprintln!("{sep}");
    for l in &linhas_logo {
        eprintln!("{}", linha_colorida(l));
    }
    eprintln!("{sep}");
    eprintln!("{}", linha_colorida("PEPPE - v0.50"));
    eprintln!("{sep}");
    eprintln!("{}", linha_colorida("Algoritmos: Lógica para Desenvolvimento de Programação Imperativa de Computadores"));
    eprintln!("{}", linha_colorida("Direitos Autorais (c) 2027 de Augusto Manzano & Jayr Figueiredo"));
    eprintln!("{}", linha_colorida("Editora LTC - Rio de Janeiro - Brasil"));
    eprintln!("{}", linha_colorida("Linguagem de Projeto de Programação (Program Design Language)"));
    eprintln!("{sep}");
    eprintln!("{}", linha_colorida("Este software é fornecido  \"no estado em que se encontra\", sem  garantia de qual-"));
    eprintln!("{}", linha_colorida("quer natureza, expressa ou implícita.  Os autores  e a editora não se responsabi-"));
    eprintln!("{}", linha_colorida("lizam  por quaisquer danos ou prejuízos decorrentes da utilização deste software."));
    eprintln!("{sep}");
    eprintln!();
    eprintln!("{}", linha_colorida("Uso: peppe [nome_programa.pe]"));
    eprintln!();
}


