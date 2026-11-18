//! Handles (ADR 0003): Send and 'static, used through their isolate's
//! context, dropped from anywhere.

use std::ptr::NonNull;

use vali_abi::{StrategyLayout, Tag};
use vali_runtime::{
    Allocate, BumpHeap, Cx, Error, Handle, HeapConfig, NoCollection, OutOfMemory, References,
    Space, Strategy, Value,
};

fn cx() -> Cx<NoCollection> {
    Cx::new(&HeapConfig::default())
}

fn list(cx: &mut Cx<NoCollection>, items: &[i64]) -> Handle {
    let mut list = Handle::empty_list();
    for &n in items.iter().rev() {
        list = cx.cons(&Handle::fixnum(n).unwrap(), &list).unwrap();
    }
    list
}

fn to_vec(cx: &mut Cx<NoCollection>, mut list: Handle) -> Vec<i64> {
    let mut out = Vec::new();
    while list.is_pair() {
        out.push(list.car(cx).unwrap().as_fixnum().unwrap());
        list = list.cdr(cx).unwrap();
    }
    out
}

#[test]
fn lists_through_handles() {
    let mut cx = cx();
    let xs = list(&mut cx, &[1, 2, 3]);
    assert_eq!(xs.tag(), Tag::Pair);
    assert_eq!(to_vec(&mut cx, xs.clone()), [1, 2, 3]);
    xs.set_car(&mut cx, &Handle::fixnum(10).unwrap()).unwrap();
    let rest = xs.cdr(&mut cx).unwrap();
    rest.set_cdr(&mut cx, &Handle::empty_list()).unwrap();
    assert_eq!(to_vec(&mut cx, xs), [10, 2]);
}

#[test]
fn immediates_need_no_context_and_no_slot() {
    let mut cx = cx();
    let n = Handle::fixnum(42).unwrap();
    assert_eq!(n.as_fixnum(), Some(42));
    assert_eq!(Handle::boolean(false).as_boolean(), Some(false));
    assert_eq!(Handle::character('λ').as_character(), Some('λ'));
    assert_eq!(cx.get(&n), Ok(Value::fixnum(42).unwrap()));
    assert_eq!(cx.live_handles(), 0);
    // Any isolate can use an immediate.
    let mut other = self::cx();
    assert!(other.cons(&n, &Handle::empty_list()).is_ok());
}

#[test]
fn other_values() {
    let mut cx = cx();
    let x = cx.flonum(2.5).unwrap();
    assert_eq!(x.to_f64(&mut cx), Ok(2.5));
    let s = cx.string("héllo").unwrap();
    assert_eq!(s.string_value(&mut cx).unwrap(), "héllo");
    let v = cx.vector(3, &Handle::boolean(true)).unwrap();
    assert_eq!(v.vector_length(&mut cx), Ok(3));
    v.vector_set(&mut cx, 1, &s).unwrap();
    let element = v.vector_ref(&mut cx, 1).unwrap();
    assert_eq!(element.string_value(&mut cx).unwrap(), "héllo");
    assert_eq!(v.vector_ref(&mut cx, 3).unwrap_err(), Error::OutOfRange);
}

#[test]
fn type_errors() {
    let mut cx = cx();
    let n = Handle::fixnum(1).unwrap();
    let s = cx.string("x").unwrap();
    assert_eq!(
        n.car(&mut cx).unwrap_err(),
        Error::WrongType { expected: "a pair" }
    );
    assert_eq!(
        s.vector_length(&mut cx).unwrap_err(),
        Error::WrongType {
            expected: "a vector"
        }
    );
    assert_eq!(
        n.to_f64(&mut cx).unwrap_err(),
        Error::WrongType {
            expected: "a flonum"
        }
    );
}

#[test]
fn another_isolate_cannot_use_a_handle() {
    let mut owner = cx();
    let mut other = cx();
    let xs = owner
        .cons(&Handle::boolean(true), &Handle::empty_list())
        .unwrap();
    assert_eq!(xs.car(&mut other).unwrap_err(), Error::WrongIsolate);
    assert_eq!(
        other.cons(&xs, &Handle::empty_list()).unwrap_err(),
        Error::WrongIsolate
    );
    assert!(Error::WrongIsolate.to_string().contains("channel"));
    assert!(xs.car(&mut owner).is_ok());
}

#[test]
fn dropped_handles_are_released_at_the_next_safepoint() {
    let mut cx = cx();
    let s = cx.string("kept").unwrap();
    let copies: Vec<Handle> = (0..10).map(|_| s.clone()).collect();
    assert_eq!(cx.live_handles(), 1, "clones share one slot");
    drop(copies);
    assert_eq!(cx.live_handles(), 1);
    drop(s);
    assert_eq!(cx.live_handles(), 0);
    // Freed slots are reused.
    let again: Vec<Handle> = (0..300)
        .map(|i| cx.string(&i.to_string()).unwrap())
        .collect();
    assert_eq!(cx.live_handles(), 300);
    drop(again);
    assert_eq!(cx.live_handles(), 0);
}

#[test]
fn handles_are_dropped_from_any_thread() {
    let mut cx = cx();
    let handles: Vec<Handle> = (0..1000)
        .map(|i| cx.string(&i.to_string()).unwrap())
        .collect();
    let keep = handles[7].clone();
    std::thread::scope(|scope| {
        for chunk in handles.chunks(125) {
            let clones: Vec<Handle> = chunk.iter().flat_map(|h| [h.clone(), h.clone()]).collect();
            scope.spawn(move || drop(clones));
        }
    });
    drop(handles);
    assert_eq!(cx.live_handles(), 1);
    assert_eq!(keep.string_value(&mut cx).unwrap(), "7");
}

#[test]
fn handles_and_contexts_cross_await_points() {
    fn send<T: Send + 'static>(_: T) {}
    fn sync<T: Sync>() {}
    sync::<Handle>();
    let mut cx = cx();
    let xs = cx
        .cons(&Handle::fixnum(1).unwrap(), &Handle::empty_list())
        .unwrap();
    // A future holding a context and its handles across an `.await` is
    // `Send`, so a host can `tokio::spawn` it.
    send(async move {
        std::future::ready(()).await;
        let car = xs.car(&mut cx).unwrap();
        std::future::ready(()).await;
        car.as_fixnum()
    });
}

/// Counts the references the runtime holds, to check that every retain has
/// a matching release.
#[derive(Debug)]
struct Probe;

#[derive(Debug)]
struct ProbeHeap {
    bump: BumpHeap,
    held: i64,
}

// SAFETY: delegates allocation to `BumpHeap`.
unsafe impl Allocate for Probe {
    const LAYOUT: StrategyLayout = StrategyLayout::COMPACT;
    type Heap = ProbeHeap;

    fn new_heap(config: &HeapConfig) -> ProbeHeap {
        ProbeHeap {
            bump: BumpHeap::new(config),
            held: 0,
        }
    }

    fn allocate(
        heap: &mut ProbeHeap,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        heap.bump.allocate(space, words)
    }

    fn bytes_used(heap: &ProbeHeap) -> usize {
        heap.bump.bytes_used()
    }
}

impl References for Probe {
    fn retain(heap: &mut ProbeHeap, v: Value) {
        assert!(v.is_pointer());
        heap.held += 1;
    }

    fn release(heap: &mut ProbeHeap, v: Value) {
        assert!(v.is_pointer());
        heap.held -= 1;
    }
}

impl Strategy for Probe {
    const NAME: &'static str = "test-probe";
}

#[test]
fn every_retain_is_released() {
    let mut cx = Cx::<Probe>::new(&HeapConfig::default());
    {
        let a = cx.string("a").unwrap();
        let b = cx.cons(&a, &Handle::empty_list()).unwrap();
        let _a_again = b.car(&mut cx).unwrap();
        let _n = Handle::fixnum(3).unwrap();
        assert_eq!(cx.live_handles(), 3, "a, b and a again each have a slot");
        assert_eq!(cx.heap().held, 3);
    }
    cx.safepoint();
    assert_eq!(cx.heap().held, 0);
    assert_eq!(cx.live_handles(), 0);
}
