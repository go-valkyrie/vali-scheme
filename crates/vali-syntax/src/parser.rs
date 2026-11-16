//! The parser (ADR 0014): turns tokens into a stream of events for a sink,
//! never stopping at an error.
//!
//! The grammar, over the lexer's tokens:
//!
//! ```text
//! file          = item* EOF
//! item          = datum | datum-comment | Directive
//! datum         = atom | list | vector | bytevector | abbreviation | labelled | LabelRef
//! list          = "(" item* [ "." datum item* ] ")"       -- or "[" … "]"
//! vector        = "#(" item* ")"
//! bytevector    = "#u8(" item* ")"
//! abbreviation  = prefix datum                           -- ' ` , ,@ #' #` #, #,@
//! labelled      = LabelDef datum
//! datum-comment = "#;" datum
//! ```
//!
//! It needs one token of lookahead and never wraps a node it has already
//! finished, so events go straight to the sink, with no list in between.

use text_size::TextRange;

use crate::{Lexed, SyntaxKind, Token};

/// Receives the parser's events, in source order.
pub trait EventSink {
    /// A node of `kind` starts. Its tokens and child nodes follow, until the
    /// matching [`finish_node`](EventSink::finish_node).
    fn start_node(&mut self, kind: SyntaxKind);
    /// The next token, trivia included. Every token is delivered exactly
    /// once, in order.
    fn token(&mut self, token: Token, text: &str);
    /// The most recently started node that is still open finishes.
    fn finish_node(&mut self);
    /// A syntax error.
    fn error(&mut self, error: ParseError);
}

/// A syntax error the parser found and recovered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What is wrong.
    pub kind: ParseErrorKind,
    /// Where.
    pub range: TextRange,
}

/// The kinds of [`ParseError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseErrorKind {
    /// A `)` or `]` with nothing open for it to close.
    UnexpectedCloser,
    /// A `)` or `]` that doesn't match the open bracket, as in `(a ]`.
    MismatchedCloser,
    /// The input ends inside a list, vector or bytevector. The range is the
    /// opening bracket.
    Unclosed,
    /// A `.` outside a list: at the top level, or in a vector or bytevector.
    DotOutsideList,
    /// A `.` with no datum before it, as in `( . a)`.
    DotWithoutHead,
    /// A `.` with no datum after it, as in `(a . )`.
    DotWithoutTail,
    /// More than one datum after a `.`, as in `(a . b c)`.
    ExtraDatumAfterDot,
    /// A prefix, label or `#;` with no datum after it, as in `(a ')`.
    MissingDatum,
}

impl std::fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ParseErrorKind::UnexpectedCloser => "closing bracket with nothing open to close",
            ParseErrorKind::MismatchedCloser => "closing bracket does not match the open bracket",
            ParseErrorKind::Unclosed => "bracket is never closed",
            ParseErrorKind::DotOutsideList => "`.` is only allowed in a list",
            ParseErrorKind::DotWithoutHead => "`.` needs a datum before it",
            ParseErrorKind::DotWithoutTail => "`.` needs a datum after it",
            ParseErrorKind::ExtraDatumAfterDot => "only one datum may follow `.`",
            ParseErrorKind::MissingDatum => "expected a datum here",
        })
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {:?}", self.kind, self.range)
    }
}

impl std::error::Error for ParseError {}

/// Parses `lexed`, the tokens of `source`, and sends the events to `sink`:
/// one `Root` node holding every token.
pub fn parse(source: &str, lexed: &Lexed, sink: &mut impl EventSink) {
    let mut parser = Parser {
        source,
        tokens: &lexed.tokens,
        pos: 0,
        sink,
        closers: Vec::new(),
    };
    parser.file();
}

struct Parser<'a, S> {
    source: &'a str,
    tokens: &'a [Token],
    pos: usize,
    sink: &'a mut S,
    /// The closing bracket each open list, vector or bytevector expects,
    /// innermost last.
    closers: Vec<SyntaxKind>,
}

/// Where a list is relative to its `.`, if it has one.
enum Dotted {
    No,
    AwaitingTail { dot: TextRange },
    Complete,
}

impl<S: EventSink> Parser<'_, S> {
    fn peek(&self) -> Option<SyntaxKind> {
        self.tokens.get(self.pos).map(|t| t.kind)
    }

    fn range(&self) -> TextRange {
        self.tokens[self.pos].range
    }

    fn bump(&mut self) {
        let token = self.tokens[self.pos];
        self.sink.token(token, &self.source[token.range]);
        self.pos += 1;
    }

    fn error(&mut self, kind: ParseErrorKind, range: TextRange) {
        self.sink.error(ParseError { kind, range });
    }

    /// Wraps the current token in an `Error` node.
    fn bump_as_error(&mut self) {
        self.sink.start_node(SyntaxKind::Error);
        self.bump();
        self.sink.finish_node();
    }

    fn at_datum(&self) -> bool {
        self.peek().is_some_and(SyntaxKind::starts_datum)
    }

    /// Passes over trivia, directives and commented-out datums, which can
    /// appear anywhere a datum can.
    fn skip(&mut self) {
        loop {
            match self.peek() {
                Some(kind) if kind.is_trivia() || kind == SyntaxKind::Directive => self.bump(),
                Some(SyntaxKind::DatumComment) => {
                    let range = self.range();
                    self.sink.start_node(SyntaxKind::CommentedDatum);
                    self.bump();
                    // `#; #; a b` comments out both `a` and `b`.
                    self.skip();
                    if self.at_datum() {
                        self.datum();
                    } else {
                        self.error(ParseErrorKind::MissingDatum, range);
                    }
                    self.sink.finish_node();
                }
                _ => return,
            }
        }
    }

    fn file(&mut self) {
        self.sink.start_node(SyntaxKind::Root);
        loop {
            self.skip();
            match self.peek() {
                None => break,
                Some(SyntaxKind::RParen | SyntaxKind::RBracket) => {
                    self.error(ParseErrorKind::UnexpectedCloser, self.range());
                    self.bump_as_error();
                }
                Some(SyntaxKind::Dot) => {
                    self.error(ParseErrorKind::DotOutsideList, self.range());
                    self.bump_as_error();
                }
                Some(_) if self.at_datum() => self.datum(),
                Some(_) => self.bump_as_error(),
            }
        }
        self.sink.finish_node();
    }

    /// One datum, with the cursor on its first token.
    fn datum(&mut self) {
        match self.peek() {
            Some(SyntaxKind::LParen) => self.sequence(SyntaxKind::List, SyntaxKind::RParen),
            Some(SyntaxKind::LBracket) => self.sequence(SyntaxKind::List, SyntaxKind::RBracket),
            Some(SyntaxKind::VectorOpen) => self.sequence(SyntaxKind::Vector, SyntaxKind::RParen),
            Some(SyntaxKind::BytevectorOpen) => {
                self.sequence(SyntaxKind::Bytevector, SyntaxKind::RParen)
            }
            Some(SyntaxKind::LabelDef) => self.prefixed(SyntaxKind::Labelled),
            Some(kind) if kind.abbreviation_symbol().is_some() => {
                self.prefixed(SyntaxKind::Abbreviation)
            }
            _ => self.bump(),
        }
    }

    /// A prefix (an abbreviation or a label), then the datum it applies to.
    fn prefixed(&mut self, kind: SyntaxKind) {
        let prefix = self.range();
        self.sink.start_node(kind);
        self.bump();
        self.skip();
        if self.at_datum() {
            self.datum();
        } else {
            self.error(ParseErrorKind::MissingDatum, prefix);
        }
        self.sink.finish_node();
    }

    /// A list, vector or bytevector, with the cursor on its opening bracket.
    /// Only lists may contain a `.`.
    fn sequence(&mut self, kind: SyntaxKind, closer: SyntaxKind) {
        let opener = self.range();
        self.sink.start_node(kind);
        self.bump();
        self.closers.push(closer);
        let mut items = 0usize;
        let mut dotted = Dotted::No;
        loop {
            self.skip();
            match self.peek() {
                None => {
                    self.error(ParseErrorKind::Unclosed, opener);
                    break;
                }
                Some(found @ (SyntaxKind::RParen | SyntaxKind::RBracket)) => {
                    if found == closer {
                        if let Dotted::AwaitingTail { dot } = dotted {
                            self.error(ParseErrorKind::DotWithoutTail, dot);
                        }
                        self.bump();
                        break;
                    }
                    self.error(ParseErrorKind::MismatchedCloser, self.range());
                    let enclosing = &self.closers[..self.closers.len() - 1];
                    if enclosing.contains(&found) {
                        // It closes an enclosing bracket: leave it for that one.
                        break;
                    }
                    self.bump_as_error();
                }
                Some(SyntaxKind::Dot) => {
                    let dot = self.range();
                    match dotted {
                        _ if kind != SyntaxKind::List => {
                            self.error(ParseErrorKind::DotOutsideList, dot);
                            self.bump_as_error();
                        }
                        Dotted::No => {
                            if items == 0 {
                                self.error(ParseErrorKind::DotWithoutHead, dot);
                            }
                            self.bump();
                            dotted = Dotted::AwaitingTail { dot };
                        }
                        Dotted::AwaitingTail { .. } | Dotted::Complete => {
                            self.error(ParseErrorKind::ExtraDatumAfterDot, dot);
                            self.bump_as_error();
                        }
                    }
                }
                Some(_) if self.at_datum() => match dotted {
                    Dotted::No => {
                        self.datum();
                        items += 1;
                    }
                    Dotted::AwaitingTail { .. } => {
                        self.datum();
                        dotted = Dotted::Complete;
                    }
                    Dotted::Complete => {
                        self.error(ParseErrorKind::ExtraDatumAfterDot, self.range());
                        self.sink.start_node(SyntaxKind::Error);
                        self.datum();
                        self.sink.finish_node();
                    }
                },
                Some(_) => self.bump_as_error(),
            }
        }
        self.closers.pop();
        self.sink.finish_node();
    }
}
