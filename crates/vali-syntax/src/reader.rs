//! The datum sink and the reader (ADR 0014): turns parser events into
//! datums, doing the reader's semantic work on the way.

use std::borrow::Cow;
use std::ops::Range;

use text_size::{TextRange, TextSize};
use unicode_normalization::UnicodeNormalization;
use vali_num::{Number, Radix};

use crate::datum::{CHARACTER_NAMES, Datum, PlainBuilder};
use crate::parser::{EventSink, ParseError, ParseErrorKind, parse};
use crate::tree::{SyntaxNode, TreeSink};
use crate::{LexErrorKind, SourceId, Span, SyntaxKind, Token, lex};

/// Builds datums for the [`DatumSink`]. Each host decides what a datum is:
/// [`PlainBuilder`] makes a plain [`Datum`] tree, and the runtime will build
/// heap values and syntax objects directly.
pub trait DatumBuilder {
    /// What the builder builds.
    type Datum;
    /// A number.
    fn number(&mut self, value: Number, span: Span) -> Self::Datum;
    /// `#t` or `#f`.
    fn boolean(&mut self, value: bool, span: Span) -> Self::Datum;
    /// A character.
    fn character(&mut self, value: char, span: Span) -> Self::Datum;
    /// A string, with its escapes decoded.
    fn string(&mut self, value: String, span: Span) -> Self::Datum;
    /// A symbol, case-folded if `#!fold-case` is in effect, and in NFC.
    fn symbol(&mut self, name: &str, span: Span) -> Self::Datum;
    /// An SRFI 88 keyword, named without its colon.
    fn keyword(&mut self, name: &str, span: Span) -> Self::Datum;
    /// A list: proper when `tail` is `None`, dotted otherwise.
    fn list(
        &mut self,
        items: Vec<Self::Datum>,
        tail: Option<Self::Datum>,
        span: Span,
    ) -> Self::Datum;
    /// A vector.
    fn vector(&mut self, items: Vec<Self::Datum>, span: Span) -> Self::Datum;
    /// A bytevector.
    fn bytevector(&mut self, bytes: Vec<u8>, span: Span) -> Self::Datum;
    /// `#label=datum`. `datum` may already contain references to `label`.
    fn label_def(&mut self, label: u64, datum: Self::Datum, span: Span) -> Self::Datum;
    /// `#label#`, inside the datum labelled `label`, or after it in the same
    /// top-level datum.
    fn label_ref(&mut self, label: u64, span: Span) -> Self::Datum;
}

/// Why some text could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadErrorKind {
    /// An invalid token.
    Lex(LexErrorKind),
    /// A syntax error.
    Parse(ParseErrorKind),
    /// An escape in a string or barred identifier that R7RS does not
    /// define, such as `\q` or `\x110000;`.
    InvalidEscape,
    /// A character literal with an unknown name, such as `#\bell`.
    InvalidCharacter,
    /// A bytevector element that is not an exact integer from 0 to 255.
    NotAByte,
    /// A datum label too large to represent.
    LabelTooLarge,
    /// A reference to a label not defined earlier in the same datum.
    UndefinedLabel,
    /// A label defined twice in the same datum.
    DuplicateLabel,
}

impl std::fmt::Display for ReadErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadErrorKind::Lex(kind) => kind.fmt(f),
            ReadErrorKind::Parse(kind) => kind.fmt(f),
            ReadErrorKind::InvalidEscape => f.write_str("invalid escape"),
            ReadErrorKind::InvalidCharacter => f.write_str("unknown character name"),
            ReadErrorKind::NotAByte => {
                f.write_str("bytevector elements must be exact integers from 0 to 255")
            }
            ReadErrorKind::LabelTooLarge => f.write_str("datum label is too large"),
            ReadErrorKind::UndefinedLabel => f.write_str("datum label is not defined here"),
            ReadErrorKind::DuplicateLabel => {
                f.write_str("datum label is already defined in this datum")
            }
        }
    }
}

/// A problem found while reading, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadError {
    /// What is wrong.
    pub kind: ReadErrorKind,
    /// The byte range it concerns.
    pub range: TextRange,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {:?}", self.kind, self.range)
    }
}

impl std::error::Error for ReadError {}

/// The datums read from a source, and every problem found.
#[derive(Clone, Debug)]
pub struct Read<D> {
    /// The top-level datums that were read without error, in order.
    pub datums: Vec<D>,
    /// Every lexical, syntax and reader error, in source order.
    pub errors: Vec<ReadError>,
}

/// Reads every datum in `source` into plain [`Datum`]s.
pub fn read(source: &str, id: SourceId) -> Read<Datum> {
    read_with(source, id, &mut PlainBuilder)
}

/// Reads every datum in `source` with `builder`.
///
/// A top-level datum containing any error is left out, and its errors are
/// reported; reading carries on with the next one.
pub fn read_with<B: DatumBuilder>(source: &str, id: SourceId, builder: &mut B) -> Read<B::Datum> {
    let lexed = lex(source);
    let mut sink = DatumSink::new(id, builder);
    parse(source, &lexed, &mut sink);
    let (datums, mut errors) = sink.finish();
    errors.extend(lexed.errors.iter().map(|e| ReadError {
        kind: ReadErrorKind::Lex(e.kind),
        range: e.range,
    }));
    errors.sort_by_key(|e| e.range.start());
    Read { datums, errors }
}

/// A lossless syntax tree and the syntax errors in it.
#[derive(Clone, Debug)]
pub struct ParsedTree {
    /// The `Root` node, holding every byte of the source.
    pub root: SyntaxNode,
    /// Lexical and syntax errors, in source order. Problems only the datum
    /// sink finds, such as an invalid escape, need [`read`].
    pub errors: Vec<ReadError>,
}

/// Parses `source` into a lossless syntax tree, for tools.
pub fn parse_tree(source: &str) -> ParsedTree {
    let lexed = lex(source);
    let mut sink = TreeSink::default();
    parse(source, &lexed, &mut sink);
    let (root, parse_errors) = sink.finish();
    let mut errors: Vec<ReadError> = lexed
        .errors
        .iter()
        .map(|e| ReadError {
            kind: ReadErrorKind::Lex(e.kind),
            range: e.range,
        })
        .chain(parse_errors.into_iter().map(parse_error))
        .collect();
    errors.sort_by_key(|e| e.range.start());
    ParsedTree { root, errors }
}

fn parse_error(e: ParseError) -> ReadError {
    ReadError {
        kind: ReadErrorKind::Parse(e.kind),
        range: e.range,
    }
}

/// An open node, while its contents arrive.
struct Frame<D> {
    kind: SyntaxKind,
    /// Where its first meaningful token starts, once one has arrived.
    first: Option<TextSize>,
    /// Where its last meaningful token so far ends.
    last: TextSize,
    items: Vec<D>,
    tail: Option<D>,
    after_dot: bool,
    prefix: Option<(SyntaxKind, TextRange)>,
    label: Option<u64>,
    bytes: Vec<u8>,
    failed: bool,
}

impl<D> Frame<D> {
    fn new(kind: SyntaxKind) -> Self {
        Frame {
            kind,
            first: None,
            last: TextSize::from(0),
            items: Vec::new(),
            tail: None,
            after_dot: false,
            prefix: None,
            label: None,
            bytes: Vec::new(),
            failed: false,
        }
    }

    fn range(&self) -> TextRange {
        TextRange::new(self.first.unwrap_or(self.last), self.last)
    }
}

/// An [`EventSink`] that builds datums with a [`DatumBuilder`], never
/// building a syntax tree.
///
/// On the way it decodes strings and characters, parses numbers, applies
/// `#!fold-case`, normalises identifiers to NFC, checks bytevectors and
/// datum labels, and expands abbreviations: `'x` becomes `(quote x)`.
pub struct DatumSink<'b, B: DatumBuilder> {
    source: SourceId,
    builder: &'b mut B,
    stack: Vec<Frame<B::Datum>>,
    output: Vec<B::Datum>,
    errors: Vec<ReadError>,
    fold_case: bool,
    /// Labels defined so far in the current top-level datum.
    labels: Vec<u64>,
}

impl<B: DatumBuilder> std::fmt::Debug for DatumSink<'_, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatumSink")
            .field("source", &self.source)
            .field("errors", &self.errors)
            .field("fold_case", &self.fold_case)
            .finish_non_exhaustive()
    }
}

impl<'b, B: DatumBuilder> DatumSink<'b, B> {
    /// A sink reading from `source`, building with `builder`.
    pub fn new(source: SourceId, builder: &'b mut B) -> Self {
        DatumSink {
            source,
            builder,
            stack: Vec::new(),
            output: Vec::new(),
            errors: Vec::new(),
            fold_case: false,
            labels: Vec::new(),
        }
    }

    /// The top-level datums read without error, and the errors this sink
    /// found. Lexical errors are not among them: they come from the lexer.
    pub fn finish(self) -> (Vec<B::Datum>, Vec<ReadError>) {
        (self.output, self.errors)
    }

    fn top(&mut self) -> &mut Frame<B::Datum> {
        self.stack
            .last_mut()
            .expect("the parser always opens a Root node first")
    }

    fn at_top_level(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|f| f.kind == SyntaxKind::Root)
    }

    fn span(&self, range: TextRange) -> Span {
        Span {
            source: self.source,
            range,
        }
    }

    fn fail(&mut self, kind: ReadErrorKind, range: TextRange) {
        self.errors.push(ReadError { kind, range });
        self.top().failed = true;
    }

    /// Hands a finished datum to the node it belongs to.
    fn add(&mut self, datum: B::Datum, range: TextRange) {
        let parent = self.top();
        match parent.kind {
            SyntaxKind::Root => self.output.push(datum),
            SyntaxKind::Bytevector => self.fail(ReadErrorKind::NotAByte, range),
            SyntaxKind::CommentedDatum => {}
            SyntaxKind::List if parent.after_dot => parent.tail = Some(datum),
            _ => parent.items.push(datum),
        }
    }

    fn atom(&mut self, kind: SyntaxKind, text: &str, range: TextRange) {
        if self.at_top_level() {
            self.labels.clear();
        }
        if self.top().kind == SyntaxKind::Bytevector {
            return self.byte(kind, text, range);
        }
        let span = self.span(range);
        let datum = match kind {
            SyntaxKind::Number => {
                let n = vali_num::parse(text, Radix::Decimal).expect("the lexer checked it");
                self.builder.number(n, span)
            }
            SyntaxKind::Boolean => {
                let value = text.as_bytes()[1].eq_ignore_ascii_case(&b't');
                self.builder.boolean(value, span)
            }
            SyntaxKind::Character => match decode_character(text, self.fold_case) {
                Some(c) => self.builder.character(c, span),
                None => return self.fail(ReadErrorKind::InvalidCharacter, range),
            },
            SyntaxKind::String => match decode_escapes(&text[1..text.len() - 1]) {
                Ok(s) => self.builder.string(s, span),
                Err(bad) => return self.fail(ReadErrorKind::InvalidEscape, inner(range, bad)),
            },
            SyntaxKind::Identifier => {
                let name = if let Some(barred) = text.strip_prefix('|') {
                    // Barred identifiers are never case-folded.
                    match decode_escapes(&barred[..barred.len() - 1]) {
                        Ok(name) => nfc(Cow::Owned(name)),
                        Err(bad) => {
                            return self.fail(ReadErrorKind::InvalidEscape, inner(range, bad));
                        }
                    }
                } else {
                    nfc(self.fold(text))
                };
                self.builder.symbol(&name, span)
            }
            SyntaxKind::Keyword => {
                let name = nfc(self.fold(&text[..text.len() - 1]));
                self.builder.keyword(&name, span)
            }
            SyntaxKind::LabelRef => match label_number(text) {
                Some(label) if self.labels.contains(&label) => self.builder.label_ref(label, span),
                Some(_) => return self.fail(ReadErrorKind::UndefinedLabel, range),
                None => return self.fail(ReadErrorKind::LabelTooLarge, range),
            },
            _ => return self.top().failed = true,
        };
        self.add(datum, range);
    }

    /// An element of a bytevector, which must be an exact integer from 0 to
    /// 255.
    fn byte(&mut self, kind: SyntaxKind, text: &str, range: TextRange) {
        let byte = (kind == SyntaxKind::Number)
            .then(|| vali_num::parse(text, Radix::Decimal).ok())
            .flatten()
            .and_then(|n| match n {
                Number::Integer(i) => i.to_u64().and_then(|b| u8::try_from(b).ok()),
                _ => None,
            });
        match byte {
            Some(b) => self.top().bytes.push(b),
            None => self.fail(ReadErrorKind::NotAByte, range),
        }
    }

    fn fold<'t>(&self, text: &'t str) -> Cow<'t, str> {
        if self.fold_case {
            Cow::Owned(caseless::default_case_fold_str(text))
        } else {
            Cow::Borrowed(text)
        }
    }
}

impl<B: DatumBuilder> EventSink for DatumSink<'_, B> {
    fn start_node(&mut self, kind: SyntaxKind) {
        // Labels are scoped to the top-level datum they appear in.
        if kind != SyntaxKind::CommentedDatum && self.at_top_level() {
            self.labels.clear();
        }
        self.stack.push(Frame::new(kind));
    }

    fn token(&mut self, token: Token, text: &str) {
        let kind = token.kind;
        if kind.is_trivia() {
            return;
        }
        let frame = self.top();
        frame.first.get_or_insert(token.range.start());
        frame.last = token.range.end();
        match kind {
            SyntaxKind::Directive => match text {
                "#!fold-case" => self.fold_case = true,
                "#!no-fold-case" => self.fold_case = false,
                _ => {}
            },
            // The lexer reported it.
            SyntaxKind::Error => frame.failed = true,
            SyntaxKind::LParen
            | SyntaxKind::RParen
            | SyntaxKind::LBracket
            | SyntaxKind::RBracket
            | SyntaxKind::VectorOpen
            | SyntaxKind::BytevectorOpen
            | SyntaxKind::DatumComment => {}
            SyntaxKind::Dot => frame.after_dot = true,
            SyntaxKind::LabelDef => match label_number(text) {
                Some(label) if self.labels.contains(&label) => {
                    self.fail(ReadErrorKind::DuplicateLabel, token.range)
                }
                Some(label) => {
                    self.labels.push(label);
                    self.top().label = Some(label);
                }
                None => self.fail(ReadErrorKind::LabelTooLarge, token.range),
            },
            k if k.abbreviation_symbol().is_some() => frame.prefix = Some((k, token.range)),
            _ => self.atom(kind, text, token.range),
        }
    }

    fn finish_node(&mut self) {
        let frame = self.stack.pop().expect("nodes are balanced");
        if frame.kind == SyntaxKind::Root {
            return;
        }
        let range = frame.range();
        let span = self.span(range);
        let mut failed = frame.failed;
        let datum = if failed {
            None
        } else {
            let single = |mut items: Vec<B::Datum>| {
                if items.len() == 1 { items.pop() } else { None }
            };
            match frame.kind {
                SyntaxKind::List => Some(self.builder.list(frame.items, frame.tail, span)),
                SyntaxKind::Vector => Some(self.builder.vector(frame.items, span)),
                SyntaxKind::Bytevector => Some(self.builder.bytevector(frame.bytes, span)),
                SyntaxKind::Abbreviation => match (frame.prefix, single(frame.items)) {
                    (Some((prefix, prefix_range)), Some(item)) => {
                        let name = prefix.abbreviation_symbol().expect("a prefix token");
                        let symbol = self.builder.symbol(name, self.span(prefix_range));
                        Some(self.builder.list(vec![symbol, item], None, span))
                    }
                    _ => None,
                },
                SyntaxKind::Labelled => match (frame.label, single(frame.items)) {
                    (Some(label), Some(item)) => Some(self.builder.label_def(label, item, span)),
                    _ => None,
                },
                _ => None,
            }
        };
        if datum.is_none() && frame.kind != SyntaxKind::CommentedDatum {
            failed = true;
        }

        let parent = self.top();
        if let Some(first) = frame.first {
            parent.first.get_or_insert(first);
            parent.last = parent.last.max(frame.last);
        }
        parent.failed |= failed;
        if let Some(datum) = datum {
            self.add(datum, range);
        }
    }

    fn error(&mut self, error: ParseError) {
        let error = parse_error(error);
        self.errors.push(error);
        self.top().failed = true;
    }
}

/// The range of `bad`, an offset range inside a string or barred
/// identifier's body, in the source.
fn inner(token: TextRange, bad: Range<usize>) -> TextRange {
    let body = token.start() + TextSize::from(1);
    let start = TextSize::try_from(bad.start).expect("within a token");
    let end = TextSize::try_from(bad.end).expect("within a token");
    TextRange::new(body + start, body + end)
}

fn nfc(name: Cow<'_, str>) -> String {
    if name.is_ascii() {
        name.into_owned()
    } else {
        name.nfc().collect()
    }
}

/// The number in `#12=` or `#12#`.
fn label_number(text: &str) -> Option<u64> {
    text[1..text.len() - 1].parse().ok()
}

/// The character a `#\…` token denotes: a single character, an R7RS
/// character name, or `#\x` and a hexadecimal scalar value. Names are
/// case-folded under `#!fold-case`; single characters never are.
fn decode_character(text: &str, fold_case: bool) -> Option<char> {
    let body = &text[2..];
    let mut chars = body.chars();
    let first = chars.next()?;
    if chars.as_str().is_empty() {
        return Some(first);
    }
    let name = if fold_case {
        Cow::Owned(caseless::default_case_fold_str(body))
    } else {
        Cow::Borrowed(body)
    };
    if let Some((_, c)) = CHARACTER_NAMES.iter().find(|(n, _)| *n == name) {
        return Some(*c);
    }
    let hex = name.strip_prefix('x')?;
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
}

/// Decodes the escapes in the body of a string or barred identifier: R7RS's
/// `\a \b \t \n \r \" \\ \|`, `\x` hex escapes ending in `;`, and line
/// continuations. On error, returns the escape's byte range in `body`.
fn decode_escapes(body: &str) -> Result<String, Range<usize>> {
    if !body.contains('\\') {
        return Ok(body.to_owned());
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body.char_indices().peekable();
    while let Some((i, c)) = rest.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some((j, e)) = rest.next() else {
            return Err(i..body.len());
        };
        let end = j + e.len_utf8();
        match e {
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '|' => out.push('|'),
            'x' => {
                let digits_end = body[end..].find(';').map(|n| end + n).ok_or(i..end)?;
                let digits = &body[end..digits_end];
                let c = (!digits.is_empty() && digits.chars().all(|d| d.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(digits, 16).ok())
                    .flatten()
                    .and_then(char::from_u32)
                    .ok_or(i..digits_end + 1)?;
                out.push(c);
                while rest.next_if(|&(k, _)| k <= digits_end).is_some() {}
            }
            // A line continuation: \ <intraline whitespace>* <line ending>
            // <intraline whitespace>*, which reads as nothing.
            ' ' | '\t' | '\n' | '\r' => {
                let mut saw_line_ending = false;
                let mut c = e;
                loop {
                    match c {
                        ' ' | '\t' => {}
                        '\n' if !saw_line_ending => saw_line_ending = true,
                        '\r' if !saw_line_ending => {
                            saw_line_ending = true;
                            rest.next_if(|&(_, n)| n == '\n');
                        }
                        _ => break,
                    }
                    match rest.next_if(|&(_, n)| matches!(n, ' ' | '\t' | '\n' | '\r')) {
                        Some((_, n)) if !(saw_line_ending && matches!(n, '\n' | '\r')) => c = n,
                        Some((_, n)) => {
                            // A second line ending ends the continuation.
                            out.push(n);
                            break;
                        }
                        None => break,
                    }
                }
                if !saw_line_ending {
                    return Err(i..end);
                }
            }
            _ => return Err(i..end),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_u16() {
        for (i, kind) in SyntaxKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, i);
            assert_eq!(SyntaxKind::from_u16(i as u16), Some(*kind));
        }
        assert_eq!(SyntaxKind::from_u16(SyntaxKind::ALL.len() as u16), None);
    }

    #[test]
    fn escapes() {
        assert_eq!(
            decode_escapes(r#"a\nb\t\x41;\\\""#),
            Ok("a\nb\tA\\\"".to_owned())
        );
        assert_eq!(decode_escapes("one\\  \n   two"), Ok("onetwo".to_owned()));
        assert_eq!(decode_escapes("one\\\r\n two"), Ok("onetwo".to_owned()));
        assert_eq!(decode_escapes(r"\q"), Err(0..2));
        assert_eq!(decode_escapes(r"ab\x41"), Err(2..4));
        assert_eq!(decode_escapes(r"\x110000;"), Err(0..9));
        assert_eq!(decode_escapes(r"\xD800;"), Err(0..7));
        assert_eq!(decode_escapes("\\  x"), Err(0..2));
    }
}
