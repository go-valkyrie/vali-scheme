//! Memory for one heap: whole pages, and large objects allocated one by one.

use std::alloc::{self, Layout};
use std::collections::HashMap;
use std::fmt;
use std::ptr::NonNull;

/// The size of a page, which is also its alignment, so an object's page
/// starts at its address with the low bits cleared.
pub const PAGE_BYTES: usize = 32 * 1024;

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

/// How a heap is set up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapConfig {
    /// The most memory the heap may take from the system, in bytes, counting
    /// whole pages and large objects.
    pub max_bytes: Option<usize>,
}

/// Hands out pages and large objects, counts them against the heap's limit,
/// and frees them all when dropped.
pub struct PageSource {
    limit: usize,
    used: usize,
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
            blocks: HashMap::new(),
        }
    }

    /// How many bytes the source has handed out.
    pub fn bytes_used(&self) -> usize {
        self.used
    }

    /// A fresh page, `PAGE_BYTES` long and aligned to `PAGE_BYTES`.
    pub fn page(&mut self) -> Result<NonNull<u8>, OutOfMemory> {
        self.block(Layout::from_size_align(PAGE_BYTES, PAGE_BYTES).expect("a valid page layout"))
    }

    /// Memory for one large object of `bytes` bytes, 8-byte aligned.
    pub fn large(&mut self, bytes: usize) -> Result<NonNull<u8>, OutOfMemory> {
        self.block(Layout::from_size_align(bytes, 8).map_err(|_| OutOfMemory)?)
    }

    fn block(&mut self, layout: Layout) -> Result<NonNull<u8>, OutOfMemory> {
        let used = self.used.checked_add(layout.size()).ok_or(OutOfMemory)?;
        if used > self.limit {
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
            // SAFETY: allocated with exactly this layout, freed once.
            unsafe { alloc::dealloc(block.as_ptr(), layout) };
        }
    }
}

impl Drop for PageSource {
    fn drop(&mut self) {
        for (block, layout) in self.blocks.drain() {
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
}

impl Bump {
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
        }
        let object = self.cursor;
        self.cursor += bytes;
        // SAFETY: `object` lies within a page the source allocated, so it is
        // non-null.
        Ok(unsafe { NonNull::new_unchecked(object as *mut u64) })
    }
}
