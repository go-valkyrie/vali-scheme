//! The raw object API under no collection, with pairs and flonums both with
//! and without headers.

use std::ptr::NonNull;

use vali_abi::{StrategyLayout, Tag, TypeCode, data};
use vali_runtime::{
    Allocate, BumpHeap, Cx, HeapConfig, NoCollection, OutOfMemory, PAGE_BYTES, References, Space,
    Strategy, Value,
};

/// No collection, but with a header on every pair and flonum, as a counting
/// strategy that keeps its counts in headers would lay them out.
#[derive(Debug)]
struct Headered;

// SAFETY: delegates to `BumpHeap`, which upholds the contract.
unsafe impl Allocate for Headered {
    const LAYOUT: StrategyLayout = StrategyLayout::HEADERED;
    type Heap = BumpHeap;

    fn new_heap(config: &HeapConfig) -> BumpHeap {
        BumpHeap::new(config)
    }

    fn allocate(
        heap: &mut BumpHeap,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        heap.allocate(space, words)
    }

    fn bytes_used(heap: &BumpHeap) -> usize {
        heap.bytes_used()
    }
}

impl References for Headered {}

impl Strategy for Headered {
    const NAME: &'static str = "test-headered";
}

fn cx<S: Strategy>() -> Cx<S> {
    Cx::new(&HeapConfig::default())
}

fn lists_and_pairs<S: Strategy>() {
    let mut cx = cx::<S>();
    let mut list = Value::EMPTY_LIST;
    for i in 0..100_000 {
        list = unsafe { cx.cons_raw(Value::fixnum(i).unwrap(), list) }.unwrap();
        assert_eq!(list.tag(), Tag::Pair);
    }
    let mut expected = 100_000;
    let mut sum = 0;
    while list != Value::EMPTY_LIST {
        expected -= 1;
        let n = unsafe { cx.car(list) }.as_fixnum().unwrap();
        assert_eq!(n, expected);
        sum += n;
        list = unsafe { cx.cdr(list) };
    }
    assert_eq!(sum, (0..100_000).sum::<i64>());

    let pair = unsafe { cx.cons_raw(Value::TRUE, Value::FALSE) }.unwrap();
    unsafe {
        cx.set_car(pair, Value::character('x'));
        cx.set_cdr(pair, Value::EMPTY_LIST);
        assert_eq!(cx.car(pair), Value::character('x'));
        assert_eq!(cx.cdr(pair), Value::EMPTY_LIST);
        let header = cx.header(pair, TypeCode::Pair);
        assert_eq!(header.is_some(), S::LAYOUT.pair_header);
        if let Some(header) = header {
            assert_eq!(header.type_code(), Some(TypeCode::Pair));
        }
    }
}

#[test]
fn lists_without_pair_headers() {
    lists_and_pairs::<NoCollection>();
}

#[test]
fn lists_with_pair_headers() {
    lists_and_pairs::<Headered>();
}

#[test]
fn pairs_fill_pages_densely() {
    let mut cx = cx::<NoCollection>();
    let first = unsafe { cx.cons_raw(Value::TRUE, Value::TRUE) }.unwrap();
    let second = unsafe { cx.cons_raw(Value::TRUE, Value::TRUE) }.unwrap();
    assert_eq!(second.address() - first.address(), 16, "two words per pair");
    assert_eq!(first.address() % PAGE_BYTES as u64, 0, "a fresh page");
    let used = cx.bytes_used();
    for _ in 0..(PAGE_BYTES / 16) {
        unsafe { cx.cons_raw(Value::TRUE, Value::TRUE) }.unwrap();
    }
    assert_eq!(cx.bytes_used(), used + PAGE_BYTES);
}

fn flonums_and_vectors<S: Strategy>() {
    let mut cx = cx::<S>();
    let x = cx.flonum_raw(-0.0).unwrap();
    assert_eq!(x.tag(), Tag::Flonum);
    assert!(unsafe { cx.flonum_value(x) }.to_bits() == (-0.0f64).to_bits());
    let nan = cx.flonum_raw(f64::NAN).unwrap();
    assert!(unsafe { cx.flonum_value(nan) }.is_nan());

    for len in [0, 1, 7, 10_000] {
        let v = unsafe { cx.make_vector_raw(len, Value::UNSPECIFIED) }.unwrap();
        assert_eq!(v.tag(), Tag::Other);
        unsafe {
            assert_eq!(cx.vector_length(v), len);
            let header = cx.header(v, TypeCode::Vector).unwrap();
            assert_eq!(header.type_code(), Some(TypeCode::Vector));
            for i in 0..len {
                assert_eq!(cx.vector_ref(v, i), Value::UNSPECIFIED);
                cx.vector_set(v, i, Value::fixnum(i as i64).unwrap());
            }
            for i in 0..len {
                assert_eq!(cx.vector_ref(v, i), Value::fixnum(i as i64).unwrap());
            }
        }
    }
}

#[test]
fn flonums_and_vectors_compact() {
    flonums_and_vectors::<NoCollection>();
}

#[test]
fn flonums_and_vectors_headered() {
    flonums_and_vectors::<Headered>();
}

#[test]
fn strings_are_utf8() {
    let mut cx = cx::<NoCollection>();
    for (text, ascii) in [("", true), ("hello", true), ("héllo, λ 🦀", false)] {
        let s = cx.string_raw(text, false).unwrap();
        assert_eq!(s.tag(), Tag::String);
        unsafe {
            assert_eq!(cx.string_str(s), text);
            assert_eq!(cx.string_length(s), text.chars().count());
            let header = cx.header(s, TypeCode::String).unwrap();
            assert_eq!(header.data() & data::ASCII != 0, ascii);
            assert_eq!(header.data() & data::IMMUTABLE, 0);
        }
    }
    let literal = cx.string_raw("x", true).unwrap();
    let header = unsafe { cx.header(literal, TypeCode::String) }.unwrap();
    assert_ne!(header.data() & data::IMMUTABLE, 0);

    let bytes = cx.bytevector_raw(&[1, 2, 3, 255]).unwrap();
    assert_eq!(unsafe { cx.bytevector_bytes(bytes) }, &[1, 2, 3, 255]);
}

#[test]
fn the_heap_limit_is_enforced() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig {
        max_bytes: Some(2 * PAGE_BYTES),
    });
    let mut pairs = 0;
    while unsafe { cx.cons_raw(Value::TRUE, Value::TRUE) }.is_ok() {
        pairs += 1;
    }
    assert_eq!(pairs, 2 * PAGE_BYTES / 16);
    // Large objects count against the same limit.
    assert_eq!(
        unsafe { cx.make_vector_raw(10_000, Value::TRUE) }.unwrap_err(),
        OutOfMemory
    );
}

#[test]
fn values() {
    assert_eq!(Value::fixnum(-5).unwrap().as_fixnum(), Some(-5));
    assert_eq!(Value::fixnum(i64::MAX), None);
    assert_eq!(Value::boolean(true).as_boolean(), Some(true));
    assert!(!Value::FALSE.is_true());
    assert!(Value::EMPTY_LIST.is_true());
    assert_eq!(Value::character('λ').as_character(), Some('λ'));
    assert_eq!(Value::TRUE.as_fixnum(), None);
    assert_eq!(format!("{:?}", Value::fixnum(42).unwrap()), "42");
    assert_eq!(format!("{:?}", Value::EMPTY_LIST), "()");
    assert_eq!(format!("{:?}", Value::character('a')), "#\\a");
    assert_eq!(format!("{:?}", Value::UNBOUND), "#<marker 0>");
}

#[test]
fn a_context_is_send() {
    fn send<T: Send>() {}
    send::<Cx<NoCollection>>();
}
