//! Memory for one heap: whole pages, and large objects allocated one by one.

use std::alloc::{self, Layout};
use std::collections::HashMap;
use std::fmt;
use std::ptr::NonNull;

use crate::asan;

/// The size of a page, which is also its alignment, so an object's page
/// starts at its address with the low bits cleared.
pub const PAGE_BYTES: usize = 32 * 1024;

/// The header every page that can hold a pair starts with, and no object
/// starts at the page's first word. Its first word is `frozen`: nonzero on a
/// frozen page, one the pool's pair constants live on (ADR 0020), and zero
/// on every other. A pair is frozen if its page's first word, at its address
/// with the low bits cleared, is not zero.
///
/// This holds for every space that can hold a pair, so a heap that cannot
/// give its pair pages this header cannot be used with the check
/// (`Cx::pair_is_frozen`).
pub const PAGE_HEADER_BYTES: usize = 8;

/// Objects larger than this are allocated on their own, not in pages.
pub const LARGE_OBJECT_BYTES: usize = 8 * 1024;

/// Allocation failed: the heap's limit was reached, or the system is out of
/// memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfMemory;

impl fmt::Display for OutOfMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("out of memory")
    }
}

impl std::error::Error for OutOfMemory {}

/// How a heap is set up, and the resource limits of the isolate that owns
/// it: one configuration type for all of them (ADR 0023). `None` is
/// unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapConfig {
    /// The most memory the heap may take from the system, in bytes, counting
    /// whole pages and large objects.
    pub max_bytes: Option<usize>,
    /// The most symbols and keywords together an isolate may intern. A
    /// symbol counts for as long as the isolate lives, so a long-lived
    /// isolate that interns without bound reaches this for good (ADR 0023).
    pub max_symbols: Option<usize>,
    /// The most runtime syntax objects an isolate may hold at once, counting
    /// the ones whose heap object has not been freed (ADR 0023).
    pub max_syntaxes: Option<usize>,
}

/// Hands out pages and large objects, counts them against the heap's limit,
/// and frees them all when dropped.
pub struct PageSource {
    limit: usize,
    used: usize,
    /// Memory the runtime holds outside the pages and charged here (ADR
    /// 0023): it counts against the limit like the pages do.
    external: usize,
    /// Every block, by address, with its layout.
    blocks: HashMap<usize, Layout>,
}

// SAFETY: the blocks are owned by the source, and only freed in `drop`.
unsafe impl Send for PageSource {}

impl fmt::Debug for PageSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageSource")
            .field("limit", &self.limit)
            .field("used", &self.used)
            .finish_non_exhaustive()
    }
}

impl PageSource {
    /// A source with the limit from `config`.
    pub fn new(config: &HeapConfig) -> PageSource {
        PageSource {
            limit: config.max_bytes.unwrap_or(usize::MAX),
            used: 0,
            external: 0,
            blocks: HashMap::new(),
        }
    }

    /// How many bytes the source has handed out.
    pub fn bytes_used(&self) -> usize {
        self.used
    }

    /// Charges `bytes` of memory held outside the pages against the limit
    /// (ADR 0023). It fails when the pages and the charges together would
    /// pass it; a heap that can give pages back does so and tries again
    /// (`PageHeap::charge_external`).
    pub fn charge_external(&mut self, bytes: usize) -> Result<(), OutOfMemory> {
        match self.external.checked_add(bytes) {
            Some(total) if total.saturating_add(self.used) <= self.limit => {
                self.external = total;
                Ok(())
            }
            _ => Err(OutOfMemory),
        }
    }

    /// Gives back `bytes` charged with [`PageSource::charge_external`].
    pub fn release_external(&mut self, bytes: usize) {
        self.external = self.external.saturating_sub(bytes);
    }

    /// A fresh page, `PAGE_BYTES` long and aligned to `PAGE_BYTES`.
    pub fn page(&mut self) -> Result<NonNull<u8>, OutOfMemory> {
        self.block(Layout::from_size_align(PAGE_BYTES, PAGE_BYTES).expect("a valid page layout"))
    }

    /// A fresh frozen page: its first word, the header's `frozen` field, set.
    /// The rest of the page is the caller's to lay out.
    pub fn frozen_page(&mut self) -> Result<NonNull<u8>, OutOfMemory> {
        let page = self.page()?;
        // SAFETY: a page just allocated, `PAGE_BYTES` long and 8-byte
        // aligned.
        unsafe { (page.as_ptr() as *mut usize).write(1) };
        Ok(page)
    }

    /// Memory for one large object of `bytes` bytes, 8-byte aligned.
    pub fn large(&mut self, bytes: usize) -> Result<NonNull<u8>, OutOfMemory> {
        self.block(Layout::from_size_align(bytes, 8).map_err(|_| OutOfMemory)?)
    }

    fn block(&mut self, layout: Layout) -> Result<NonNull<u8>, OutOfMemory> {
        let used = self.used.checked_add(layout.size()).ok_or(OutOfMemory)?;
        if used.saturating_add(self.external) > self.limit {
            return Err(OutOfMemory);
        }
        // SAFETY: the layout has a non-zero size: a page, or an object of at
        // least one header word.
        let block = NonNull::new(unsafe { alloc::alloc(layout) }).ok_or(OutOfMemory)?;
        self.used = used;
        self.blocks.insert(block.as_ptr() as usize, layout);
        Ok(block)
    }

    /// Returns a large object's memory to the system.
    ///
    /// # Safety
    ///
    /// `block` must have come from [`PageSource::large`] and not have been
    /// freed since; nothing may use it afterwards.
    pub unsafe fn free_large(&mut self, block: NonNull<u8>, _bytes: usize) {
        if let Some(layout) = self.blocks.remove(&(block.as_ptr() as usize)) {
            self.used -= layout.size();
            asan::unpoison(block.as_ptr(), layout.size());
            // SAFETY: allocated with exactly this layout, freed once.
            unsafe { alloc::dealloc(block.as_ptr(), layout) };
        }
    }
}

impl Drop for PageSource {
    fn drop(&mut self) {
        for (block, layout) in self.blocks.drain() {
            asan::unpoison(block as *const u8, layout.size());
            // SAFETY: each block was allocated with exactly this layout and
            // is freed once.
            unsafe { alloc::dealloc(block as *mut u8, layout) };
        }
    }
}

/// A bump-allocation buffer over part of a page.
#[derive(Debug, Default)]
pub struct Bump {
    cursor: usize,
    end: usize,
    /// How many bytes a new page starts with before its first object: a
    /// page header, which is zeroed.
    header: usize,
}

impl Bump {
    /// A buffer whose pages start with a header of `header` bytes, zeroed:
    /// for pairs, whose pages need the header of [`PAGE_HEADER_BYTES`].
    pub const fn with_header(header: usize) -> Bump {
        Bump {
            cursor: 0,
            end: 0,
            header,
        }
    }

    /// `bytes` bytes from this buffer, refilled from `source` with a new page
    /// when it runs out. `bytes` must be a multiple of 8 and at most
    /// `PAGE_BYTES`.
    #[inline]
    pub fn allocate(
        &mut self,
        source: &mut PageSource,
        bytes: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        debug_assert!(bytes.is_multiple_of(8) && bytes <= PAGE_BYTES);
        if self.end - self.cursor < bytes {
            let page = source.page()?;
            self.cursor = page.as_ptr() as usize;
            self.end = self.cursor + PAGE_BYTES;
            if self.header != 0 {
                // SAFETY: the start of a page just allocated, which has room
                // for the header.
                unsafe { std::ptr::write_bytes(page.as_ptr(), 0, self.header) };
                self.cursor += self.header;
            }
            // Nothing may touch the rest of the page until it is allocated.
            asan::poison(self.cursor as *const u8, self.end - self.cursor);
        }
        let object = self.cursor;
        self.cursor += bytes;
        asan::unpoison(object as *const u8, bytes);
        // SAFETY: `object` lies within a page the source allocated, so it is
        // non-null.
        Ok(unsafe { NonNull::new_unchecked(object as *mut u64) })
    }
}
