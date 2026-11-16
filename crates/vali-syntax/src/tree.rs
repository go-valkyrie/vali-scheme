//! The tree sink (ADR 0014): builds a lossless Rowan tree for tools.

use rowan::{GreenNode, GreenNodeBuilder, Language};

use crate::parser::{EventSink, ParseError};
use crate::{SyntaxKind, Token};

/// Vali's language, for Rowan's typed trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValiLanguage {}

impl Language for ValiLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::from_u16(raw.0).expect("Rowan only holds kinds this crate gave it")
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

/// A node of Vali's lossless syntax tree.
pub type SyntaxNode = rowan::SyntaxNode<ValiLanguage>;
/// A token of Vali's lossless syntax tree.
pub type SyntaxToken = rowan::SyntaxToken<ValiLanguage>;
/// A node or a token of Vali's lossless syntax tree.
pub type SyntaxElement = rowan::SyntaxElement<ValiLanguage>;

/// An [`EventSink`] that builds a Rowan tree keeping every byte of the
/// source, for language servers, formatters and other tools.
#[derive(Default)]
pub struct TreeSink {
    builder: GreenNodeBuilder<'static>,
    errors: Vec<ParseError>,
}

impl std::fmt::Debug for TreeSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeSink")
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl TreeSink {
    /// The finished tree and the syntax errors reported while building it.
    pub fn finish(self) -> (SyntaxNode, Vec<ParseError>) {
        let green: GreenNode = self.builder.finish();
        (SyntaxNode::new_root(green), self.errors)
    }
}

impl EventSink for TreeSink {
    fn start_node(&mut self, kind: SyntaxKind) {
        self.builder.start_node(ValiLanguage::kind_to_raw(kind));
    }

    fn token(&mut self, token: Token, text: &str) {
        self.builder
            .token(ValiLanguage::kind_to_raw(token.kind), text);
    }

    fn finish_node(&mut self) {
        self.builder.finish_node();
    }

    fn error(&mut self, error: ParseError) {
        self.errors.push(error);
    }
}
