//! The two counting strategies (ADR 0003), which share everything but how
//! they treat the stack.
//!
//! Deferred counting (Deutsch and Bobrow) counts only references from the
//! heap, variables' cells and the runtime's own tables, never those from
//! frames, so calls and returns cost nothing. An object whose count reaches
//! zero waits in the zero-count table until the next epoch, which counts
//! the stack's references in, frees what is still at zero, and counts them
//! out again.
//!
//! Precise counting also counts every reference from a frame slot: the VM
//! counts each slot store, and counts a frame's slots out as it returns.
//! This first version is naive, with no ownership inference, its worst
//! case. An object whose count reaches zero still waits in the zero-count
//! table until the next epoch, which needs no stack scan; so a Rust
//! procedure can hold a value whose count drops while it runs.
//!
//! Cycles of garbage are found by trial deletion (`cycles`), inside an
//! epoch while every reference is counted, once the isolate has allocated
//! as much as was live after the last collection. Its candidates are the
//! objects whose count dropped but not to zero: under deferred counting,
//! including when an epoch counts the stack's references out. Deferred
//! counting also takes the objects that leave the zero-count table still
//! referred to. A new object starts out referred to only by the stack, so
//! if it is then closed into a cycle and dropped between epochs, nothing
//! ever counts it down.
//!
//! An object's count lives in the strategy's half of its header word (ADR
//! 0002 #7), with its colour and flags. With side counts, a pair has no
//! header: its count is a byte in its page's side table, sticky at 255, and
//! its colour and flags another byte. An object with a sticky count is never
//! freed.

use std::ptr::NonNull;

use vali_abi::StrategyLayout;

use crate::cycles::{Colour, Counted, Cycles, Describe};
use crate::page::{HeapConfig, OutOfMemory, PageSource};
use crate::page_heap::PageHeap;
use crate::strategy::{Allocate, References, Space, Strategy};
use crate::value::Value;

/// Reference counting: deferred, or with `PRECISE` counting frames' slots
/// too. With `SIDE_COUNTS`, pairs carry no header and keep their counts in
/// their pages' side tables.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counting<const SIDE_COUNTS: bool, const PRECISE: bool>;

/// Deferred counting with every object headered.
pub type DeferredHeadered = Counting<false, false>;

/// Deferred counting with pairs' counts in side tables.
pub type DeferredSideCounts = Counting<true, false>;

/// Precise counting with every object headered.
pub type PreciseHeadered = Counting<false, true>;

/// Precise counting with pairs' counts in side tables.
pub type PreciseSideCounts = Counting<true, true>;

/// How much an isolate allocates between epochs.
const EPOCH_BYTES: usize = 1 << 20;

/// How much an isolate allocates between cycle collections, at least.
const CYCLE_BYTES: usize = 4 << 20;

/// A heap under a counting strategy.
#[derive(Debug)]
pub struct CountingHeap {
    pages: PageHeap,
    /// The zero-count table: objects whose count reached zero, to free at
    /// the next epoch unless a frame refers to them.
    zct: Vec<Value>,
    /// Bytes allocated since the last epoch.
    since: usize,
    /// The cycle collector's candidates and work lists.
    cycles: Cycles,
    /// Bytes allocated before this epoch since the last cycle collection.
    cycle_bytes: usize,
    /// How many bytes allocated make a cycle collection due: what was live
    /// after the last.
    cycle_threshold: usize,
    /// How many candidates the last epoch left, which it swept.
    swept: usize,
}

impl CountingHeap {
    /// How many bytes live objects take, garbage still awaiting an epoch
    /// included.
    pub fn bytes_live(&self) -> usize {
        self.pages.bytes_live()
    }

    /// How many cycle candidates wait for the next collection.
    pub fn cycle_candidates(&self) -> usize {
        self.cycles.candidates()
    }
}

/// In the header's strategy half: the count, the colour, the cycle
/// buffer's flag and the zero-count table's flag.
const COUNT: u32 = (1 << 28) - 1;
const COLOUR_SHIFT: u32 = 28;
const BUFFERED: u32 = 1 << 30;
const IN_ZCT: u32 = 1 << 31;

/// In a side table: a pair's count, sticky at this, and its flags byte:
/// the zero-count table's flag, the colour and the cycle buffer's flag.
const STICKY: u8 = u8::MAX;
const SIDE_IN_ZCT: u8 = 1;
const SIDE_COLOUR_SHIFT: u8 = 1;
const SIDE_BUFFERED: u8 = 1 << 3;

/// What `count_down` returns for a sticky count.
const STUCK: usize = usize::MAX;

impl<const SIDE_COUNTS: bool, const PRECISE: bool> Counting<SIDE_COUNTS, PRECISE> {
    #[inline(always)]
    fn side(v: Value) -> bool {
        SIDE_COUNTS && v.is_pair()
    }

    /// The strategy's half of `v`'s header word.
    ///
    /// # Safety
    ///
    /// `v` must be a live object with a header.
    #[inline(always)]
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
    #[inline(always)]
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
        Self::set_flag(v, IN_ZCT, SIDE_IN_ZCT, on);
    }

    /// Sets or clears a flag: `flag` in a header, `side_flag` in a side
    /// table.
    fn set_flag(v: Value, flag: u32, side_flag: u8, on: bool) {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let flags = Self::side_bytes(v).1;
                *flags = if on {
                    *flags | side_flag
                } else {
                    *flags & !side_flag
                };
            } else {
                let half = Self::half(v);
                *half = if on { *half | flag } else { *half & !flag };
            }
        }
    }

    #[inline(always)]
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

    /// Counts `v` down, returning its new count, or `STUCK` for a sticky
    /// side count, which stays.
    #[inline(always)]
    fn count_down(v: Value) -> usize {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let count = Self::side_bytes(v).0;
                if *count == STICKY {
                    return STUCK;
                }
                debug_assert!(*count > 0, "counting down a count of zero");
                *count -= 1;
                usize::from(*count)
            } else {
                let half = Self::half(v);
                debug_assert!(*half & COUNT > 0, "counting down a count of zero");
                *half -= 1;
                (*half & COUNT) as usize
            }
        }
    }

    fn to_zct(heap: &mut CountingHeap, v: Value) {
        if !Self::in_zct(v) {
            Self::set_in_zct(v, true);
            heap.zct.push(v);
        }
    }

    /// The cycle buffer still refers to `v`, which nothing else does: the
    /// next collection frees it, finding it black with a count of zero.
    #[cold]
    #[inline(never)]
    fn free_buffered(v: Value) {
        Self::set_colour(v, Colour::Black);
    }

    /// `v` lost a reference, or left the zero-count table, and is still
    /// referred to, its count `count`: it may be all that keeps a cycle
    /// alive.
    ///
    /// An object waiting in the zero-count table is not buffered: it is
    /// looked at again as it leaves the table. So freeing a vector that
    /// holds a new pair twice does not buffer the pair on the way to zero.
    #[inline(never)]
    fn survived(heap: &mut CountingHeap, v: Value, count: usize) {
        if count == STUCK || (Self::side(v) && count == usize::from(STICKY)) {
            return;
        }
        if Self::in_zct(v) {
            // Precise counting marks it, to buffer if it leaves the table
            // still referred to; deferred counting buffers every object
            // that does.
            if PRECISE {
                Self::set_colour(v, Colour::Purple);
            }
            return;
        }
        heap.cycles.possible_root::<Self>(v);
    }
}

impl<const SIDE_COUNTS: bool, const PRECISE: bool> Counted for Counting<SIDE_COUNTS, PRECISE> {
    type Heap = PageHeap;

    fn count(v: Value) -> usize {
        Self::count_of(v)
    }

    fn increment(v: Value) {
        Self::increment(v);
    }

    fn decrement(v: Value) {
        Self::count_down(v);
    }

    fn colour(v: Value) -> Colour {
        // SAFETY: counted values are live objects.
        unsafe {
            if Self::side(v) {
                Colour::from_bits(u32::from(*Self::side_bytes(v).1 >> SIDE_COLOUR_SHIFT))
            } else {
                Colour::from_bits(*Self::half(v) >> COLOUR_SHIFT)
            }
        }
    }

    fn set_colour(v: Value, colour: Colour) {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                let flags = Self::side_bytes(v).1;
                *flags = (*flags & !(3 << SIDE_COLOUR_SHIFT)) | (colour as u8) << SIDE_COLOUR_SHIFT;
            } else {
                let half = Self::half(v);
                *half = (*half & !(3 << COLOUR_SHIFT)) | (colour as u32) << COLOUR_SHIFT;
            }
        }
    }

    fn buffered(v: Value) -> bool {
        // SAFETY: as above.
        unsafe {
            if Self::side(v) {
                *Self::side_bytes(v).1 & SIDE_BUFFERED != 0
            } else {
                *Self::half(v) & BUFFERED != 0
            }
        }
    }

    fn set_buffered(v: Value, on: bool) {
        Self::set_flag(v, BUFFERED, SIDE_BUFFERED, on);
    }

    unsafe fn reclaim(pages: &mut PageHeap, v: Value, words: usize) {
        let block = NonNull::new(v.address() as *mut u64).expect("an object's address");
        // SAFETY: passed on to the caller.
        unsafe { pages.free(block, words) };
    }
}

// SAFETY: allocation comes from the page heap, whose blocks are 8-byte
// aligned and unused until freed, and freed only by an epoch, when nothing
// refers to them.
unsafe impl<const SIDE_COUNTS: bool, const PRECISE: bool> Allocate
    for Counting<SIDE_COUNTS, PRECISE>
{
    const LAYOUT: StrategyLayout = StrategyLayout {
        pair_header: !SIDE_COUNTS,
        flonum_header: true,
    };
    type Heap = CountingHeap;

    fn new_heap(config: &HeapConfig) -> CountingHeap {
        CountingHeap {
            pages: PageHeap::new(PageSource::new(config), if SIDE_COUNTS { 2 } else { 0 }),
            zct: Vec::new(),
            since: 0,
            cycles: Cycles::default(),
            cycle_bytes: 0,
            cycle_threshold: CYCLE_BYTES,
            swept: 0,
        }
    }

    #[inline]
    fn allocate(
        heap: &mut CountingHeap,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        heap.since += words * 8;
        heap.pages.allocate(space, words)
    }

    fn bytes_used(heap: &CountingHeap) -> usize {
        heap.pages.bytes_used()
    }
}

impl<const SIDE_COUNTS: bool, const PRECISE: bool> References for Counting<SIDE_COUNTS, PRECISE> {
    const COUNTS: bool = true;
    const PRECISE: bool = PRECISE;

    #[inline(always)]
    fn retain(_heap: &mut CountingHeap, v: Value) {
        Self::increment(v);
    }

    #[inline(always)]
    fn release(heap: &mut CountingHeap, v: Value) {
        if v.is_pointer() {
            match Self::count_down(v) {
                0 => Self::to_zct(heap, v),
                count => Self::survived(heap, v, count),
            }
        }
    }

    #[inline]
    fn allocated(heap: &mut CountingHeap, v: Value) {
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
    fn init(_heap: &mut CountingHeap, new: Value) {
        Self::increment(new);
    }

    #[inline(always)]
    fn write(heap: &mut CountingHeap, old: Value, new: Value) {
        Self::increment(new);
        Self::release(heap, old);
    }

    fn epoch_due(heap: &CountingHeap) -> bool {
        heap.since >= EPOCH_BYTES
    }

    fn take_zct(heap: &mut CountingHeap) -> Vec<Value> {
        std::mem::take(&mut heap.zct)
    }

    fn in_zct(_heap: &CountingHeap, v: Value) -> bool {
        Self::in_zct(v)
    }

    fn leave_zct(heap: &mut CountingHeap, v: Value) -> bool {
        Self::set_in_zct(v, false);
        match Self::count_of(v) {
            0 => true,
            count => {
                // Under precise counting, the stack's reference to a new
                // object was counted, so only one counted down while it
                // waited, and marked purple, may be keeping a cycle alive.
                if !PRECISE || Self::colour(v) == Colour::Purple {
                    Self::survived(heap, v, count);
                }
                false
            }
        }
    }

    fn release_child(heap: &mut CountingHeap, v: Value) -> bool {
        match Self::count_down(v) {
            0 => !Self::in_zct(v),
            count => {
                Self::survived(heap, v, count);
                false
            }
        }
    }

    #[inline]
    unsafe fn free(heap: &mut CountingHeap, v: Value, words: usize) {
        if Self::buffered(v) {
            Self::free_buffered(v);
            return;
        }
        // SAFETY: passed on to the caller.
        unsafe { Self::reclaim(&mut heap.pages, v, words) };
    }

    unsafe fn collect_cycles(
        heap: &mut CountingHeap,
        describe: Describe,
        finalize: &mut dyn FnMut(Value),
        force: bool,
    ) {
        let CountingHeap { pages, cycles, .. } = heap;
        if force || heap.cycle_bytes + heap.since >= heap.cycle_threshold {
            // SAFETY: passed on to the caller: the stack's references are
            // counted in, so every count is exact.
            unsafe { cycles.collect::<Self>(pages, describe, finalize) };
            heap.cycle_bytes = 0;
            heap.cycle_threshold = CYCLE_BYTES.max(heap.pages.bytes_live());
        } else {
            // SAFETY: what counting freed, nothing refers to.
            unsafe { cycles.sweep::<Self>(pages, heap.swept, describe) };
        }
        heap.swept = heap.cycles.candidates();
    }

    fn epoch_done(heap: &mut CountingHeap) {
        heap.cycle_bytes += heap.since;
        heap.since = 0;
    }
}

impl Strategy for Counting<false, false> {
    const NAME: &'static str = "deferred";
}

impl Strategy for Counting<true, false> {
    const NAME: &'static str = "deferred-side";
}

impl Strategy for Counting<false, true> {
    const NAME: &'static str = "precise";
}

impl Strategy for Counting<true, true> {
    const NAME: &'static str = "precise-side";
}
