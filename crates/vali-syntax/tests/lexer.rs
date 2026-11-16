//! The lexer: token kinds, errors, Unicode identifiers, and losslessness.

use vali_syntax::{LexErrorKind, SyntaxKind, lex};

use SyntaxKind::*;

/// The non-whitespace tokens of `source`, as (kind, text) pairs.
fn tokens(source: &str) -> Vec<(SyntaxKind, &str)> {
    lex(source)
        .tokens
        .iter()
        .filter(|t| t.kind != Whitespace)
        .map(|t| (t.kind, &source[t.range]))
        .collect()
}

/// Checks that `source` is exactly one token of `kind`.
fn single(source: &str, kind: SyntaxKind) {
    assert_eq!(tokens(source), [(kind, source)], "lexing {source:?}");
}

/// Checks that `source` is exactly one error token, for `error`.
fn error(source: &str, error: LexErrorKind) {
    let lexed = lex(source);
    assert_eq!(
        lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
        [Error],
        "lexing {source:?}"
    );
    assert_eq!(lexed.errors.len(), 1, "errors for {source:?}");
    assert_eq!(lexed.errors[0].kind, error, "error for {source:?}");
}

#[test]
fn delimiters_and_abbreviations() {
    assert_eq!(
        tokens("( ) [ ] #( #u8( #U8( ' ` , ,@ #' #` #, #,@ #;"),
        [
            (LParen, "("),
            (RParen, ")"),
            (LBracket, "["),
            (RBracket, "]"),
            (VectorOpen, "#("),
            (BytevectorOpen, "#u8("),
            (BytevectorOpen, "#U8("),
            (Quote, "'"),
            (Quasiquote, "`"),
            (Unquote, ","),
            (UnquoteSplicing, ",@"),
            (Syntax, "#'"),
            (Quasisyntax, "#`"),
            (Unsyntax, "#,"),
            (UnsyntaxSplicing, "#,@"),
            (DatumComment, "#;"),
        ]
    );
}

#[test]
fn abbreviations_attach_without_spaces() {
    assert_eq!(
        tokens("'(a ,b ,@c)"),
        [
            (Quote, "'"),
            (LParen, "("),
            (Identifier, "a"),
            (Unquote, ","),
            (Identifier, "b"),
            (UnquoteSplicing, ",@"),
            (Identifier, "c"),
            (RParen, ")"),
        ]
    );
}

#[test]
fn identifiers() {
    for id in [
        "foo",
        "list->vector",
        "set-car!",
        "<=?",
        "a1",
        "a.b",
        "+",
        "-",
        "...",
        "->x",
        ".foo",
        "+.x",
        "|hello world|",
        "|1+|",
        "||",
        ":",
    ] {
        single(id, Identifier);
    }
}

#[test]
fn unicode_identifiers_follow_r6rs_categories() {
    // Letters, mathematical symbols, subscripts and combining marks.
    for id in ["λ", "café", "→", "≤", "x₁", "∀x", "数", "e\u{301}"] {
        single(id, Identifier);
    }
    // Invisible format characters, including the bidirectional controls of
    // "Trojan Source" attacks, are not identifier characters.
    error("a\u{200B}b", LexErrorKind::InvalidAtom);
    error("a\u{202E}b", LexErrorKind::InvalidAtom);
    // A non-ASCII digit may continue an identifier but not start one.
    single("x٣", Identifier);
    error("٣x", LexErrorKind::InvalidAtom);
    // Non-ASCII whitespace separates tokens.
    assert_eq!(tokens("a\u{A0}b"), [(Identifier, "a"), (Identifier, "b")]);
}

#[test]
fn keywords_follow_srfi_88() {
    single("foo:", Keyword);
    single("with-x:", Keyword);
    // A lone colon, and a colon in bars, are symbols.
    single(":", Identifier);
    single("|foo:|", Identifier);
}

#[test]
fn numbers() {
    for n in [
        "42", "-1.5e3", "+i", "-inf.0", "+nan.0", "1/2", "#x1F", "#e1.5", "#i3", "1+2i", ".5",
    ] {
        single(n, Number);
    }
}

#[test]
fn booleans_characters_and_strings() {
    for b in ["#t", "#f", "#true", "#false", "#T"] {
        single(b, Boolean);
    }
    for c in ["#\\a", "#\\space", "#\\x41", "#\\(", "#\\)", "#\\;", "#\\λ"] {
        single(c, Character);
    }
    assert_eq!(tokens("#\\ "), [(Character, "#\\ ")]);
    for s in [
        "\"hi\"",
        "\"a\\\"b\"",
        "\"multi\nline\"",
        "\"\"",
        "\"back\\\\\"",
    ] {
        single(s, String);
    }
}

#[test]
fn labels_end_at_their_equals_sign() {
    assert_eq!(
        tokens("#0=x #12=y #3#"),
        [
            (LabelDef, "#0="),
            (Identifier, "x"),
            (LabelDef, "#12="),
            (Identifier, "y"),
            (LabelRef, "#3#"),
        ]
    );
    error("#12", LexErrorKind::UnknownHashSyntax);
}

#[test]
fn labels_and_dots() {
    assert_eq!(
        tokens("#0=(a . #0#)"),
        [
            (LabelDef, "#0="),
            (LParen, "("),
            (Identifier, "a"),
            (Dot, "."),
            (LabelRef, "#0#"),
            (RParen, ")"),
        ]
    );
}

#[test]
fn comments_are_trivia() {
    let source = "; line\n#| outer #| nested |# still |# x #;(y)";
    assert_eq!(
        tokens(source),
        [
            (LineComment, "; line"),
            (BlockComment, "#| outer #| nested |# still |#"),
            (Identifier, "x"),
            (DatumComment, "#;"),
            (LParen, "("),
            (Identifier, "y"),
            (RParen, ")"),
        ]
    );
    assert!(LineComment.is_trivia() && BlockComment.is_trivia() && Shebang.is_trivia());
    assert!(!DatumComment.is_trivia());
}

#[test]
fn directives() {
    for d in ["#!fold-case", "#!no-fold-case", "#!r6rs"] {
        single(d, Directive);
    }
    error("#!foo", LexErrorKind::UnknownDirective);
}

#[test]
fn shebang_only_on_the_first_line() {
    assert_eq!(
        tokens("#!/usr/bin/env vali\n(x)"),
        [
            (Shebang, "#!/usr/bin/env vali"),
            (LParen, "("),
            (Identifier, "x"),
            (RParen, ")"),
        ]
    );
    assert_eq!(tokens("#! vali")[0].0, Shebang);
    // A directive on the first line is not a shebang.
    single("#!fold-case", Directive);
    // Later lines never are.
    assert_eq!(tokens("x\n#!/bin/sh")[1].0, Error);
}

#[test]
fn invalid_atoms_are_strict() {
    for atom in ["1+", "12px", "1abc", "1.0.0", "a'b", "@x"] {
        error(atom, LexErrorKind::InvalidAtom);
    }
    let message = LexErrorKind::InvalidAtom.to_string();
    assert!(message.contains("|1+|"), "{message}");
}

#[test]
fn unterminated_tokens() {
    error("\"abc", LexErrorKind::UnterminatedString);
    error("\"abc\\\"", LexErrorKind::UnterminatedString);
    error("|abc", LexErrorKind::UnterminatedIdentifier);
    error("#| x #| y |#", LexErrorKind::UnterminatedBlockComment);
}

#[test]
fn unknown_hash_syntax() {
    error("#q", LexErrorKind::UnknownHashSyntax);
    error("#xZZ", LexErrorKind::UnknownHashSyntax);
    error("#", LexErrorKind::UnexpectedCharacter);
}

#[test]
fn errors_do_not_stop_lexing() {
    assert_eq!(
        tokens("(a 1+ b)"),
        [
            (LParen, "("),
            (Identifier, "a"),
            (Error, "1+"),
            (Identifier, "b"),
            (RParen, ")"),
        ]
    );
}

/// Lexing loses nothing, ranges tile the source, and each error token has
/// exactly one error. Checked over a fixed set of generated inputs built
/// from Scheme's trickiest characters.
#[test]
fn lossless_over_generated_inputs() {
    const PIECES: &[&str] = &[
        "(",
        ")",
        "[",
        "]",
        "#",
        "|",
        ";",
        "\"",
        "\\",
        "'",
        "`",
        ",",
        "@",
        ".",
        "+",
        "-",
        "1",
        "a",
        "λ",
        "\n",
        " ",
        "u8",
        "x",
        "!",
        "e",
        "i",
        "/",
        ":",
        "#|",
        "|#",
        "#;",
        "#\\",
        "#!",
        "=",
        "\u{200B}",
        "é",
        "\t",
        "0",
        "fold-case",
    ];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    for _ in 0..5_000 {
        let pieces = next(24);
        let source: std::string::String = (0..pieces).map(|_| PIECES[next(PIECES.len())]).collect();
        let lexed = lex(&source);

        let mut end = 0usize;
        for token in &lexed.tokens {
            assert_eq!(usize::from(token.range.start()), end, "gap in {source:?}");
            assert!(!token.range.is_empty(), "empty token in {source:?}");
            end = token.range.end().into();
        }
        assert_eq!(end, source.len(), "lexing {source:?} lost its end");

        let error_ranges: Vec<_> = lexed
            .tokens
            .iter()
            .filter(|t| t.kind == Error)
            .map(|t| t.range)
            .collect();
        let reported: Vec<_> = lexed.errors.iter().map(|e| e.range).collect();
        assert_eq!(error_ranges, reported, "errors for {source:?}");
    }
}
