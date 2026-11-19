//! Resource limits (ADR 0023): the caps on symbols, keywords and runtime
//! syntax objects, seen by the host as a typed error.

use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, Error, HeapConfig, Limit, NoCollection,
    PreciseHeadered, PreciseSideCounts, Strategy,
};
use vali_syntax::{ScopeContext, ScopeKind, SourceId, Span, Syntax};

macro_rules! under_every_strategy {
    ($f:ident) => {{
        $f::<NoCollection>();
        $f::<DeferredHeadered>();
        $f::<DeferredSideCounts>();
        $f::<PreciseHeadered>();
        $f::<PreciseSideCounts>();
    }};
}

fn syntax(set: vali_syntax::ScopeSetId, name: &str) -> Syntax {
    Syntax::identifier(
        name,
        set,
        Span {
            source: SourceId(0),
            range: Default::default(),
        },
        None,
    )
}

fn scope_set() -> vali_syntax::ScopeSetId {
    let mut scopes = ScopeContext::new();
    let scope = scopes.new_scope(ScopeKind::Other).unwrap();
    scopes.intern(&[scope]).unwrap()
}

fn symbols_are_capped<S: Strategy>() {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.symbol("baseline").unwrap();
    let base = cx.interned_symbols();
    drop(cx);

    let mut cx = Cx::<S>::new(&HeapConfig {
        max_symbols: Some(base + 2),
        ..Default::default()
    });
    cx.symbol("baseline").unwrap();
    assert_eq!(cx.interned_symbols(), base);
    cx.symbol("one").unwrap();
    // A hit costs nothing, at the cap or under it.
    cx.symbol("one").unwrap();
    cx.symbol("baseline").unwrap();
    cx.symbol("two").unwrap();
    cx.symbol("two").unwrap();
    let before = cx.bytes_used();
    assert_eq!(
        cx.symbol("three").unwrap_err(),
        Error::LimitExceeded(Limit::Symbols),
        "{}",
        S::NAME
    );
    // Nothing was allocated, and the isolate still works.
    assert_eq!(cx.bytes_used(), before);
    assert_eq!(cx.interned_symbols(), base + 2);
    assert_eq!(
        cx.symbol("three").unwrap_err(),
        Error::LimitExceeded(Limit::Symbols)
    );
    let two = cx.symbol("two").unwrap();
    assert_eq!(cx.write_string(&two).unwrap(), "two");
}

#[test]
fn the_symbol_cap_raises_a_typed_error() {
    under_every_strategy!(symbols_are_capped);
}

fn keywords_count_with_symbols<S: Strategy>() {
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_symbols: Some(2),
        ..Default::default()
    });
    cx.read("a b:").unwrap();
    assert_eq!(cx.interned_symbols(), 2);
    // A hit on either table is free.
    cx.read("a b:").unwrap();
    // A new symbol, or a new keyword, is over the cap, and the reader
    // reports it as the typed error.
    assert_eq!(
        cx.read("c").unwrap_err(),
        Error::LimitExceeded(Limit::Symbols),
        "{}",
        S::NAME
    );
    assert_eq!(
        cx.read("d:").unwrap_err(),
        Error::LimitExceeded(Limit::Symbols),
        "{}",
        S::NAME
    );
    assert_eq!(cx.interned_symbols(), 2);
}

#[test]
fn keywords_and_symbols_share_a_cap() {
    under_every_strategy!(keywords_count_with_symbols);
}

fn syntaxes_are_capped<S: Strategy>() {
    let set = scope_set();
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_syntaxes: Some(3),
        ..Default::default()
    });
    let mut kept = Vec::new();
    for name in ["a", "b", "c"] {
        kept.push(cx.syntax(syntax(set, name)).unwrap());
    }
    assert_eq!(
        cx.syntax(syntax(set, "d")).unwrap_err(),
        Error::LimitExceeded(Limit::Syntaxes),
        "{}",
        S::NAME
    );
    assert_eq!(cx.write_string(&kept[0]).unwrap(), "#<syntax a>");
}

#[test]
fn the_syntax_cap_raises_a_typed_error() {
    under_every_strategy!(syntaxes_are_capped);
}

fn limits_unset_change_nothing<S: Strategy>() {
    let set = scope_set();
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    for i in 0..500 {
        cx.symbol(&format!("symbol-{i}")).unwrap();
        cx.syntax(syntax(set, "x")).unwrap();
    }
    assert!(cx.interned_symbols() >= 500);
}

#[test]
fn without_limits_nothing_is_capped() {
    under_every_strategy!(limits_unset_change_nothing);
}

fn rust_bytes_count_against_the_memory_limit<S: Strategy>() {
    // Symbols interned until the memory limit stops them: with the heap's
    // pages and the tables' entries both counted, it is not an error of
    // another kind.
    let limit = vali_runtime::PAGE_BYTES * 4;
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_bytes: Some(limit),
        ..Default::default()
    });
    let name = "n".repeat(1000);
    let mut made = 0;
    let error = loop {
        match cx.symbol(&format!("{name}{made}")) {
            Ok(_) => made += 1,
            Err(e) => break e,
        }
        assert!(made < 1_000_000, "the limit never came");
    };
    assert_eq!(error, Error::OutOfMemory, "{}", S::NAME);
    assert!(made > 10);
}

#[test]
fn the_memory_limit_stops_symbols() {
    under_every_strategy!(rust_bytes_count_against_the_memory_limit);
}
