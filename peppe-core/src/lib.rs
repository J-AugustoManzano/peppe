//! Programa ...: PEPPE (Português Estruturado Para Programação Educacional)
//! Autor ......: Augusto Manzano
//! Data .......: agosto de 2026
//! Versão .....: 0.1.0
//! Release ....: beta
//!
//! `peppe-core` — biblioteca central da linguagem PEPPE
//! (Português Estruturado Para Programação Educacional).
//!
//! Esta crate concentra toda a lógica independente de I/O: lexer, parser,
//! verificador semântico e interpretador (da especificação).
//! Tanto o `peppe-cli` quanto futuras interfaces (IDE, LSP, playground web
//! via WASM) reutilizam este crate sem reescrita.
//!
//! ## Módulos
//! - `lexer` — tokenizador
//! - `ast` — nós da AST para o núcleo estrutural 
//! - `parser` — parser recursivo-descendente do núcleo estrutural 
//! - `tipos` — tipos resolvidos e regras de compatibilidade/coerção
//! - `checker` — verificador semântico: declarações, comandos e
//!   expressões
//! - `interpreter` — interpretador *tree-walking* do núcleo estrutural
//!   ; `ir_para`/rótulos e `tente`/`captura` não
//!   são suportados

pub mod ast;
pub mod checker;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod tipos;
pub mod token;

pub use checker::{verificar, ErroSemantico, ResultadoVerificacao};
pub use interpreter::{interpretar, ConsoleIO, ConsoleMemoria, ErroExecucao, Valor};
pub use lexer::{tokenizar, ErroLexico, Lexer};
pub use parser::{parsear, ErroSintatico};
pub use token::{Token, TokenKind};
