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
//!
//! The scope machinery of ADR 0015 lives here too:
//!
//! - [`ScopeContext`] numbers [`Scope`]s and interns scope sets for one
//!   expansion.
//! - [`BindingTable`] maps a symbol and a scope set to a binding, with the
//!   [`Phases`] where it is visible, and resolves identifiers.
//! - [`Origin`] records the macro use that produced a syntax object.
//! - [`Syntax`] is a syntax object, with scope changes propagated lazily.
//! - With the `reference` feature, `reference` holds a plain syntax type
//!   for tests.

mod binding;
mod datum;
mod debug;
mod ident;
mod kind;
mod lexer;
mod origin;
mod parser;
mod reader;
#[cfg(any(test, feature = "reference"))]
pub mod reference;
mod scope;
mod span;
mod syntax;
mod tree;
mod walk;

pub use binding::{AlreadyBound, BindingTable, Phase, Phases, Resolution};
pub use datum::{Datum, DatumKind, PlainBuilder, symbol_reads_plain};
pub use ident::is_identifier;
pub use kind::SyntaxKind;
pub use lexer::{LexError, LexErrorKind, Lexed, Token, lex};
pub use origin::Origin;
pub use parser::{EventSink, ParseError, ParseErrorKind, parse};
pub use reader::{
    DatumBuilder, DatumSink, ParsedTree, Read, ReadError, ReadErrorKind, ReadFirst, parse_tree,
    read, read_first, read_first_with, read_with,
};
pub use scope::{Remap, Scope, ScopeContext, ScopeKind, ScopeSetId, TooManyScopes};
pub use span::{Location, SourceId, Sources, Span};
pub use syntax::{Effect, Exposed, NewNode, Node, Syntax, bound_identifier_eq, free_identifier_eq};
pub use text_size::{TextRange, TextSize};
pub use tree::{SyntaxElement, SyntaxNode, SyntaxToken, TreeSink, ValiLanguage};
pub use walk::{Split, TreeMap, map_tree};
