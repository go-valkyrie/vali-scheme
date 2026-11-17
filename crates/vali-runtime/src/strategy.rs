//! The memory-strategy contract (ADR 0003), split into capability traits
//! that grow as strategies need them. Each trait's hooks have empty defaults
//! where that is meaningful, so a strategy implements only what it uses, and
//! an empty hook compiles to nothing.

use std::fmt::Debug;
use std::ptr::NonNull;

use vali_abi::StrategyLayout;

use crate::page::{HeapConfig, OutOfMemory};

/// Where an object is allocated. Pairs and flonums have spaces of their own,
/// so a collector can find them without a header (ADR 0002 #8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Space {
    /// Pairs.
    Pair,
    /// Flonums.
    Flonum,
    /// Everything else. Objects over `LARGE_OBJECT_BYTES` are allocated on
    /// their own.
    General,
}

/// Allocation, which every strategy provides.
///
/// # Safety
///
/// `allocate` must return memory that is 8-byte aligned, at least `words`
/// words long, not otherwise in use, and valid until the heap is dropped or
/// the strategy reclaims the object.
pub unsafe trait Allocate: Sized + Send + Sync + 'static {
    /// Whether pairs and flonums carry a header under this strategy.
    const LAYOUT: StrategyLayout;
    /// The state of one isolate's heap: its pages, allocation buffers and
    /// pending work. One thread at a time uses it (ADR 0008).
    type Heap: Send + Debug;

    /// A fresh heap.
    fn new_heap(config: &HeapConfig) -> Self::Heap;

    /// Uninitialised memory for an object of `words` words in `space`.
    fn allocate(
        heap: &mut Self::Heap,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory>;

    /// How many bytes the heap has taken from the system.
    fn bytes_used(heap: &Self::Heap) -> usize;
}

/// References the runtime holds on objects outside the heap, such as an
/// isolate's root slots for Rust handles (ADR 0003). A counting strategy counts
/// them; the defaults do nothing.
pub trait References: Allocate {
    /// The runtime now holds one more reference to `v`, a heap object.
    #[inline(always)]
    fn retain(_heap: &mut Self::Heap, _v: crate::Value) {}

    /// The runtime has dropped one reference to `v`, a heap object.
    #[inline(always)]
    fn release(_heap: &mut Self::Heap, _v: crate::Value) {}

    /// `v` was just allocated, its fields not yet written.
    #[inline(always)]
    fn allocated(_heap: &mut Self::Heap, _v: crate::Value) {}

    /// `new`, any value, was stored into a field of an object just
    /// allocated.
    #[inline(always)]
    fn init(_heap: &mut Self::Heap, _new: crate::Value) {}

    /// `new` was stored over `old` in a field of an object or a variable's
    /// cell: the store hook (ADR 0003). Either may be any value.
    #[inline(always)]
    fn write(_heap: &mut Self::Heap, _old: crate::Value, _new: crate::Value) {}
}

/// A memory strategy: every capability trait together.
pub trait Strategy: Allocate + References {
    /// The strategy's name, as `vali build -m` spells it.
    const NAME: &'static str;
}
