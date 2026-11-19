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
    let base = cx.symbol_count();
    drop(cx);

    let mut cx = Cx::<S>::new(&HeapConfig {
        max_symbols: Some(base + 2),
        ..Default::default()
    });
    cx.symbol("baseline").unwrap();
    assert_eq!(cx.symbol_count(), base);
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
    assert_eq!(cx.symbol_count(), base + 2);
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
    assert_eq!(cx.symbol_count(), 2);
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
    assert_eq!(cx.symbol_count(), 2);
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
    assert!(cx.symbol_count() >= 500);
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

/// Fills the heap until it refuses, returning what it made.
fn fill<S: Strategy>(cx: &mut Cx<S>) -> Vec<vali_runtime::Handle> {
    let list = format!("({})", "1 ".repeat(200));
    let mut kept = Vec::new();
    loop {
        match cx.read(&list) {
            Ok(mut made) => kept.append(&mut made),
            Err(e) => {
                assert_eq!(e, Error::OutOfMemory);
                return kept;
            }
        }
        assert!(kept.len() < 100_000, "the limit never came");
    }
}

/// The memory limit counts the pages the heap took, which it reuses and
/// never returns: once the heap has been full and emptied, a new symbol or
/// syntax object still fits.
fn a_full_and_emptied_heap_still_interns<S: Strategy>() {
    let set = scope_set();
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_bytes: Some(vali_runtime::PAGE_BYTES * 8),
        ..Default::default()
    });
    let kept = fill(&mut cx);
    drop(kept);
    cx.collect_garbage();
    cx.symbol("after-the-heap-was-full").unwrap();
    cx.read("another:").unwrap();
    cx.syntax(syntax(set, "x")).unwrap();
}

#[test]
fn an_emptied_heap_is_not_refused_for_its_pages() {
    a_full_and_emptied_heap_still_interns::<DeferredHeadered>();
    a_full_and_emptied_heap_still_interns::<DeferredSideCounts>();
    a_full_and_emptied_heap_still_interns::<PreciseHeadered>();
    a_full_and_emptied_heap_still_interns::<PreciseSideCounts>();
}

/// What was charged for symbols' table entries keeps the heap from growing
/// past the limit with them: pages and charges together stay within it.
fn charges_limit_the_pages<S: Strategy>() {
    let limit = vali_runtime::PAGE_BYTES * 8;
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_bytes: Some(limit),
        ..Default::default()
    });
    let name = "n".repeat(1000);
    let mut made = 0;
    while cx.symbol(&format!("{name}{made}")).is_ok() {
        made += 1;
        assert!(made < 1_000_000, "the limit never came");
    }
    let _kept = fill(&mut cx);
    assert!(
        cx.bytes_used() + made * name.len() <= limit,
        "{}: {} pages and {} charged against {limit}",
        S::NAME,
        cx.bytes_used(),
        made * name.len()
    );
}

#[test]
fn the_heap_stops_growing_where_the_charges_leave_no_room() {
    under_every_strategy!(charges_limit_the_pages);
}
