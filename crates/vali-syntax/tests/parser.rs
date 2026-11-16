//! The parser and its two sinks: the lossless tree and the datum reader.

use vali_syntax::{
    DatumKind, ParseErrorKind, ReadErrorKind, SourceId, SyntaxElement, SyntaxKind, SyntaxNode,
    TextRange, parse_tree, read,
};

const SOURCE: SourceId = SourceId(1);

/// The tree as compact text: nodes as `Kind(…)`, tokens as `Kind"text"`,
/// whitespace left out.
fn tree(source: &str) -> String {
    fn walk(node: &SyntaxNode, out: &mut String) {
        out.push_str(&format!("{:?}(", node.kind()));
        let mut first = true;
        for child in node.children_with_tokens() {
            if let SyntaxElement::Token(t) = &child
                && t.kind() == SyntaxKind::Whitespace
            {
                continue;
            }
            if !first {
                out.push(' ');
            }
            first = false;
            match child {
                SyntaxElement::Node(n) => walk(&n, out),
                SyntaxElement::Token(t) => out.push_str(&format!("{:?}{:?}", t.kind(), t.text())),
            }
        }
        out.push(')');
    }
    let mut out = String::new();
    walk(&parse_tree(source).root, &mut out);
    out
}

/// Reads `source`, expecting no errors, and prints each datum.
fn reads(source: &str) -> Vec<String> {
    let read = read(source, SOURCE);
    assert!(
        read.errors.is_empty(),
        "reading {source:?}: {:?}",
        read.errors
    );
    read.datums.iter().map(ToString::to_string).collect()
}

/// The datums read from `source`, and its errors with the text they cover.
fn read_with_errors(source: &str) -> (Vec<String>, Vec<(ReadErrorKind, &str)>) {
    let read = read(source, SOURCE);
    (
        read.datums.iter().map(ToString::to_string).collect(),
        read.errors
            .iter()
            .map(|e| (e.kind, &source[e.range]))
            .collect(),
    )
}

fn parse(kind: ParseErrorKind) -> ReadErrorKind {
    ReadErrorKind::Parse(kind)
}

#[test]
fn tree_shape() {
    assert_eq!(
        tree("(a [b] . c)"),
        r#"Root(List(LParen"(" Identifier"a" List(LBracket"[" Identifier"b" RBracket"]") Dot"." Identifier"c" RParen")"))"#
    );
    assert_eq!(
        tree("'#(1) #u8(2) #0=x"),
        r##"Root(Abbreviation(Quote"'" Vector(VectorOpen"#(" Number"1" RParen")")) Bytevector(BytevectorOpen"#u8(" Number"2" RParen")") Labelled(LabelDef"#0=" Identifier"x"))"##
    );
}

#[test]
fn trivia_sits_beside_the_next_datum_not_inside_it() {
    assert_eq!(
        tree("; note\n(a) #;(b) c"),
        r##"Root(LineComment"; note" List(LParen"(" Identifier"a" RParen")") CommentedDatum(DatumComment"#;" List(LParen"(" Identifier"b" RParen")")) Identifier"c")"##
    );
}

#[test]
fn the_tree_keeps_every_byte() {
    for source in [
        "",
        "  (define (f x) ; comment\n  (* x x))\n",
        "#| block |# #;(gone) '(a . b) #0=(x . #0#) \"str\\n\"",
        ")) (( ] [ . #u8( 'oops",
        "#!fold-case ABC #!/not-a-shebang",
    ] {
        assert_eq!(parse_tree(source).root.text().to_string(), source);
    }
}

#[test]
fn reading_atoms_and_structure() {
    assert_eq!(
        reads("42 #t #false \"hi\" #\\a sym key: (a b . c) [x y] #(1 2) #u8(0 255)"),
        [
            "42",
            "#t",
            "#f",
            "\"hi\"",
            "#\\a",
            "sym",
            "key:",
            "(a b . c)",
            "(x y)",
            "#(1 2)",
            "#u8(0 255)",
        ]
    );
}

#[test]
fn abbreviations_expand() {
    assert_eq!(
        reads("'a `(b ,c ,@d) #'e #`(f #,g #,@h)"),
        [
            "(quote a)",
            "(quasiquote (b (unquote c) (unquote-splicing d)))",
            "(syntax e)",
            "(quasisyntax (f (unsyntax g) (unsyntax-splicing h)))",
        ]
    );
    // The `quote` symbol's span is the `'` itself.
    let read = read("'x", SOURCE);
    let DatumKind::List { items, .. } = &read.datums[0].kind else {
        panic!("expected a list")
    };
    assert_eq!(items[0].span.range, TextRange::new(0.into(), 1.into()));
}

#[test]
fn datum_comments() {
    assert_eq!(reads("#;(x) y"), ["y"]);
    assert_eq!(reads("#; #; a b c"), ["c"]);
    assert_eq!(reads("(a #;b c)"), ["(a c)"]);
}

#[test]
fn strings_and_characters_are_decoded() {
    assert_eq!(reads(r#""a\x41;b\n""#), [r#""aAb\n""#]);
    assert_eq!(
        reads("#\\space #\\x41 #\\alarm #\\("),
        ["#\\space", "#\\A", "#\\alarm", "#\\("]
    );
    let (datums, errors) = read_with_errors(r#"#\bell "ok" "a\qb""#);
    assert_eq!(datums, ["\"ok\""]);
    assert_eq!(
        errors,
        [
            (ReadErrorKind::InvalidCharacter, "#\\bell"),
            (ReadErrorKind::InvalidEscape, "\\q"),
        ]
    );
}

#[test]
fn fold_case_applies_to_names_but_not_single_characters_or_bars() {
    assert_eq!(
        reads("ABC #!fold-case ABC |XyZ| #\\SPACE #\\A Foo: Straße #!no-fold-case ABC"),
        [
            "ABC", "abc", "XyZ", "#\\space", "#\\A", "foo:", "strasse", "ABC"
        ]
    );
}

#[test]
fn identifiers_are_normalised_to_nfc() {
    let combining = read("e\u{301}", SOURCE);
    let precomposed = read("\u{e9}", SOURCE);
    let name = |r: &vali_syntax::Read<vali_syntax::Datum>| match &r.datums[0].kind {
        DatumKind::Symbol(s) => s.clone(),
        other => panic!("expected a symbol, got {other:?}"),
    };
    assert_eq!(name(&combining), name(&precomposed));
}

#[test]
fn symbols_print_with_bars_when_needed() {
    assert_eq!(
        reads("|1+| |foo:| |a b| |.| plain"),
        ["|1+|", "|foo:|", "|a b|", "|.|", "plain"]
    );
}

#[test]
fn labels() {
    assert_eq!(reads("#0=(a . #0#)"), ["#0=(a . #0#)"]);
    assert_eq!(reads("(#1=x #1#)"), ["(#1=x #1#)"]);
    let (_, errors) = read_with_errors("#1#");
    assert_eq!(errors, [(ReadErrorKind::UndefinedLabel, "#1#")]);
    let (_, errors) = read_with_errors("(#0=a #0=b)");
    assert_eq!(errors, [(ReadErrorKind::DuplicateLabel, "#0=")]);
    // Labels don't carry over from one top-level datum to the next.
    let (datums, errors) = read_with_errors("#0=a #0#");
    assert_eq!(datums, ["#0=a"]);
    assert_eq!(errors, [(ReadErrorKind::UndefinedLabel, "#0#")]);
}

#[test]
fn bytevector_elements_must_be_bytes() {
    for bad in ["256", "-1", "1.0", "x", "(1)"] {
        let source = format!("#u8({bad})");
        let (datums, errors) = read_with_errors(&source);
        assert!(datums.is_empty(), "{source}");
        assert_eq!(errors, [(ReadErrorKind::NotAByte, bad)], "{source}");
    }
}

#[test]
fn datums_span_their_tokens_without_surrounding_trivia() {
    let read = read("  (a b)  ", SOURCE);
    assert_eq!(
        read.datums[0].span.range,
        TextRange::new(2.into(), 7.into())
    );
    assert_eq!(read.datums[0].span.source, SOURCE);
}

#[test]
fn recovery_skips_only_the_broken_form() {
    /// A source, the datums read from it, and its errors with their text.
    type Case<'a> = (&'a str, &'a [&'a str], &'a [(ReadErrorKind, &'a str)]);
    let cases: &[Case] = &[
        (
            ") a",
            &["a"],
            &[(parse(ParseErrorKind::UnexpectedCloser), ")")],
        ),
        (
            "(a ] b",
            &[],
            &[
                (parse(ParseErrorKind::Unclosed), "("),
                (parse(ParseErrorKind::MismatchedCloser), "]"),
            ],
        ),
        (
            "[(a] b",
            &["b"],
            &[(parse(ParseErrorKind::MismatchedCloser), "]")],
        ),
        ("(a", &[], &[(parse(ParseErrorKind::Unclosed), "(")]),
        (
            "( . a) z",
            &["z"],
            &[(parse(ParseErrorKind::DotWithoutHead), ".")],
        ),
        (
            "(a . ) z",
            &["z"],
            &[(parse(ParseErrorKind::DotWithoutTail), ".")],
        ),
        (
            "(a . b c) z",
            &["z"],
            &[(parse(ParseErrorKind::ExtraDatumAfterDot), "c")],
        ),
        (
            "#(a . b) z",
            &["z"],
            &[(parse(ParseErrorKind::DotOutsideList), ".")],
        ),
        (
            ". z",
            &["z"],
            &[(parse(ParseErrorKind::DotOutsideList), ".")],
        ),
        (
            "(a ') z",
            &["z"],
            &[(parse(ParseErrorKind::MissingDatum), "'")],
        ),
        (
            "#0= )",
            &[],
            &[
                (parse(ParseErrorKind::MissingDatum), "#0="),
                (parse(ParseErrorKind::UnexpectedCloser), ")"),
            ],
        ),
        ("#;", &[], &[(parse(ParseErrorKind::MissingDatum), "#;")]),
        (
            "(a 1+ b) c",
            &["c"],
            &[(
                ReadErrorKind::Lex(vali_syntax::LexErrorKind::InvalidAtom),
                "1+",
            )],
        ),
    ];
    for (source, datums, errors) in cases {
        let (got_datums, got_errors) = read_with_errors(source);
        assert_eq!(&got_datums, datums, "datums for {source:?}");
        assert_eq!(&got_errors, errors, "errors for {source:?}");
    }
}

#[test]
fn printing_reads_back_the_same() {
    let program = r#"
        (define-library (demo greet)
          (export greet)
          (import (scheme base))
          (begin
            (define (greet name: who)
              `(hello ,who #\! "with \"quotes\"\n" #u8(1 2) #(a b) . 3/4))))
    "#;
    let once = reads(program);
    let twice = reads(&once.join(" "));
    assert_eq!(once, twice);
}

/// Generated inputs never panic either sink, and the tree is always
/// lossless.
#[test]
fn both_sinks_survive_generated_inputs() {
    const PIECES: &[&str] = &[
        "(",
        ")",
        "[",
        "]",
        "#(",
        "#u8(",
        ".",
        "'",
        ",@",
        "#'",
        "#;",
        "#0=",
        "#0#",
        "a",
        "1",
        "\"s\"",
        "\"\\q\"",
        "#\\x",
        "#!fold-case",
        ";c\n",
        " ",
        "#|x|#",
        "1+",
        "256",
        "B:",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    for _ in 0..5_000 {
        let pieces = next(20);
        let source: String = (0..pieces)
            .map(|_| PIECES[next(PIECES.len())])
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(parse_tree(&source).root.text().to_string(), source);
        let _ = read(&source, SOURCE);
    }
}
