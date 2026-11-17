//! Rust objects (ADR 0016): Rust values Scheme holds, dropped when the
//! strategy frees them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vali_runtime::{
    Cx, DeferredHeadered, Error, Handle, HeapConfig, NoCollection, PreciseSideCounts, RustObject,
    Strategy,
};

/// Counts its drops.
struct Probe {
    drops: Arc<AtomicUsize>,
    hits: u32,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl RustObject for Probe {
    const NAME: &'static str = "probe";
}

struct Other;

impl RustObject for Other {
    const NAME: &'static str = "other";

    fn write(&self, out: &mut String) {
        out.push_str("#<other thing>");
    }
}

fn probe<S: Strategy>(cx: &mut Cx<S>) -> (Handle, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    let h = cx
        .object(Probe {
            drops: Arc::clone(&drops),
            hits: 0,
        })
        .unwrap();
    (h, drops)
}

/// A Rust object's value is borrowed through its handle, and dropped once
/// nothing refers to it: by counting, or by cycle collection when it hangs
/// off a garbage cycle.
fn dropped_when_freed<S: Strategy>() {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    let (h, drops) = probe(&mut cx);
    assert_eq!(cx.write_string(&h).unwrap(), "#<probe>");
    assert!(cx.is_object::<Probe>(&h));
    assert!(!cx.is_object::<Other>(&h));
    cx.with_object(&h, |p: &mut Probe| p.hits += 1).unwrap();
    assert_eq!(cx.with_object(&h, |p: &mut Probe| p.hits).unwrap(), 1);
    assert!(matches!(
        cx.with_object(&h, |_: &mut Other| ()),
        Err(Error::WrongType { expected: "other" })
    ));
    let other = cx.object(Other).unwrap();
    assert_eq!(cx.write_string(&other).unwrap(), "#<other thing>");

    drop(h);
    cx.collect_garbage();
    assert_eq!(drops.load(Ordering::Relaxed), 1, "{}", S::NAME);

    // In a pair whose cdr is itself.
    let (h, drops) = probe(&mut cx);
    let ring = cx.cons(&h, &Handle::empty_list()).unwrap();
    ring.set_cdr(&mut cx, &ring).unwrap();
    drop(h);
    cx.collect_garbage();
    assert_eq!(
        drops.load(Ordering::Relaxed),
        0,
        "{}: the ring is held",
        S::NAME
    );
    drop(ring);
    cx.collect_garbage();
    assert_eq!(
        drops.load(Ordering::Relaxed),
        1,
        "{}: the ring is garbage",
        S::NAME
    );
    assert_eq!(cx.live_objects(), 1, "{}: only `other` is left", S::NAME);
}

#[test]
fn counting_strategies_drop_what_they_free() {
    dropped_when_freed::<DeferredHeadered>();
    dropped_when_freed::<PreciseSideCounts>();
}

#[test]
fn the_isolate_drops_what_is_left() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let (h, drops) = probe(&mut cx);
    drop(h);
    cx.collect_garbage();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(cx);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
