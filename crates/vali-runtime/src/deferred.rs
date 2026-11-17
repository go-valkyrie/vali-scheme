//! Deferred reference counting (Deutsch and Bobrow; ADR 0003): only
//! references from the heap, variables' cells and the runtime's own tables
//! are counted, never those from frames, so calls and returns cost nothing.
//! An object whose count reaches zero waits in the zero-count table until
//! the next epoch, which counts the stack's references in, frees what is
//! still at zero, and counts them out again.
//!
//! An object's count lives in the strategy's half of its header word (ADR
//! 0002 #7). With side counts, a pair has no header: its count is a byte in
//! its page's side table, sticky at 255, and its flags another byte.

use std::ptr::NonNull;

use vali_abi::StrategyLayout;

use crate::page::{HeapConfig, OutOfMemory, PageSource};
use crate::page_heap::PageHeap;
use crate::strategy::{Allocate, References, Space, Strategy};
use crate::value::Value;

/// Deferred counting. With `SIDE_COUNTS`, pairs carry no header and keep
/// their counts in their pages' side tables.
#[derive(Clone, Copy, Debug, Default)]
pub struct Deferred<const SIDE_COUNTS: bool>;

/// Deferred counting with every object headered.
pub type DeferredHeadered = Deferred<false>;

/// Deferred counting with pairs' counts in side tables.
pub type DeferredSideCounts = Deferred<true>;

/// How much an isolate allocates between epochs.
const EPOCH_BYTES: usize = 1 << 20;

/// A heap under deferred counting.
#[derive(Debug)]
pub struct DeferredHeap {
    pages: PageHeap,
    /// The zero-count table: objects whose count reached zero, to free at
    /// the next epoch unless a frame refers to them.
    zct: Vec<Value>,
    /// Bytes allocated since the last epoch.
    since: usize,
}

impl DeferredHeap {
    /// How many bytes live objects take, garbage still awaiting an epoch
    /// included.
    pub fn bytes_live(&self) -> usize {
        self.pages.bytes_live()
    }
}

/// In the header's strategy half: the count, and the zero-count table's
/// flag.
const COUNT: u32 = (1 << 28) - 1;
const IN_ZCT: u32 = 1 << 31;

/// In a side table: a pair's count, sticky at this, and its flags.
const STICKY: u8 = u8::MAX;
const SIDE_IN_ZCT: u8 = 1;

impl<const SIDE_COUNTS: bool> Deferred<SIDE_COUNTS> {
    fn side(v: Value) -> bool {
        SIDE_COUNTS && v.is_pair()
    }

    /// The strategy's half of `v`'s header word.
    ///
    /// # Safety
    ///
    /// `v` must be a live object with a header.
    unsafe fn half(v: Value) -> *mut u32 {
        let word = v.address() as *mut u32;
        // SAFETY: the header is the object's first word.
        unsafe { word.add(usize::from(cfg!(target_endian = "little"))) }
    }

    /// `v`'s side-table bytes: its count, then its flags.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair on a page with a two-byte side table.
    unsafe fn side_bytes(v: Value) -> (*mut u8, *mut u8) {
        // SAFETY: passed on to the caller; each pair has two bytes.
        unsafe {
            let first = PageHeap::side_byte(v.address() as *const u64);
            (first, first.add(1))
        }
    }

    fn count_of(v: Value) -> usize {
        // SAFETY: counted values are live objects.
        unsafe {
            if Self::side(v) {
                usize::from(*Self::side_bytes(v).0)
            } else {
                (*Self::half(v) & COUNT) as usize
            }
        }
    }

    fn in_zct(v: Value) -> bool {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                *Self::side_bytes(v).1 & SIDE_IN_ZCT != 0
            } else {
                *Self::half(v) & IN_ZCT != 0
            }
        }
    }

    fn set_in_zct(v: Value, on: bool) {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let flags = Self::side_bytes(v).1;
                *flags = if on {
                    *flags | SIDE_IN_ZCT
                } else {
                    *flags & !SIDE_IN_ZCT
                };
            } else {
                let half = Self::half(v);
                *half = if on { *half | IN_ZCT } else { *half & !IN_ZCT };
            }
        }
    }

    fn increment(v: Value) {
        if !v.is_pointer() {
            return;
        }
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let count = Self::side_bytes(v).0;
                if *count < STICKY {
                    *count += 1;
                }
            } else {
                *Self::half(v) += 1;
            }
        }
    }

    /// Counts `v` down, returning whether its count is now zero. A sticky
    /// side count stays.
    fn decrement(v: Value) -> bool {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let count = Self::side_bytes(v).0;
                if *count == STICKY {
                    return false;
                }
                *count -= 1;
                *count == 0
            } else {
                let half = Self::half(v);
                *half -= 1;
                *half & COUNT == 0
            }
        }
    }

    fn to_zct(heap: &mut DeferredHeap, v: Value) {
        if !Self::in_zct(v) {
            Self::set_in_zct(v, true);
            heap.zct.push(v);
        }
    }
}

// SAFETY: allocation comes from the page heap, whose blocks are 8-byte
// aligned and unused until freed, and freed only by an epoch, when nothing
// refers to them.
unsafe impl<const SIDE_COUNTS: bool> Allocate for Deferred<SIDE_COUNTS> {
    const LAYOUT: StrategyLayout = StrategyLayout {
        pair_header: !SIDE_COUNTS,
        flonum_header: true,
    };
    type Heap = DeferredHeap;

    fn new_heap(config: &HeapConfig) -> DeferredHeap {
        DeferredHeap {
            pages: PageHeap::new(PageSource::new(config), if SIDE_COUNTS { 2 } else { 0 }),
            zct: Vec::new(),
            since: 0,
        }
    }

    #[inline]
    fn allocate(
        heap: &mut DeferredHeap,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        heap.since += words * 8;
        heap.pages.allocate(space, words)
    }

    fn bytes_used(heap: &DeferredHeap) -> usize {
        heap.pages.bytes_used()
    }
}

impl<const SIDE_COUNTS: bool> References for Deferred<SIDE_COUNTS> {
    const COUNTS: bool = true;

    #[inline]
    fn retain(_heap: &mut DeferredHeap, v: Value) {
        Self::increment(v);
    }

    #[inline]
    fn release(heap: &mut DeferredHeap, v: Value) {
        if v.is_pointer() && Self::decrement(v) {
            Self::to_zct(heap, v);
        }
    }

    #[inline]
    fn allocated(heap: &mut DeferredHeap, v: Value) {
        if Self::side(v) {
            // SAFETY: a pair just allocated on a page with side bytes,
            // which may hold an earlier pair's.
            unsafe {
                let (count, flags) = Self::side_bytes(v);
                *count = 0;
                *flags = 0;
            }
        }
        // A header starts with its strategy half zero: count zero.
        Self::to_zct(heap, v);
    }

    #[inline]
    fn init(_heap: &mut DeferredHeap, new: Value) {
        Self::increment(new);
    }

    #[inline]
    fn write(heap: &mut DeferredHeap, old: Value, new: Value) {
        Self::increment(new);
        Self::release(heap, old);
    }

    fn epoch_due(heap: &DeferredHeap) -> bool {
        heap.since >= EPOCH_BYTES
    }

    fn take_zct(heap: &mut DeferredHeap) -> Vec<Value> {
        std::mem::take(&mut heap.zct)
    }

    fn in_zct(_heap: &DeferredHeap, v: Value) -> bool {
        Self::in_zct(v)
    }

    fn leave_zct(_heap: &mut DeferredHeap, v: Value) {
        Self::set_in_zct(v, false);
    }

    fn count(_heap: &DeferredHeap, v: Value) -> usize {
        Self::count_of(v)
    }

    fn decrement(_heap: &mut DeferredHeap, v: Value) -> bool {
        Self::decrement(v)
    }

    unsafe fn free(heap: &mut DeferredHeap, v: Value, words: usize) {
        let block = NonNull::new(v.address() as *mut u64).expect("an object's address");
        // SAFETY: passed on to the caller.
        unsafe { heap.pages.free(block, words) };
    }

    fn epoch_done(heap: &mut DeferredHeap) {
        heap.since = 0;
    }
}

impl Strategy for Deferred<false> {
    const NAME: &'static str = "deferred";
}

impl Strategy for Deferred<true> {
    const NAME: &'static str = "deferred-side";
}
