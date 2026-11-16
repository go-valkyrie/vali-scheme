//! The lexer (ADR 0014): splits source text into tokens, losing nothing.

mod raw;

use logos::Logos;
use text_size::{TextRange, TextSize};
use vali_num::Radix;

use crate::SyntaxKind;
use crate::ident::is_identifier;
use raw::Raw;

/// One token: its kind and where it is in the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    /// What kind of token it is.
    pub kind: SyntaxKind,
    /// Its byte range in the source.
    pub range: TextRange,
}

/// Why some text is not a valid token.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum LexErrorKind {
    /// A character that cannot start any token, such as a lone `#`.
    #[default]
    UnexpectedCharacter,
    /// A `#|` comment that is never closed.
    UnterminatedBlockComment,
    /// A string that is never closed.
    UnterminatedString,
    /// An identifier in vertical bars that is never closed.
    UnterminatedIdentifier,
    /// A `#!` directive other than `#!fold-case`, `#!no-fold-case` and
    /// `#!r6rs`.
    UnknownDirective,
    /// A `#` form that is neither a boolean nor a number, such as `#q`.
    UnknownHashSyntax,
    /// An atom that is neither a number nor an identifier, such as `1+`.
    InvalidAtom,
}

/// An invalid token: why, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexError {
    /// What is wrong.
    pub kind: LexErrorKind,
    /// The byte range of the [`SyntaxKind::Error`] token it concerns.
    pub range: TextRange,
}

impl std::fmt::Display for LexErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LexErrorKind::UnexpectedCharacter => "unexpected character",
            LexErrorKind::UnterminatedBlockComment => "block comment is never closed with |#",
            LexErrorKind::UnterminatedString => "string is never closed",
            LexErrorKind::UnterminatedIdentifier => "identifier is never closed with |",
            LexErrorKind::UnknownDirective => {
                "unknown directive; Vali recognises #!fold-case, #!no-fold-case and #!r6rs"
            }
            LexErrorKind::UnknownHashSyntax => "unknown # syntax",
            LexErrorKind::InvalidAtom => {
                "neither a number nor an identifier; write it between vertical bars, as in |1+|, for a symbol"
            }
        })
    }
}

impl std::error::Error for LexError {}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {:?}", self.kind, self.range)
    }
}

/// The result of lexing a source text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lexed {
    /// Every token, in order. Their texts, concatenated, are the source.
    pub tokens: Vec<Token>,
    /// One entry for each [`SyntaxKind::Error`] token, in order.
    pub errors: Vec<LexError>,
}

/// Splits `source` into tokens.
///
/// Never fails: text that is not a valid token becomes a
/// [`SyntaxKind::Error`] token, with a [`LexError`] saying why, and lexing
/// carries on. Nothing is lost: concatenating the tokens' texts gives back
/// `source`.
///
/// # Panics
///
/// If `source` is 4 GiB or longer, since ranges are 32-bit.
pub fn lex(source: &str) -> Lexed {
    let len = TextSize::try_from(source.len()).expect("sources are limited to 4 GiB");
    let mut out = Lexed::default();
    let mut push = |kind, start: usize, end: usize, error: Option<LexErrorKind>| {
        let range = TextRange::new(offset(start), offset(end));
        out.tokens.push(Token { kind, range });
        if let Some(kind) = error {
            out.errors.push(LexError { kind, range });
        }
    };

    let body_start = shebang_len(source);
    if body_start > 0 {
        push(SyntaxKind::Shebang, 0, body_start, None);
    }

    let mut lexer = Raw::lexer(&source[body_start..]);
    while let Some(result) = lexer.next() {
        let span = lexer.span();
        let (start, end) = (body_start + span.start, body_start + span.end);
        let text = &source[start..end];
        let classified = result.and_then(|raw| classify(raw, text));
        match classified {
            Ok(kind) => push(kind, start, end, None),
            Err(error) => push(SyntaxKind::Error, start, end, Some(error)),
        }
    }
    debug_assert_eq!(
        out.tokens
            .last()
            .map_or(TextSize::from(0), |t| t.range.end()),
        len
    );
    out
}

fn offset(n: usize) -> TextSize {
    TextSize::try_from(n).expect("sources are limited to 4 GiB")
}

/// The length of a shebang line at the very start of `source`, or 0.
fn shebang_len(source: &str) -> usize {
    if source.starts_with("#!/") || source.starts_with("#! ") {
        source.find('\n').unwrap_or(source.len())
    } else {
        0
    }
}

fn classify(raw: Raw, text: &str) -> Result<SyntaxKind, LexErrorKind> {
    Ok(match raw {
        Raw::Whitespace => SyntaxKind::Whitespace,
        Raw::LineComment => SyntaxKind::LineComment,
        Raw::BlockComment => SyntaxKind::BlockComment,
        Raw::Directive => match text {
            "#!fold-case" | "#!no-fold-case" | "#!r6rs" => SyntaxKind::Directive,
            _ => return Err(LexErrorKind::UnknownDirective),
        },
        Raw::LParen => SyntaxKind::LParen,
        Raw::RParen => SyntaxKind::RParen,
        Raw::LBracket => SyntaxKind::LBracket,
        Raw::RBracket => SyntaxKind::RBracket,
        Raw::VectorOpen => SyntaxKind::VectorOpen,
        Raw::BytevectorOpen => SyntaxKind::BytevectorOpen,
        Raw::Quote => SyntaxKind::Quote,
        Raw::Quasiquote => SyntaxKind::Quasiquote,
        Raw::Unquote => SyntaxKind::Unquote,
        Raw::UnquoteSplicing => SyntaxKind::UnquoteSplicing,
        Raw::Syntax => SyntaxKind::Syntax,
        Raw::Quasisyntax => SyntaxKind::Quasisyntax,
        Raw::Unsyntax => SyntaxKind::Unsyntax,
        Raw::UnsyntaxSplicing => SyntaxKind::UnsyntaxSplicing,
        Raw::DatumComment => SyntaxKind::DatumComment,
        Raw::LabelDef => SyntaxKind::LabelDef,
        Raw::LabelRef => SyntaxKind::LabelRef,
        Raw::String => SyntaxKind::String,
        Raw::PipedIdentifier => SyntaxKind::Identifier,
        Raw::Character => SyntaxKind::Character,
        Raw::HashAtom => classify_hash_atom(text)?,
        Raw::Atom => classify_atom(text)?,
    })
}

/// An atom starting with `#`: a boolean, or a number with a radix or
/// exactness prefix.
fn classify_hash_atom(text: &str) -> Result<SyntaxKind, LexErrorKind> {
    if ["#t", "#f", "#true", "#false"]
        .iter()
        .any(|b| text.eq_ignore_ascii_case(b))
    {
        Ok(SyntaxKind::Boolean)
    } else if vali_num::parse(text, Radix::Decimal).is_ok() {
        Ok(SyntaxKind::Number)
    } else {
        Err(LexErrorKind::UnknownHashSyntax)
    }
}

/// Any other atom: a lone `.`, a number, an identifier or keyword, or an
/// error. A number is tried first, so `+i` and `-inf.0` are numbers.
fn classify_atom(text: &str) -> Result<SyntaxKind, LexErrorKind> {
    if text == "." {
        Ok(SyntaxKind::Dot)
    } else if vali_num::parse(text, Radix::Decimal).is_ok() {
        Ok(SyntaxKind::Number)
    } else if is_identifier(text) {
        // SRFI 88: an identifier ending in a colon is a keyword, but a lone
        // colon is a symbol.
        if text.len() > 1 && text.ends_with(':') {
            Ok(SyntaxKind::Keyword)
        } else {
            Ok(SyntaxKind::Identifier)
        }
    } else {
        Err(LexErrorKind::InvalidAtom)
    }
}
