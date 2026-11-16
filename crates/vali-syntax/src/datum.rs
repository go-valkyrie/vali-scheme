//! A plain tree of datums, built by [`PlainBuilder`], for tests and tools.

use std::fmt::{self, Write};

use vali_num::{Number, Radix};

use crate::reader::DatumBuilder;
use crate::{Span, is_identifier};

/// A datum and where it came from.
#[derive(Clone, Debug)]
pub struct Datum {
    /// What the datum is.
    pub kind: DatumKind,
    /// Where it was read from.
    pub span: Span,
}

/// The kinds of [`Datum`]. Labels stay explicit, rather than becoming
/// shared or circular structure.
#[derive(Clone, Debug)]
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
impl fmt::Display for Datum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DatumKind::Number(n) => write!(f, "{n}"),
            DatumKind::Boolean(b) => f.write_str(if *b { "#t" } else { "#f" }),
            DatumKind::Character(c) => write_character(f, *c),
            DatumKind::String(s) => write_string(f, s),
            DatumKind::Symbol(name) => write_symbol(f, name),
            DatumKind::Keyword(name) => write!(f, "{name}:"),
            DatumKind::List { items, tail } => {
                f.write_char('(')?;
                write_items(f, items)?;
                if let Some(tail) = tail {
                    write!(f, " . {tail}")?;
                }
                f.write_char(')')
            }
            DatumKind::Vector(items) => {
                f.write_str("#(")?;
                write_items(f, items)?;
                f.write_char(')')
            }
            DatumKind::Bytevector(bytes) => {
                f.write_str("#u8(")?;
                for (i, b) in bytes.iter().enumerate() {
                    if i > 0 {
                        f.write_char(' ')?;
                    }
                    write!(f, "{b}")?;
                }
                f.write_char(')')
            }
            DatumKind::Labelled(label, datum) => write!(f, "#{label}={datum}"),
            DatumKind::LabelRef(label) => write!(f, "#{label}#"),
        }
    }
}

fn write_items(f: &mut fmt::Formatter<'_>, items: &[Datum]) -> fmt::Result {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            f.write_char(' ')?;
        }
        write!(f, "{item}")?;
    }
    Ok(())
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

/// Writes a symbol plainly when that reads back as the same symbol, and
/// between vertical bars otherwise: for names that would read as numbers,
/// keywords or `.`, or that aren't identifiers.
fn write_symbol(f: &mut fmt::Formatter<'_>, name: &str) -> fmt::Result {
    let plain = is_identifier(name)
        && vali_num::parse(name, Radix::Decimal).is_err()
        && !(name.len() > 1 && name.ends_with(':'));
    if plain {
        return f.write_str(name);
    }
    f.write_char('|')?;
    for c in name.chars() {
        write_escaped(f, c, '|')?;
    }
    f.write_char('|')
}
