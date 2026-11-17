//! The no-collection strategy: allocate, never reclaim. The baseline that M1
//! measures the counting strategies against (ADR 0003), like the JVM's
//! Epsilon collector. Memory is returned only when the heap is dropped.

use std::ptr::NonNull;

use vali_abi::StrategyLayout;

use crate::page::{Bump, HeapConfig, LARGE_OBJECT_BYTES, OutOfMemory, PageSource};
use crate::strategy::{Allocate, References, Space, Strategy};

/// Allocate, never reclaim.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCollection;

/// A task's allocation buffers, one per space.
#[derive(Debug, Default)]
pub struct Buffers {
    pair: Bump,
    flonum: Bump,
    general: Bump,
}

// SAFETY: allocation comes from whole pages the source owns, or from large
// blocks, both 8-byte aligned and never reused while the heap lives.
unsafe impl Allocate for NoCollection {
    const LAYOUT: StrategyLayout = StrategyLayout::COMPACT;
    type Heap = PageSource;
    type Mutator = Buffers;

    fn new_heap(config: &HeapConfig) -> PageSource {
        PageSource::new(config)
    }

    fn new_mutator(_heap: &PageSource) -> Buffers {
        Buffers::default()
    }

    #[inline]
    fn allocate(
        heap: &PageSource,
        mutator: &mut Buffers,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        let bytes = words.checked_mul(8).ok_or(OutOfMemory)?;
        let buffer = match space {
            Space::Pair => &mut mutator.pair,
            Space::Flonum => &mut mutator.flonum,
            Space::General if bytes > LARGE_OBJECT_BYTES => {
                return heap.large(bytes).map(NonNull::cast);
            }
            Space::General => &mut mutator.general,
        };
        buffer.allocate(heap, bytes)
    }
}

impl References for NoCollection {}

impl Strategy for NoCollection {
    const NAME: &'static str = "none";
}
