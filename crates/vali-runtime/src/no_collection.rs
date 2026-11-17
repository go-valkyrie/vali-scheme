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

/// A heap under no collection: its pages, and a bump buffer per space.
#[derive(Debug)]
pub struct BumpHeap {
    pages: PageSource,
    pair: Bump,
    flonum: Bump,
    general: Bump,
}

impl BumpHeap {
    /// An empty heap with the limit from `config`.
    pub fn new(config: &HeapConfig) -> BumpHeap {
        BumpHeap {
            pages: PageSource::new(config),
            pair: Bump::default(),
            flonum: Bump::default(),
            general: Bump::default(),
        }
    }

    /// Memory for `words` words in `space`, never reclaimed.
    #[inline]
    pub fn allocate(&mut self, space: Space, words: usize) -> Result<NonNull<u64>, OutOfMemory> {
        let bytes = words.checked_mul(8).ok_or(OutOfMemory)?;
        let buffer = match space {
            Space::Pair => &mut self.pair,
            Space::Flonum => &mut self.flonum,
            Space::General if bytes > LARGE_OBJECT_BYTES => {
                return self.pages.large(bytes).map(NonNull::cast);
            }
            Space::General => &mut self.general,
        };
        buffer.allocate(&mut self.pages, bytes)
    }

    /// How many bytes the heap has taken from the system.
    pub fn bytes_used(&self) -> usize {
        self.pages.bytes_used()
    }
}

// SAFETY: allocation comes from whole pages the heap owns, or from large
// blocks, both 8-byte aligned and never reused while the heap lives.
unsafe impl Allocate for NoCollection {
    const LAYOUT: StrategyLayout = StrategyLayout::COMPACT;
    type Heap = BumpHeap;

    fn new_heap(config: &HeapConfig) -> BumpHeap {
        BumpHeap::new(config)
    }

    #[inline]
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

impl References for NoCollection {}

impl Strategy for NoCollection {
    const NAME: &'static str = "none";
}
