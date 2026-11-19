//! A heap that frees: pages with a free list each, as mimalloc has (ADR
//! 0003), for the counting strategies.
//!
//! Every page is `PAGE_BYTES` long and aligned to that, so an object's page
//! header is its address with the low bits cleared. A page holds blocks of
//! one size class, for one space. Allocation takes a block from the current
//! page of its class, first from the page's list of freed blocks, then from
//! the part never used; when the page runs out, the class moves to another
//! of its pages with free blocks, then to an empty page, then to a fresh
//! one. Freeing pushes the block onto its own page's list. A page whose
//! blocks are all free, and that isn't a class's current page, becomes an
//! empty page that any class can take.
//!
//! A pair page can keep a side table of bytes, one per block, between its
//! header and its blocks: the side-count pair layout keeps counts there.
//! Objects larger than `LARGE_OBJECT_BYTES` are allocated and freed one by
//! one.

use std::fmt;
use std::ptr::{self, NonNull};

use crate::asan;
use crate::page::{LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES, PAGE_HEADER_BYTES, PageSource};
use crate::strategy::Space;

/// The block sizes, in words. Each is at most a quarter above the one
/// before, so no block wastes more than a fifth of itself.
const CLASSES: [usize; 36] = [
    1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160,
    192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024,
];

const LARGEST: usize = LARGE_OBJECT_BYTES / 8;

/// The class of each size up to the largest, in words.
const CLASS_OF: [u8; LARGEST + 1] = {
    let mut table = [0u8; LARGEST + 1];
    let mut words = 1;
    let mut class = 0;
    while words <= LARGEST {
        while CLASSES[class] < words {
            class += 1;
        }
        table[words] = class as u8;
        words += 1;
    }
    table
};

const SPACES: usize = 4;

fn space_index(space: Space) -> usize {
    match space {
        Space::Pair => 0,
        Space::Flonum => 1,
        Space::General => 2,
        Space::Literal => 3,
    }
}

/// The header at the start of every page. Its first word, `frozen`, is
/// where the write check of a pair looks (`PAGE_HEADER_BYTES`): nonzero on a
/// frozen pair page, which holds the pool's constants (ADR 0020) and is
/// never in a list, a class or the heap's accounting, and zero on every page
/// the heap allocates. It is a field of its own: `space` is an index into
/// `classes`, which `free` uses.
#[repr(C)]
struct Page {
    frozen: usize,
    space: usize,
    class: usize,
    /// The size of each block, in bytes.
    block_bytes: usize,
    /// Where the blocks start.
    blocks: *mut u8,
    /// The side table, `stride` bytes per block, or null.
    side: *mut u8,
    stride: usize,
    /// Blocks in use.
    used: usize,
    /// Freed blocks, each holding the next in its first word.
    free: *mut u64,
    /// The part never used: from `bump` to `end`.
    bump: *mut u8,
    end: *mut u8,
    /// Links in its class's list of pages with free blocks, or in the list
    /// of empty pages.
    prev: *mut Page,
    next: *mut Page,
    /// Whether it is in a list.
    listed: bool,
}

/// Where a page's header ends, rounded to whole words.
const HEADER_BYTES: usize = std::mem::size_of::<Page>().next_multiple_of(16);

/// A doubly linked list of pages.
#[derive(Clone, Copy)]
struct List {
    head: *mut Page,
}

impl List {
    const EMPTY: List = List {
        head: ptr::null_mut(),
    };

    /// # Safety
    ///
    /// `page` must be a live page in no list.
    unsafe fn push(&mut self, page: *mut Page) {
        // SAFETY: passed on to the caller; the head, if any, is live.
        unsafe {
            (*page).prev = ptr::null_mut();
            (*page).next = self.head;
            (*page).listed = true;
            if !self.head.is_null() {
                (*self.head).prev = page;
            }
        }
        self.head = page;
    }

    /// # Safety
    ///
    /// `page` must be a live page in this list.
    unsafe fn remove(&mut self, page: *mut Page) {
        // SAFETY: passed on to the caller; its neighbours are live.
        unsafe {
            let (prev, next) = ((*page).prev, (*page).next);
            if prev.is_null() {
                self.head = next;
            } else {
                (*prev).next = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
            (*page).listed = false;
        }
    }

    fn pop(&mut self) -> Option<*mut Page> {
        let page = self.head;
        if page.is_null() {
            return None;
        }
        // SAFETY: the head of a list is a live page in it.
        unsafe { self.remove(page) };
        Some(page)
    }
}

/// One size class of one space.
#[derive(Clone, Copy)]
struct Class {
    /// The page allocation takes from, or null.
    current: *mut Page,
    /// Its other pages with free blocks.
    available: List,
}

/// A heap of pages that frees.
pub struct PageHeap {
    source: PageSource,
    classes: [[Class; CLASSES.len()]; SPACES],
    /// Pages whose blocks are all free, for any class.
    empty: List,
    /// Side-table bytes per block on pair pages.
    pair_side_bytes: usize,
    /// Live bytes in blocks and large objects.
    live: usize,
}

// SAFETY: the pages are owned by the heap's source and reached only
// through the heap, which one thread at a time uses (ADR 0008).
unsafe impl Send for PageHeap {}

impl fmt::Debug for PageHeap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageHeap")
            .field("source", &self.source)
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

/// The page `object` is on.
fn page_of(object: *const u64) -> *mut Page {
    (object as usize & !(PAGE_BYTES - 1)) as *mut Page
}

impl PageHeap {
    /// An empty heap whose pair pages keep `pair_side_bytes` bytes per
    /// block in a side table.
    pub fn new(source: PageSource, pair_side_bytes: usize) -> PageHeap {
        PageHeap {
            source,
            classes: [[Class {
                current: ptr::null_mut(),
                available: List::EMPTY,
            }; CLASSES.len()]; SPACES],
            empty: List::EMPTY,
            pair_side_bytes,
            live: 0,
        }
    }

    /// How many bytes the heap has taken from the system.
    pub fn bytes_used(&self) -> usize {
        self.source.bytes_used()
    }

    /// Charges memory held outside the heap against its limit. When the
    /// pages and the charges would pass it, gives empty pages back to the
    /// source, one at a time, until they fit; with none left to give, it
    /// fails (ADR 0023).
    pub fn charge_external(&mut self, bytes: usize) -> Result<(), OutOfMemory> {
        loop {
            if self.source.charge_external(bytes).is_ok() {
                return Ok(());
            }
            let Some(page) = self.empty.pop() else {
                return Err(OutOfMemory);
            };
            // SAFETY: a page on the empty list holds no live block, is no
            // class's current page and is in no other list, so nothing
            // refers to it; it came from the source as a page.
            unsafe {
                self.source
                    .free_large(NonNull::new_unchecked(page.cast()), PAGE_BYTES)
            };
        }
    }

    /// Gives back what `charge_external` charged.
    pub fn release_external(&mut self, bytes: usize) {
        self.source.release_external(bytes);
    }

    /// How many bytes live objects take.
    pub fn bytes_live(&self) -> usize {
        self.live
    }

    /// Memory for `words` words in `space`.
    #[inline]
    pub fn allocate(&mut self, space: Space, words: usize) -> Result<NonNull<u64>, OutOfMemory> {
        let bytes = words.checked_mul(8).ok_or(OutOfMemory)?;
        if words > LARGEST {
            let block = self.source.large(bytes)?;
            self.live += bytes;
            return Ok(block.cast());
        }
        let (s, c) = (space_index(space), usize::from(CLASS_OF[words]));
        loop {
            let current = self.classes[s][c].current;
            if !current.is_null() {
                // SAFETY: a class's current page is live.
                if let Some(block) = unsafe { Self::take(current) } {
                    self.live += CLASSES[c] * 8;
                    return Ok(block);
                }
                self.classes[s][c].current = ptr::null_mut();
            }
            let next = match self.classes[s][c].available.pop() {
                Some(page) => page,
                None => self.fresh(s, c)?,
            };
            self.classes[s][c].current = next;
        }
    }

    /// A block from `page`, if it has one free.
    ///
    /// # Safety
    ///
    /// `page` must be a live page.
    #[inline]
    unsafe fn take(page: *mut Page) -> Option<NonNull<u64>> {
        // SAFETY: passed on to the caller; freed blocks hold the next one.
        unsafe {
            let p = &mut *page;
            let block = if !p.free.is_null() {
                let block = p.free;
                p.free = *block as *mut u64;
                block
            } else if p.end as usize - p.bump as usize >= p.block_bytes {
                let block = p.bump as *mut u64;
                p.bump = p.bump.add(p.block_bytes);
                block
            } else {
                return None;
            };
            asan::unpoison(block, p.block_bytes);
            p.used += 1;
            Some(NonNull::new_unchecked(block))
        }
    }

    /// An empty page set up for class `c` of space `s`: one whose blocks
    /// were all freed, or a fresh one.
    fn fresh(&mut self, s: usize, c: usize) -> Result<*mut Page, OutOfMemory> {
        let page = match self.empty.pop() {
            Some(page) => page,
            None => self.source.page()?.as_ptr() as *mut Page,
        };
        let block_bytes = CLASSES[c] * 8;
        // Pairs, and literal pairs, which are pairs on frozen pages.
        let side = if s == 0 || s == 3 {
            self.pair_side_bytes
        } else {
            0
        };
        let room = PAGE_BYTES - HEADER_BYTES;
        let capacity = room / (block_bytes + side);
        let base = page as *mut u8;
        // A page that was emptied holds poisoned blocks.
        asan::unpoison(base, PAGE_BYTES);
        // SAFETY: the page is `PAGE_BYTES` long and nothing else uses it;
        // the side table and blocks fit after the header by construction.
        unsafe {
            let side_table = base.add(HEADER_BYTES);
            let blocks = side_table.add((capacity * side).next_multiple_of(8));
            // Nothing may touch the blocks until they are taken.
            asan::poison(blocks, PAGE_BYTES - (blocks as usize - base as usize));
            ptr::write(
                page,
                Page {
                    frozen: usize::from(s == 3),
                    space: s,
                    class: c,
                    block_bytes,
                    blocks,
                    side: if side == 0 {
                        ptr::null_mut()
                    } else {
                        side_table
                    },
                    stride: side,
                    used: 0,
                    free: ptr::null_mut(),
                    bump: blocks,
                    end: blocks.add(capacity * block_bytes),
                    prev: ptr::null_mut(),
                    next: ptr::null_mut(),
                    listed: false,
                },
            );
        }
        Ok(page)
    }

    /// A frozen page for the pool's pairs of `words` words each (ADR 0020),
    /// laid out as [`Page`] lays out a pair page, because `side_byte` reads
    /// that layout: a header, the side table if pairs have one, preset to
    /// sticky counts and no flags, and the blocks. The page is in none of
    /// the heap's lists, and nothing frees it but the heap's source when it
    /// is dropped. Returns where the first pair goes and where the last
    /// ends.
    pub fn pool_pair_page(
        &mut self,
        words: usize,
        sticky: u8,
    ) -> Result<(usize, usize), OutOfMemory> {
        let page = self.source.page()?.as_ptr() as *mut Page;
        let block_bytes = words * 8;
        let side = self.pair_side_bytes;
        let room = PAGE_BYTES - HEADER_BYTES;
        let capacity = room / (block_bytes + side);
        let base = page as *mut u8;
        // SAFETY: the page is `PAGE_BYTES` long and nothing else uses it;
        // the side table and blocks fit after the header by construction.
        unsafe {
            let side_table = base.add(HEADER_BYTES);
            let blocks = side_table.add((capacity * side).next_multiple_of(8));
            let end = blocks.add(capacity * block_bytes);
            for i in 0..capacity * side {
                side_table
                    .add(i)
                    .write(if i % side == 0 { sticky } else { 0 });
            }
            ptr::write(
                page,
                Page {
                    frozen: 1,
                    space: 0,
                    class: usize::from(CLASS_OF[words]),
                    block_bytes,
                    blocks,
                    side: if side == 0 {
                        ptr::null_mut()
                    } else {
                        side_table
                    },
                    stride: side,
                    // Full, with nothing to free.
                    used: capacity,
                    free: ptr::null_mut(),
                    bump: end,
                    end,
                    prev: ptr::null_mut(),
                    next: ptr::null_mut(),
                    listed: false,
                },
            );
            Ok((blocks as usize, end as usize))
        }
    }

    /// A frozen page for the pool's other objects: its `frozen` field set,
    /// and objects from after it. Returns where the first goes and where
    /// the page ends.
    pub fn pool_page(&mut self) -> Result<(usize, usize), OutOfMemory> {
        let page = self.source.frozen_page()?.as_ptr() as usize;
        Ok((page + PAGE_HEADER_BYTES, page + PAGE_BYTES))
    }

    /// Memory for one large pool object, which the heap neither counts as
    /// live nor ever frees until it is dropped.
    pub fn pool_large(&mut self, bytes: usize) -> Result<NonNull<u64>, OutOfMemory> {
        self.source.large(bytes).map(NonNull::cast)
    }

    /// Frees `object`, of `words` words, which this heap allocated.
    ///
    /// # Safety
    ///
    /// `object` must have come from [`PageHeap::allocate`] with `words`,
    /// and not have been freed since. Nothing may use it afterwards.
    pub unsafe fn free(&mut self, object: NonNull<u64>, words: usize) {
        if words > LARGEST {
            self.live -= words * 8;
            // SAFETY: passed on to the caller.
            unsafe { self.source.free_large(object.cast(), words * 8) };
            return;
        }
        let page = page_of(object.as_ptr());
        // SAFETY: the object's page is live, and holds it.
        unsafe {
            let p = &mut *page;
            *object.as_ptr() = p.free as u64;
            p.free = object.as_ptr();
            // Only the link, in the first word, stays readable.
            asan::poison(object.as_ptr().add(1), p.block_bytes - 8);
            p.used -= 1;
            self.live -= p.block_bytes;
            let class = &mut self.classes[p.space][p.class];
            if class.current == page {
                return;
            }
            if p.used == 0 {
                // All free: any class can have it.
                if p.listed {
                    class.available.remove(page);
                }
                self.empty.push(page);
            } else if !p.listed {
                // It was full; now it has a free block.
                class.available.push(page);
            }
        }
    }

    /// The first side-table byte of `object`, on a pair page that has a
    /// side table; the object's others follow it.
    ///
    /// # Safety
    ///
    /// `object` must be a live block on a page with a side table.
    #[inline(always)]
    pub unsafe fn side_byte(object: *const u64) -> *mut u8 {
        let page = page_of(object);
        // SAFETY: passed on to the caller.
        unsafe {
            let p = &*page;
            let index = (object as usize - p.blocks as usize) / p.block_bytes;
            p.side.add(index * p.stride)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::HeapConfig;

    fn heap() -> PageHeap {
        PageHeap::new(PageSource::new(&HeapConfig::default()), 0)
    }

    #[test]
    fn classes_cover_every_size() {
        for (words, &class) in CLASS_OF.iter().enumerate().skip(1) {
            let class = usize::from(class);
            assert!(CLASSES[class] >= words);
            assert!(class == 0 || CLASSES[class - 1] < words);
        }
    }

    #[test]
    fn freed_blocks_are_reused() {
        let mut heap = heap();
        let a = heap.allocate(Space::Pair, 2).unwrap();
        let b = heap.allocate(Space::Pair, 2).unwrap();
        assert_ne!(a, b);
        // SAFETY: allocated above, freed once.
        unsafe { heap.free(a, 2) };
        assert_eq!(heap.allocate(Space::Pair, 2).unwrap(), a);
        assert_eq!(heap.bytes_live(), 32);
    }

    #[test]
    fn empty_pages_change_class() {
        let mut heap = heap();
        // Fill several pages of one class, then free them all.
        let blocks: Vec<_> = (0..10_000)
            .map(|_| heap.allocate(Space::General, 3).unwrap())
            .collect();
        let used = heap.bytes_used();
        for &b in &blocks {
            // SAFETY: each allocated above and freed once.
            unsafe { heap.free(b, 3) };
        }
        assert_eq!(heap.bytes_live(), 0);
        // Another class reuses the emptied pages instead of taking more.
        let more: Vec<_> = (0..5_000)
            .map(|_| heap.allocate(Space::General, 5).unwrap())
            .collect();
        assert_eq!(heap.bytes_used(), used);
        assert_eq!(more.len(), 5_000);
    }

    #[test]
    fn large_objects_are_freed() {
        let mut heap = heap();
        let big = heap.allocate(Space::General, 5_000).unwrap();
        assert!(heap.bytes_used() >= 40_000);
        // SAFETY: allocated above.
        unsafe { heap.free(big, 5_000) };
        assert_eq!(heap.bytes_live(), 0);
        assert!(heap.bytes_used() < 40_000);
    }

    #[test]
    fn side_tables_have_bytes_per_block() {
        // Two bytes each: neighbours' bytes must not overlap.
        let mut heap = PageHeap::new(PageSource::new(&HeapConfig::default()), 2);
        let a = heap.allocate(Space::Pair, 2).unwrap();
        let b = heap.allocate(Space::Pair, 2).unwrap();
        // SAFETY: both live on a pair page with a side table.
        unsafe {
            *PageHeap::side_byte(a.as_ptr()) = 7;
            *PageHeap::side_byte(a.as_ptr()).add(1) = 8;
            *PageHeap::side_byte(b.as_ptr()) = 9;
            assert_eq!(*PageHeap::side_byte(a.as_ptr()), 7);
            assert_eq!(*PageHeap::side_byte(a.as_ptr()).add(1), 8);
            assert_eq!(*PageHeap::side_byte(b.as_ptr()), 9);
        }
    }
}
