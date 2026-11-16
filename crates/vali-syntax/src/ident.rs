//! R7RS's identifier grammar, with non-ASCII characters allowed by R6RS's
//! Unicode categories (ADR 0014).

use unicode_general_category::{GeneralCategory, get_general_category};

/// Whether `text` is an identifier written without vertical bars.
///
/// Follows R7RS-small, section 7.1.1: an `<initial>` followed by any number
/// of `<subsequent>`s, or a peculiar identifier such as `+`, `...` or `->x`.
/// Numbers are not identifiers, but this function doesn't check for them:
/// the lexer tries a number first.
pub fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let rest = chars.as_str();
    if is_initial(first) {
        return all_subsequent(rest);
    }
    match first {
        // <explicit sign>, <explicit sign> <sign subsequent> <subsequent>*,
        // or <explicit sign> . <dot subsequent> <subsequent>*
        '+' | '-' => {
            let mut chars = rest.chars();
            match chars.next() {
                None => true,
                Some('.') => dot_tail(chars.as_str()),
                Some(c) if is_sign_subsequent(c) => all_subsequent(chars.as_str()),
                Some(_) => false,
            }
        }
        // . <dot subsequent> <subsequent>*
        '.' => dot_tail(rest),
        _ => false,
    }
}

/// `<dot subsequent> <subsequent>*`
fn dot_tail(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if is_dot_subsequent(c) => all_subsequent(chars.as_str()),
        _ => false,
    }
}

fn all_subsequent(text: &str) -> bool {
    text.chars().all(is_subsequent)
}

/// `<initial>`: a letter, one of `! $ % & * / : < = > ? ^ _ ~`, or a
/// non-ASCII character in one of R6RS's constituent categories.
fn is_initial(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || "!$%&*/:<=>?^_~".contains(c)
    } else {
        use GeneralCategory::*;
        matches!(
            get_general_category(c),
            UppercaseLetter
                | LowercaseLetter
                | TitlecaseLetter
                | ModifierLetter
                | OtherLetter
                | NonspacingMark
                | LetterNumber
                | OtherNumber
                | DashPunctuation
                | ConnectorPunctuation
                | OtherPunctuation
                | CurrencySymbol
                | MathSymbol
                | ModifierSymbol
                | OtherSymbol
                | PrivateUse
        )
    }
}

/// `<subsequent>`: an initial, a digit, one of `+ - . @`, or a non-ASCII
/// decimal digit or combining mark.
fn is_subsequent(c: char) -> bool {
    if is_initial(c) {
        return true;
    }
    if c.is_ascii() {
        c.is_ascii_digit() || "+-.@".contains(c)
    } else {
        use GeneralCategory::*;
        matches!(
            get_general_category(c),
            DecimalNumber | SpacingMark | EnclosingMark
        )
    }
}

/// `<sign subsequent>`: an initial, an explicit sign or `@`.
fn is_sign_subsequent(c: char) -> bool {
    is_initial(c) || matches!(c, '+' | '-' | '@')
}

/// `<dot subsequent>`: a sign subsequent or `.`.
fn is_dot_subsequent(c: char) -> bool {
    is_sign_subsequent(c) || c == '.'
}
