//! The Logos token definitions, before atoms are classified.
//!
//! Delimiters, as R7RS defines them plus brackets, are whitespace and
//! `( ) [ ] " ; |`. Everything else runs together into an atom, which
//! [`super::classify_atom`] turns into a number, an identifier or an error.

use logos::{Lexer, Logos};

use crate::LexErrorKind;

#[derive(Logos, Clone, Copy, Debug, PartialEq, Eq)]
#[logos(error = LexErrorKind)]
pub(super) enum Raw {
    #[regex(r"\s+")]
    Whitespace,
    #[regex(r";[^\n]*", allow_greedy = true)]
    LineComment,
    #[token("#|", block_comment)]
    BlockComment,
    #[regex(r#"#![^\s()\[\]";|]*"#, allow_greedy = true)]
    Directive,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("#(")]
    VectorOpen,
    #[regex(r"#[uU]8\(")]
    BytevectorOpen,

    #[token("'")]
    Quote,
    #[token("`")]
    Quasiquote,
    #[token(",")]
    Unquote,
    #[token(",@")]
    UnquoteSplicing,
    #[token("#'")]
    Syntax,
    #[token("#`")]
    Quasisyntax,
    #[token("#,")]
    Unsyntax,
    #[token("#,@")]
    UnsyntaxSplicing,

    #[token("#;")]
    DatumComment,
    #[regex(r"#[0-9]+=", priority = 10)]
    LabelDef,
    #[regex(r"#[0-9]+#", priority = 10)]
    LabelRef,

    /// A string, closed or not; [`string`] reports a missing close.
    #[regex(r#""([^"\\]|\\(.|\n))*"?"#, string)]
    String,
    /// An identifier in vertical bars, closed or not.
    #[regex(r"\|([^|\\]|\\(.|\n))*\|?", piped_identifier)]
    PipedIdentifier,
    /// `#\` and the character or name after it.
    #[regex(r#"#\\(.|\n)[^\s()\[\]";|]*"#, allow_greedy = true)]
    Character,

    /// An atom starting with `#`: a boolean or a number with a prefix. It
    /// never starts with a digit, so `#0=x` is a label and then `x`.
    #[regex(r#"#[^\s()\[\]";|'`,\\!#0-9][^\s()\[\]";|]*"#, allow_greedy = true)]
    HashAtom,
    /// `#` and digits that are neither a label definition nor a reference.
    #[regex(r"#[0-9]+")]
    HashDigits,
    /// Any other atom: a number, an identifier or `.`.
    #[regex(r#"[^\s()\[\]";|'`,#][^\s()\[\]";|]*"#, allow_greedy = true)]
    Atom,
}

/// Consumes a block comment after its opening `#|`, counting nested ones.
fn block_comment(lex: &mut Lexer<'_, Raw>) -> Result<(), LexErrorKind> {
    let rest = lex.remainder().as_bytes();
    let mut depth = 1usize;
    let mut i = 0;
    while i < rest.len() {
        match (rest[i], rest.get(i + 1)) {
            (b'#', Some(b'|')) => {
                depth += 1;
                i += 2;
            }
            (b'|', Some(b'#')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    lex.bump(i);
                    return Ok(());
                }
            }
            _ => i += 1,
        }
    }
    lex.bump(rest.len());
    Err(LexErrorKind::UnterminatedBlockComment)
}

fn string(lex: &mut Lexer<'_, Raw>) -> Result<(), LexErrorKind> {
    closed(lex.slice(), '"', LexErrorKind::UnterminatedString)
}

fn piped_identifier(lex: &mut Lexer<'_, Raw>) -> Result<(), LexErrorKind> {
    closed(lex.slice(), '|', LexErrorKind::UnterminatedIdentifier)
}

/// Whether a quoted token ends with its closing quote, rather than at the end
/// of the input.
fn closed(slice: &str, quote: char, error: LexErrorKind) -> Result<(), LexErrorKind> {
    let body = &slice[1..];
    // The token's pattern consumes escapes in pairs, so a closing quote can
    // only be its last character, and only if that character is not itself
    // escaped. An even run of backslashes before it escapes nothing.
    if body.ends_with(quote) {
        let before = &body[..body.len() - 1];
        let backslashes = before.chars().rev().take_while(|&c| c == '\\').count();
        if backslashes % 2 == 0 {
            return Ok(());
        }
    }
    Err(error)
}
