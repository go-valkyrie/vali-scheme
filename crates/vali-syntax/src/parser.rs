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
//! It keeps the nodes it is inside on a stack of its own, never recursing,
//! so deep nesting cannot overflow the native stack.

use std::iter::Peekable;

use text_size::{TextRange, TextSize};

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
    Parser::new(source, lexed.tokens.iter().copied(), sink).run(false);
}

/// Parses the first top-level item of `source` from `tokens`: trivia,
/// directives and commented-out datums, then one datum, or one stray token
/// in error. The events are one `Root` node holding those tokens alone.
///
/// Returns where parsing stopped: the end of the last token taken, which is
/// the end of `source` if it held no datum. No token after it is lexed.
pub(crate) fn parse_first(
    source: &str,
    tokens: impl Iterator<Item = Token>,
    sink: &mut impl EventSink,
) -> TextSize {
    let mut parser = Parser::new(source, tokens, sink);
    parser.run(true);
    parser.end
}

/// What the parser is inside, besides the root. The parser keeps these on
/// a stack of its own rather than recursing, so nesting is limited only by
/// memory (ADR 0014).
#[derive(Clone, Copy)]
enum Frame {
    /// A list, vector or bytevector. Only lists may contain a `.`.
    Sequence {
        kind: SyntaxKind,
        /// The closing bracket it expects.
        closer: SyntaxKind,
        /// Its opening bracket.
        opener: TextRange,
        /// How many datums it holds before any `.`.
        items: usize,
        dotted: Dotted,
    },
    /// A prefix (an abbreviation or a label), waiting for its datum.
    Prefixed { prefix: TextRange },
    /// A `#;`, waiting for the datum it comments out.
    Commented { prefix: TextRange },
    /// An `Error` node around a datum after a list's tail, as in
    /// `(a . b c)`.
    Extra,
}

/// Where a list is relative to its `.`, if it has one.
#[derive(Clone, Copy)]
enum Dotted {
    No,
    AwaitingTail { dot: TextRange },
    Complete,
}

struct Parser<'a, S, I: Iterator<Item = Token>> {
    source: &'a str,
    tokens: Peekable<I>,
    sink: &'a mut S,
    /// The open nodes, innermost last.
    stack: Vec<Frame>,
    /// How many open sequences expect `)`, and how many `]`.
    open_parens: usize,
    open_brackets: usize,
    /// The end of the last token taken.
    end: TextSize,
}

impl<'a, S: EventSink, I: Iterator<Item = Token>> Parser<'a, S, I> {
    fn new(source: &'a str, tokens: I, sink: &'a mut S) -> Self {
        Parser {
            source,
            tokens: tokens.peekable(),
            sink,
            stack: Vec::new(),
            open_parens: 0,
            open_brackets: 0,
            end: TextSize::from(0),
        }
    }

    fn peek(&mut self) -> Option<SyntaxKind> {
        self.tokens.peek().map(|t| t.kind)
    }

    /// The range of the current token. Only called when there is one.
    fn range(&mut self) -> TextRange {
        self.tokens.peek().expect("a current token").range
    }

    fn bump(&mut self) {
        let token = self.tokens.next().expect("a current token");
        self.end = token.range.end();
        self.sink.token(token, &self.source[token.range]);
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

    fn at_datum(&mut self) -> bool {
        self.peek().is_some_and(SyntaxKind::starts_datum)
    }

    fn open_count(&mut self, closer: SyntaxKind) -> &mut usize {
        if closer == SyntaxKind::RBracket {
            &mut self.open_brackets
        } else {
            &mut self.open_parens
        }
    }

    /// Parses items until the input ends, or, if `first_only`, until one
    /// top-level item has been parsed.
    fn run(&mut self, first_only: bool) {
        self.sink.start_node(SyntaxKind::Root);
        loop {
            // Trivia, directives and commented-out datums can appear
            // anywhere a datum can.
            match self.peek() {
                Some(kind) if kind.is_trivia() || kind == SyntaxKind::Directive => {
                    self.bump();
                    continue;
                }
                Some(SyntaxKind::DatumComment) => {
                    let prefix = self.range();
                    self.sink.start_node(SyntaxKind::CommentedDatum);
                    self.bump();
                    // `#; #; a b` comments out both `a` and `b`: the inner
                    // one finishes first, and the outer takes the next.
                    self.stack.push(Frame::Commented { prefix });
                    continue;
                }
                _ => {}
            }
            let top_level_item = match self.stack.last().copied() {
                None => match self.root_item() {
                    Some(done) => done,
                    None => break,
                },
                Some(Frame::Sequence {
                    kind,
                    closer,
                    opener,
                    items,
                    dotted,
                }) => self.sequence_item(kind, closer, opener, items, dotted),
                Some(Frame::Prefixed { prefix }) => {
                    if self.at_datum() {
                        self.start_datum()
                    } else {
                        self.error(ParseErrorKind::MissingDatum, prefix);
                        self.close()
                    }
                }
                Some(Frame::Commented { prefix }) => {
                    if self.at_datum() {
                        self.start_datum()
                    } else {
                        self.error(ParseErrorKind::MissingDatum, prefix);
                        self.close()
                    }
                }
                Some(Frame::Extra) => unreachable!("an Extra frame starts with its datum"),
            };
            if first_only && top_level_item {
                break;
            }
        }
        self.sink.finish_node();
    }

    /// One item at the top level. Returns `None` at the end of the input,
    /// and otherwise whether a top-level item is now complete.
    fn root_item(&mut self) -> Option<bool> {
        Some(match self.peek()? {
            SyntaxKind::RParen | SyntaxKind::RBracket => {
                let range = self.range();
                self.error(ParseErrorKind::UnexpectedCloser, range);
                self.bump_as_error();
                true
            }
            SyntaxKind::Dot => {
                let range = self.range();
                self.error(ParseErrorKind::DotOutsideList, range);
                self.bump_as_error();
                true
            }
            _ if self.at_datum() => self.start_datum(),
            _ => {
                self.bump_as_error();
                true
            }
        })
    }

    /// The next item inside the innermost sequence. Returns whether a
    /// top-level item is now complete.
    fn sequence_item(
        &mut self,
        kind: SyntaxKind,
        closer: SyntaxKind,
        opener: TextRange,
        items: usize,
        dotted: Dotted,
    ) -> bool {
        match self.peek() {
            None => {
                self.error(ParseErrorKind::Unclosed, opener);
                self.close()
            }
            Some(found @ (SyntaxKind::RParen | SyntaxKind::RBracket)) => {
                if found == closer {
                    if let Dotted::AwaitingTail { dot } = dotted {
                        self.error(ParseErrorKind::DotWithoutTail, dot);
                    }
                    self.bump();
                    return self.close();
                }
                let range = self.range();
                self.error(ParseErrorKind::MismatchedCloser, range);
                if *self.open_count(found) > 0 {
                    // It closes an enclosing bracket: leave it for that one.
                    return self.close();
                }
                self.bump_as_error();
                false
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
                        self.set_dotted(Dotted::AwaitingTail { dot });
                    }
                    Dotted::AwaitingTail { .. } | Dotted::Complete => {
                        self.error(ParseErrorKind::ExtraDatumAfterDot, dot);
                        self.bump_as_error();
                    }
                }
                false
            }
            Some(_) if self.at_datum() => {
                if let Dotted::Complete = dotted {
                    let range = self.range();
                    self.error(ParseErrorKind::ExtraDatumAfterDot, range);
                    self.sink.start_node(SyntaxKind::Error);
                    self.stack.push(Frame::Extra);
                }
                self.start_datum()
            }
            Some(_) => {
                self.bump_as_error();
                false
            }
        }
    }

    fn set_dotted(&mut self, to: Dotted) {
        if let Some(Frame::Sequence { dotted, .. }) = self.stack.last_mut() {
            *dotted = to;
        }
    }

    /// Starts the datum at the cursor. An atom is complete at once; a list,
    /// vector, bytevector, abbreviation or label opens a frame. Returns
    /// whether a top-level item is now complete.
    fn start_datum(&mut self) -> bool {
        let (kind, closer) = match self.peek() {
            Some(SyntaxKind::LParen) => (SyntaxKind::List, SyntaxKind::RParen),
            Some(SyntaxKind::LBracket) => (SyntaxKind::List, SyntaxKind::RBracket),
            Some(SyntaxKind::VectorOpen) => (SyntaxKind::Vector, SyntaxKind::RParen),
            Some(SyntaxKind::BytevectorOpen) => (SyntaxKind::Bytevector, SyntaxKind::RParen),
            Some(prefix @ SyntaxKind::LabelDef) => return self.open_prefixed(prefix),
            Some(prefix) if prefix.abbreviation_symbol().is_some() => {
                return self.open_prefixed(prefix);
            }
            _ => {
                self.bump();
                return self.completed();
            }
        };
        let opener = self.range();
        self.sink.start_node(kind);
        self.bump();
        *self.open_count(closer) += 1;
        self.stack.push(Frame::Sequence {
            kind,
            closer,
            opener,
            items: 0,
            dotted: Dotted::No,
        });
        false
    }

    fn open_prefixed(&mut self, prefix: SyntaxKind) -> bool {
        let range = self.range();
        self.sink.start_node(if prefix == SyntaxKind::LabelDef {
            SyntaxKind::Labelled
        } else {
            SyntaxKind::Abbreviation
        });
        self.bump();
        self.stack.push(Frame::Prefixed { prefix: range });
        false
    }

    /// Finishes the innermost frame's node, and every frame that finishes
    /// with it, as `'''a` does. Returns whether a top-level item is now
    /// complete.
    fn close(&mut self) -> bool {
        loop {
            let frame = self.stack.pop().expect("a frame to close");
            if let Frame::Sequence { closer, .. } = frame {
                *self.open_count(closer) -= 1;
            }
            self.sink.finish_node();
            match frame {
                Frame::Sequence { .. } | Frame::Prefixed { .. } => {}
                // A commented-out datum is no datum, and an extra one does
                // not change the list it is in.
                Frame::Commented { .. } | Frame::Extra => return false,
            }
            if let Some(done) = self.note_datum() {
                return done;
            }
        }
    }

    /// A datum just finished: tells the frame around it. Returns whether a
    /// top-level item is now complete.
    fn completed(&mut self) -> bool {
        match self.note_datum() {
            Some(done) => done,
            None => self.close(),
        }
    }

    /// Tells the innermost frame that a datum inside it finished. Returns
    /// `None` if that finishes the frame too, and otherwise whether a
    /// top-level item is now complete.
    fn note_datum(&mut self) -> Option<bool> {
        match self.stack.last_mut() {
            None => Some(true),
            Some(Frame::Sequence { items, dotted, .. }) => {
                match dotted {
                    Dotted::No => *items += 1,
                    Dotted::AwaitingTail { .. } => *dotted = Dotted::Complete,
                    Dotted::Complete => unreachable!("an extra datum is in an Extra frame"),
                }
                Some(false)
            }
            Some(Frame::Prefixed { .. } | Frame::Commented { .. } | Frame::Extra) => None,
        }
    }
}
