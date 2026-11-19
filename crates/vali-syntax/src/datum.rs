//! A plain tree of datums, built by [`PlainBuilder`], for tests and tools.

use std::fmt::{self, Write};

use vali_num::{Number, Radix};

use crate::reader::DatumBuilder;
use crate::{Span, is_identifier};

/// A datum and where it came from.
#[derive(Debug)]
pub struct Datum {
    /// What the datum is.
    pub kind: DatumKind,
    /// Where it was read from.
    pub span: Span,
}

/// The kinds of [`Datum`]. Labels stay explicit, rather than becoming
/// shared or circular structure.
#[derive(Debug)]
#[non_exhaustive]
pub enum DatumKind {
    /// A number.
    Number(Number),
    /// `#t` or `#f`.
    Boolean(bool),
    /// A character.
    Character(char),
    /// A string, with its escapes decoded.
    String(String),
    /// A symbol, case-folded if `#!fold-case` was in effect, and in NFC.
    Symbol(String),
    /// An SRFI 88 keyword, named without its colon.
    Keyword(String),
    /// A list, proper when `tail` is `None` and dotted otherwise.
    List {
        /// The elements before any `.`.
        items: Vec<Datum>,
        /// The datum after the `.`, if there is one.
        tail: Option<Box<Datum>>,
    },
    /// A vector.
    Vector(Vec<Datum>),
    /// A bytevector.
    Bytevector(Vec<u8>),
    /// `#n=datum`: a datum given label `n`.
    Labelled(u64, Box<Datum>),
    /// `#n#`: a reference to the datum labelled `n`.
    LabelRef(u64),
}

/// Drops the datums inside a datum one at a time, from a list of its own,
/// so that dropping a deeply nested datum cannot overflow the stack.
impl Drop for Datum {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        take_children(&mut self.kind, &mut pending);
        while let Some(mut datum) = pending.pop() {
            // `datum` is left without children, so dropping it at the end
            // of this iteration recurses no further.
            take_children(&mut datum.kind, &mut pending);
        }
    }
}

/// Moves the datums directly inside `kind` to `out`.
fn take_children(kind: &mut DatumKind, out: &mut Vec<Datum>) {
    match kind {
        DatumKind::List { items, tail } => {
            out.append(items);
            out.extend(tail.take().map(|tail| *tail));
        }
        DatumKind::Vector(items) => out.append(items),
        DatumKind::Labelled(_, inner) => {
            let inner = std::mem::replace(&mut **inner, Datum::leaf());
            out.push(inner);
        }
        _ => {}
    }
}
/// Clones a datum with a stack of its own, so that cloning a deeply nested
/// datum cannot overflow the native stack.
impl Clone for Datum {
    fn clone(&self) -> Datum {
        Datum {
            kind: self.kind.clone(),
            span: self.span,
        }
    }
}

/// As for [`Datum`]: the parts are cloned from a list of the cloning still
/// to do.
impl Clone for DatumKind {
    fn clone(&self) -> DatumKind {
        /// A node with parts, partly cloned.
        enum Frame<'a> {
            List {
                span: Option<Span>,
                rest: std::slice::Iter<'a, Datum>,
                items: Vec<Datum>,
                tail: Option<&'a Datum>,
                tail_started: bool,
                tail_done: Option<Datum>,
            },
            Vector {
                span: Option<Span>,
                rest: std::slice::Iter<'a, Datum>,
                items: Vec<Datum>,
            },
            Labelled {
                span: Option<Span>,
                label: u64,
                inner: Option<&'a Datum>,
                done: Option<Datum>,
            },
        }

        /// What cloning a node starts with.
        enum Started<'a> {
            /// A leaf, cloned.
            Leaf(DatumKind),
            /// A node with parts, and the frame to clone it by.
            Open(Frame<'a>),
        }

        /// A leaf cloned, or the frame to clone a node with parts by.
        fn start(kind: &DatumKind, span: Option<Span>) -> Started<'_> {
            Started::Leaf(match kind {
                DatumKind::List { items, tail } => {
                    return Started::Open(Frame::List {
                        span,
                        rest: items.iter(),
                        items: Vec::with_capacity(items.len()),
                        tail: tail.as_deref(),
                        tail_started: false,
                        tail_done: None,
                    });
                }
                DatumKind::Vector(items) => {
                    return Started::Open(Frame::Vector {
                        span,
                        rest: items.iter(),
                        items: Vec::with_capacity(items.len()),
                    });
                }
                DatumKind::Labelled(label, inner) => {
                    return Started::Open(Frame::Labelled {
                        span,
                        label: *label,
                        inner: Some(inner),
                        done: None,
                    });
                }
                DatumKind::Number(n) => DatumKind::Number(n.clone()),
                DatumKind::Boolean(b) => DatumKind::Boolean(*b),
                DatumKind::Character(c) => DatumKind::Character(*c),
                DatumKind::String(s) => DatumKind::String(s.clone()),
                DatumKind::Symbol(s) => DatumKind::Symbol(s.clone()),
                DatumKind::Keyword(s) => DatumKind::Keyword(s.clone()),
                DatumKind::Bytevector(b) => DatumKind::Bytevector(b.clone()),
                DatumKind::LabelRef(label) => DatumKind::LabelRef(*label),
            })
        }

        let mut stack: Vec<Frame<'_>> = Vec::new();
        // A node cloned, and its span unless it is the root.
        let mut finished = match start(self, None) {
            Started::Leaf(kind) => Some((kind, None)),
            Started::Open(frame) => {
                stack.push(frame);
                None
            }
        };
        loop {
            if let Some((kind, span)) = finished.take() {
                let Some(parent) = stack.last_mut() else {
                    return kind;
                };
                let child = Datum {
                    kind,
                    span: span.expect("a part has a span"),
                };
                match parent {
                    Frame::List {
                        items,
                        tail_started,
                        tail_done,
                        ..
                    } => {
                        if *tail_started {
                            *tail_done = Some(child);
                        } else {
                            items.push(child);
                        }
                    }
                    Frame::Vector { items, .. } => items.push(child),
                    Frame::Labelled { done, .. } => *done = Some(child),
                }
            }
            let top = stack.last_mut().expect("a frame to continue");
            let next = match top {
                Frame::List {
                    rest,
                    tail,
                    tail_started,
                    ..
                } => rest.next().or_else(|| {
                    let tail = tail.take();
                    *tail_started = tail.is_some();
                    tail
                }),
                Frame::Vector { rest, .. } => rest.next(),
                Frame::Labelled { inner, .. } => inner.take(),
            };
            match next {
                Some(part) => match start(&part.kind, Some(part.span)) {
                    Started::Leaf(kind) => finished = Some((kind, Some(part.span))),
                    Started::Open(frame) => stack.push(frame),
                },
                None => {
                    finished = Some(match stack.pop().expect("the top frame") {
                        Frame::List {
                            span,
                            items,
                            tail_done,
                            ..
                        } => (
                            DatumKind::List {
                                items,
                                tail: tail_done.map(Box::new),
                            },
                            span,
                        ),
                        Frame::Vector { span, items, .. } => (DatumKind::Vector(items), span),
                        Frame::Labelled {
                            span, label, done, ..
                        } => (
                            DatumKind::Labelled(label, Box::new(done.expect("its datum"))),
                            span,
                        ),
                    });
                }
            }
        }
    }
}

impl Datum {
    /// A datum with nothing inside, to leave in place of one taken out.
    fn leaf() -> Datum {
        Datum {
            kind: DatumKind::Boolean(false),
            span: Span {
                source: crate::SourceId(0),
                range: text_size::TextRange::default(),
            },
        }
    }
}

/// A [`DatumBuilder`] that builds plain [`Datum`]s.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainBuilder;

impl DatumBuilder for PlainBuilder {
    type Datum = Datum;

    fn number(&mut self, value: Number, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Number(value),
            span,
        }
    }

    fn boolean(&mut self, value: bool, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Boolean(value),
            span,
        }
    }

    fn character(&mut self, value: char, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Character(value),
            span,
        }
    }

    fn string(&mut self, value: String, span: Span) -> Datum {
        Datum {
            kind: DatumKind::String(value),
            span,
        }
    }

    fn symbol(&mut self, name: &str, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Symbol(name.to_owned()),
            span,
        }
    }

    fn keyword(&mut self, name: &str, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Keyword(name.to_owned()),
            span,
        }
    }

    fn list(&mut self, items: Vec<Datum>, tail: Option<Datum>, span: Span) -> Datum {
        let tail = tail.map(Box::new);
        Datum {
            kind: DatumKind::List { items, tail },
            span,
        }
    }

    fn vector(&mut self, items: Vec<Datum>, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Vector(items),
            span,
        }
    }

    fn bytevector(&mut self, bytes: Vec<u8>, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Bytevector(bytes),
            span,
        }
    }

    fn label_def(&mut self, label: u64, datum: Datum, span: Span) -> Datum {
        Datum {
            kind: DatumKind::Labelled(label, Box::new(datum)),
            span,
        }
    }

    fn label_ref(&mut self, label: u64, span: Span) -> Datum {
        Datum {
            kind: DatumKind::LabelRef(label),
            span,
        }
    }
}

/// Writes the datum in Scheme's external syntax, as `write` would, so that
/// reading the text back gives an equal datum.
///
/// The datums inside wait on a list of their own, so a deeply nested datum
/// cannot overflow the stack.
impl fmt::Display for Datum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut work = vec![Task::Datum(self)];
        while let Some(task) = work.pop() {
            let datum = match task {
                Task::Text(text) => {
                    f.write_str(text)?;
                    continue;
                }
                Task::Datum(datum) => datum,
            };
            match &datum.kind {
                DatumKind::Number(n) => write!(f, "{n}")?,
                DatumKind::Boolean(b) => f.write_str(if *b { "#t" } else { "#f" })?,
                DatumKind::Character(c) => write_character(f, *c)?,
                DatumKind::String(s) => write_string(f, s)?,
                DatumKind::Symbol(name) => write_symbol(f, name)?,
                DatumKind::Keyword(name) => write!(f, "{name}:")?,
                DatumKind::List { items, tail } => {
                    f.write_char('(')?;
                    work.push(Task::Text(")"));
                    if let Some(tail) = tail {
                        work.push(Task::Datum(tail));
                        work.push(Task::Text(" . "));
                    }
                    push_items(&mut work, items);
                }
                DatumKind::Vector(items) => {
                    f.write_str("#(")?;
                    work.push(Task::Text(")"));
                    push_items(&mut work, items);
                }
                DatumKind::Bytevector(bytes) => {
                    f.write_str("#u8(")?;
                    for (i, b) in bytes.iter().enumerate() {
                        if i > 0 {
                            f.write_char(' ')?;
                        }
                        write!(f, "{b}")?;
                    }
                    f.write_char(')')?;
                }
                DatumKind::Labelled(label, inner) => {
                    write!(f, "#{label}=")?;
                    work.push(Task::Datum(inner));
                }
                DatumKind::LabelRef(label) => write!(f, "#{label}#")?,
            }
        }
        Ok(())
    }
}

/// What is left to write.
enum Task<'a> {
    Datum(&'a Datum),
    Text(&'static str),
}

/// Schedules `items`, spaced, to be written in order.
fn push_items<'a>(work: &mut Vec<Task<'a>>, items: &'a [Datum]) {
    for (i, item) in items.iter().enumerate().rev() {
        work.push(Task::Datum(item));
        if i > 0 {
            work.push(Task::Text(" "));
        }
    }
}

/// The R7RS character names, in both directions.
pub(crate) const CHARACTER_NAMES: &[(&str, char)] = &[
    ("alarm", '\u{7}'),
    ("backspace", '\u{8}'),
    ("delete", '\u{7f}'),
    ("escape", '\u{1b}'),
    ("newline", '\n'),
    ("null", '\0'),
    ("return", '\r'),
    ("space", ' '),
    ("tab", '\t'),
];

fn write_character(f: &mut fmt::Formatter<'_>, c: char) -> fmt::Result {
    if let Some((name, _)) = CHARACTER_NAMES.iter().find(|(_, named)| *named == c) {
        write!(f, "#\\{name}")
    } else if c.is_control() || c.is_whitespace() {
        write!(f, "#\\x{:x}", u32::from(c))
    } else {
        write!(f, "#\\{c}")
    }
}

fn write_string(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        write_escaped(f, c, '"')?;
    }
    f.write_char('"')
}

/// Writes `c` inside a string or a barred identifier closed by `quote`.
fn write_escaped(f: &mut fmt::Formatter<'_>, c: char, quote: char) -> fmt::Result {
    match c {
        '\\' => f.write_str("\\\\"),
        '\n' => f.write_str("\\n"),
        '\t' => f.write_str("\\t"),
        '\r' => f.write_str("\\r"),
        '\u{7}' => f.write_str("\\a"),
        '\u{8}' => f.write_str("\\b"),
        c if c == quote => write!(f, "\\{quote}"),
        c if c.is_control() => write!(f, "\\x{:x};", u32::from(c)),
        c => f.write_char(c),
    }
}

/// Whether a symbol named `name` written without vertical bars reads back
/// as that symbol: it is an identifier, and neither a number (the reader
/// tries numbers first, so `+i`, `-inf.0` and `1e3` are numbers) nor a
/// keyword.
pub fn symbol_reads_plain(name: &str) -> bool {
    is_identifier(name)
        && vali_num::parse(name, Radix::Decimal).is_err()
        && !(name.ends_with(':') && name.chars().any(|c| c != ':'))
}

/// Writes a symbol plainly when that reads back as the same symbol, and
/// between vertical bars otherwise: for names that would read as numbers,
/// keywords or `.`, or that aren't identifiers.
fn write_symbol(f: &mut fmt::Formatter<'_>, name: &str) -> fmt::Result {
    if symbol_reads_plain(name) {
        return f.write_str(name);
    }
    f.write_char('|')?;
    for c in name.chars() {
        write_escaped(f, c, '|')?;
    }
    f.write_char('|')
}
