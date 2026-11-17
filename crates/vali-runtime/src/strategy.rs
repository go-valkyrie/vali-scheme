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
    /// The state shared by every task of a heap.
    type Heap: Send + Sync + Debug;
    /// The state of one task: its allocation buffers, and pending work.
    type Mutator: Send + Debug;

    /// A heap's shared state.
    fn new_heap(config: &HeapConfig) -> Self::Heap;

    /// A task's state.
    fn new_mutator(heap: &Self::Heap) -> Self::Mutator;

    /// Uninitialised memory for an object of `words` words in `space`.
    fn allocate(
        heap: &Self::Heap,
        mutator: &mut Self::Mutator,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory>;
}

/// References the runtime holds on objects outside the heap, such as a
/// task's root slots for Rust handles (ADR 0003). A counting strategy counts
/// them; the defaults do nothing.
pub trait References: Allocate {
    /// The runtime now holds one more reference to `v`, a heap object.
    #[inline(always)]
    fn retain(_mutator: &mut Self::Mutator, _v: crate::Value) {}

    /// The runtime has dropped one reference to `v`, a heap object.
    #[inline(always)]
    fn release(_mutator: &mut Self::Mutator, _v: crate::Value) {}
}

/// A memory strategy: every capability trait together.
pub trait Strategy: Allocate + References {
    /// The strategy's name, as `vali build -m` spells it.
    const NAME: &'static str;
}
