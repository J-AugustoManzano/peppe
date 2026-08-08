//! `peppe-core` — biblioteca central da linguagem PEPPE
//! (Português Estruturado Para Programação Educacional).
//!
//! Esta crate concentra toda a lógica independente de I/O: lexer, parser,
//! verificador semântico e interpretador (seção 17.1 da especificação).
//! Tanto o `peppe-cli` quanto futuras interfaces (IDE, LSP, playground web
//! via WASM) reutilizam este crate sem reescrita.
//!
//! ## Status
//! - ✅ `lexer` — tokenizador completo (seção 1–6 da especificação)
//! - ✅ `ast` — nós da AST para o núcleo estrutural (seções 1–9)
//! - ✅ `parser` — parser recursivo-descendente do núcleo estrutural (seções 1–9)
//! - ✅ `tipos` — tipos resolvidos e regras de compatibilidade/coerção (seção 10.5)
//! - ✅ `checker` — verificador semântico completo (seção 15): declarações,
//!   comandos e expressões
//! - ✅ `interpreter` — interpretador *tree-walking* do núcleo estrutural
//!   (seções 1–9); `ir_para`/rótulos e `tente`/`captura` (seção 20.1) ainda
//!   não implementados

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
