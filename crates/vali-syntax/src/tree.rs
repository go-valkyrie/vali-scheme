//! The tree sink (ADR 0014): builds a lossless Rowan tree for tools.

use std::collections::HashMap;

use rowan::{GreenNode, GreenToken, Language, NodeOrToken};

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

type GreenElement = NodeOrToken<GreenNode, GreenToken>;

/// An [`EventSink`] that builds a Rowan tree keeping every byte of the
/// source, for language servers, formatters and other tools.
///
/// It builds green nodes itself rather than with Rowan's builder, whose
/// node cache rehashes trees recursively: deep nesting would overflow the
/// stack there, and take quadratic time. Equal tokens are still shared.
#[derive(Default)]
pub struct TreeSink {
    /// The open nodes, innermost last: each one's kind and the children it
    /// has so far.
    open: Vec<(SyntaxKind, Vec<GreenElement>)>,
    /// The finished root.
    root: Option<GreenNode>,
    /// Every distinct token so far.
    tokens: HashMap<(SyntaxKind, Box<str>), GreenToken>,
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
    ///
    /// # Panics
    ///
    /// If the parser has not finished its root node.
    pub fn finish(self) -> (SyntaxNode, Vec<ParseError>) {
        let green = self.root.expect("the parser finishes its root node");
        (SyntaxNode::new_root(green), self.errors)
    }
}

impl EventSink for TreeSink {
    fn start_node(&mut self, kind: SyntaxKind) {
        self.open.push((kind, Vec::new()));
    }

    fn token(&mut self, token: Token, text: &str) {
        let key = (token.kind, Box::from(text));
        let green = self
            .tokens
            .entry(key)
            .or_insert_with(|| GreenToken::new(ValiLanguage::kind_to_raw(token.kind), text))
            .clone();
        self.open
            .last_mut()
            .expect("tokens arrive inside the root node")
            .1
            .push(NodeOrToken::Token(green));
    }

    fn finish_node(&mut self) {
        let (kind, children) = self.open.pop().expect("nodes are balanced");
        let node = GreenNode::new(ValiLanguage::kind_to_raw(kind), children);
        match self.open.last_mut() {
            Some((_, siblings)) => siblings.push(NodeOrToken::Node(node)),
            None => self.root = Some(node),
        }
    }

    fn error(&mut self, error: ParseError) {
        self.errors.push(error);
    }
}
