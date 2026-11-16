//! Vali's reader and source representation (ADR 0014).
//!
//! One Logos lexer and one error-tolerant parser emit events, which two
//! sinks consume: a lossless Rowan tree for tools ([`parse_tree`]), and a
//! datum builder for running code ([`read`], [`read_with`]).
//!
//! - [`lex`] splits source text into [`Token`]s, losing nothing.
//! - [`parse`] streams the parser's events into any [`EventSink`].
//! - [`TreeSink`] builds a lossless [`SyntaxNode`] tree.
//! - [`DatumSink`] builds datums through a [`DatumBuilder`]; [`PlainBuilder`]
//!   makes plain [`Datum`]s.

mod datum;
mod ident;
mod kind;
mod lexer;
mod parser;
mod reader;
mod span;
mod tree;

pub use datum::{Datum, DatumKind, PlainBuilder};
pub use ident::is_identifier;
pub use kind::SyntaxKind;
pub use lexer::{LexError, LexErrorKind, Lexed, Token, lex};
pub use parser::{EventSink, ParseError, ParseErrorKind, parse};
pub use reader::{
    DatumBuilder, DatumSink, ParsedTree, Read, ReadError, ReadErrorKind, parse_tree, read,
    read_with,
};
pub use span::{SourceId, Span};
pub use text_size::{TextRange, TextSize};
pub use tree::{SyntaxElement, SyntaxNode, SyntaxToken, TreeSink, ValiLanguage};
