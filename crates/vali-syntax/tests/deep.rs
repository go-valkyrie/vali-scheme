//! Datums and syntax objects as deep or as long as memory allows: cloning,
//! printing, converting between them, remapping and dropping must not
//! recurse on the nesting, which would overflow a test thread's 2 MiB stack
//! (ADR 0014 #2).

use vali_syntax::{
    Datum, DatumKind, Remap, ScopeContext, ScopeKind, SourceId, Span, Syntax, TextRange,
};

/// Far beyond what a recursive pass could take on a 2 MiB stack.
const DEEP: usize = 500_000;

fn span() -> Span {
    Span {
        source: SourceId(0),
        range: TextRange::default(),
    }
}

fn datum(kind: DatumKind) -> Datum {
    Datum { kind, span: span() }
}

/// `n` levels of the given nesting around `a`, as the reader builds them:
/// `(((a)))`, `#(#(a))`, `(x . (x . a))`, or `#0=(#0=a)`.
fn nest(shape: &str, n: usize) -> Datum {
    let mut inner = datum(DatumKind::Symbol("a".into()));
    for _ in 0..n {
        inner = datum(match shape {
            "list" => DatumKind::List {
                items: vec![inner],
                tail: None,
            },
            "vector" => DatumKind::Vector(vec![inner]),
            "dotted" => DatumKind::List {
                items: vec![datum(DatumKind::Boolean(true))],
                tail: Some(Box::new(inner)),
            },
            "labelled" => DatumKind::Labelled(0, Box::new(inner)),
            _ => unreachable!("a shape"),
        });
    }
    inner
}

const SHAPES: [&str; 4] = ["list", "vector", "dotted", "labelled"];

/// How many characters `shape` nested `n` deep prints as.
fn printed_length(shape: &str, n: usize) -> usize {
    1 + match shape {
        "list" => 2 * n,
        "vector" => 3 * n,
        "dotted" => 7 * n,
        "labelled" => 3 * n,
        _ => unreachable!("a shape"),
    }
}

/// How many characters `shape` prints as once it has been syntax. Syntax
/// continues a list through a dotted tail that is a list, so `(x . (x . a))`
/// is `(x x . a)`.
fn syntax_length(shape: &str) -> usize {
    match shape {
        "dotted" => 3 * DEEP + 5,
        shape => printed_length(shape, DEEP),
    }
}

#[test]
fn deep_datums_clone_print_and_drop() {
    for shape in SHAPES {
        let deep = nest(shape, DEEP);
        let copy = deep.clone();
        let text = copy.to_string();
        assert_eq!(text.len(), printed_length(shape, DEEP), "{shape}");
        assert_eq!(text, deep.to_string(), "{shape}");
        drop(copy);
        drop(deep);
    }
}

#[test]
fn long_datums_clone_print_and_drop() {
    let long = datum(DatumKind::List {
        items: (0..DEEP)
            .map(|_| datum(DatumKind::Boolean(false)))
            .collect(),
        tail: Some(Box::new(datum(DatumKind::Symbol("end".into())))),
    });
    let copy = long.clone();
    assert_eq!(copy.to_string().len(), DEEP * 3 + 7);
}

#[test]
fn cloning_keeps_the_datum() {
    let read = vali_syntax::read("(a #(1 \"two\" #u8(3)) (4 . 5) #0=(6 #0#) 'q)", SourceId(0));
    assert!(read.errors.is_empty());
    for datum in &read.datums {
        assert_eq!(datum.clone().to_string(), datum.to_string());
    }
}

#[test]
fn deep_syntax_converts_remaps_and_drops() {
    let mut from = ScopeContext::new();
    let library = from.new_scope(ScopeKind::Library).unwrap();
    let step = from.new_scope(ScopeKind::Macro).unwrap();
    let top = from.intern(&[library]).unwrap();
    for shape in ["list", "vector", "dotted"] {
        let deep = nest(shape, DEEP);
        let syntax = Syntax::from_datum(&deep, top);
        // A pending change below every node, which remapping maps too.
        let flipped = syntax.flip(&mut from, step).unwrap();
        assert_eq!(
            flipped.to_datum().to_string().len(),
            syntax_length(shape),
            "{shape}"
        );
        let mut to = ScopeContext::new();
        let mut remap = Remap::new();
        let copy = flipped.remap(&mut remap, &from, &mut to).unwrap();
        assert_eq!(
            copy.to_datum().to_string().len(),
            syntax_length(shape),
            "{shape}"
        );
        drop(copy);
        drop(flipped);
        drop(syntax);
        drop(deep);
    }
}

#[test]
fn syntax_shared_between_nodes_drops_once() {
    // A subform several nodes share survives until the last drops, and a
    // deep one dropped through its sharers does not overflow either.
    let mut context = ScopeContext::new();
    let scope = context.new_scope(ScopeKind::Library).unwrap();
    let top = context.intern(&[scope]).unwrap();
    let deep = Syntax::from_datum(&nest("list", DEEP), top);
    let wrapper = Syntax::list(vec![deep.clone(), deep.clone()], None, top, span(), None);
    drop(deep);
    drop(wrapper);
}

#[test]
fn deep_datums_and_syntax_format_with_debug() {
    let mut context = ScopeContext::new();
    let scope = context.new_scope(ScopeKind::Library).unwrap();
    let top = context.intern(&[scope]).unwrap();
    // Small values print as a derive would.
    let small = nest("list", 2);
    let text = format!("{small:?}");
    assert!(text.starts_with("Datum { kind: List { items: [Datum { kind: List"));
    assert!(!text.contains("..."), "{text}");
    // Deep ones stop at a bounded depth, plain or pretty.
    for shape in SHAPES {
        let deep = nest(shape, DEEP);
        for text in [format!("{deep:?}"), format!("{deep:#?}")] {
            assert!(text.contains("..."), "{shape}");
            assert!(text.len() < 10_000_000, "{shape}: {}", text.len());
        }
        let syntax = Syntax::from_datum(&deep, top);
        let text = format!("{syntax:?}");
        assert!(text.starts_with("Syntax { content: "), "{shape}");
        assert!(text.contains("..."), "{shape}");
        assert!(text.len() < 10_000_000, "{shape}: {}", text.len());
    }
}

/// `(x . (x . (... . a)))`, each dotted tail a list of its own, as
/// [`Syntax::from_parts`] keeps them: the nodes a template's output
/// leaves, which the constructors would have spliced.
fn dotted_chain(n: usize, scopes: vali_syntax::ScopeSetId) -> Syntax {
    let mut chain = Syntax::identifier("a", scopes, span(), None);
    for _ in 0..n {
        chain = Syntax::from_parts(
            vali_syntax::NewNode::List {
                items: vec![Syntax::identifier("x", scopes, span(), None)],
                tail: Some(chain),
            },
            scopes,
            Vec::new(),
            span(),
            None,
        )
        .unwrap();
    }
    chain
}

#[test]
fn exposing_a_deep_chain_of_dotted_tails_is_one_list() {
    let mut context = ScopeContext::new();
    let library = context.new_scope(ScopeKind::Library).unwrap();
    let step = context.new_scope(ScopeKind::Macro).unwrap();
    let top = context.intern(&[library]).unwrap();
    let chain = dotted_chain(DEEP, top);
    // With and without a pending change that every part below receives.
    for syntax in [chain.clone(), chain.flip(&mut context, step).unwrap()] {
        let vali_syntax::Exposed::List { items, tail } = syntax.expose(&mut context).unwrap()
        else {
            panic!("a list");
        };
        assert_eq!(items.len(), DEEP);
        let tail = tail.expect("a dotted tail");
        assert!(matches!(
            tail.expose(&mut context).unwrap(),
            vali_syntax::Exposed::Identifier(_)
        ));
    }
}
