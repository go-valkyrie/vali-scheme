//! Vali's reader and source representation (ADR 0014).
//!
//! One Logos lexer and one error-tolerant parser emit events, which two
//! sinks consume: a lossless Rowan tree for tools, and a datum builder for
//! running code. This crate also holds spans, source identities, scope sets
//! and the syntax-object data structures.
//!
//! So far it has the lexer: [`lex`] splits source text into [`Token`]s,
//! losing nothing.

mod ident;
mod kind;
mod lexer;

pub use ident::is_identifier;
pub use kind::SyntaxKind;
pub use lexer::{LexError, LexErrorKind, Lexed, Token, lex};
pub use text_size::{TextRange, TextSize};
