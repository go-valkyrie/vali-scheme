//! Stack segments (ADR 0004): memory for a task's frames, growing downward
//! like a native stack. When a frame does not fit in the current segment,
//! the VM chains a new one; returning from that frame drops back to the
//! previous segment.

use std::alloc::{self, Layout};
use std::ptr::NonNull;

/// How many words a segment holds, unless a frame needs more.
pub(crate) const SEGMENT_WORDS: usize = 8 * 1024;

/// The most words a stack may take by default: 256 MiB.
pub(crate) const MAX_STACK_WORDS: usize = 32 * 1024 * 1024;

/// One stack segment.
pub(crate) struct Segment {
    base: NonNull<u64>,
    words: usize,
}

// SAFETY: the segment owns its memory, and only its isolate uses it.
unsafe impl Send for Segment {}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("words", &self.words)
            .finish()
    }
}

impl Segment {
    fn layout(words: usize) -> Layout {
        Layout::array::<u64>(words)
            .and_then(|l| l.align_to(16))
            .expect("a valid segment layout")
    }

    /// A segment of `words` words, zeroed: every word reads as a fixnum
    /// until it is stored to, which precise counting relies on.
    pub(crate) fn new(words: usize) -> Segment {
        let layout = Self::layout(words);
        // SAFETY: the layout is non-zero in size.
        let base = unsafe { alloc::alloc_zeroed(layout) } as *mut u64;
        let base = NonNull::new(base).unwrap_or_else(|| alloc::handle_alloc_error(layout));
        Segment { base, words }
    }

    /// How many words the segment holds.
    pub(crate) fn words(&self) -> usize {
        self.words
    }

    /// One past the highest word: where the stack starts.
    pub(crate) fn top(&self) -> *mut u64 {
        // SAFETY: one past the end of the allocation.
        unsafe { self.base.as_ptr().add(self.words) }
    }

    /// The lowest word: frames must not extend below it.
    pub(crate) fn limit(&self) -> *mut u64 {
        self.base.as_ptr()
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { alloc::dealloc(self.base.as_ptr() as *mut u8, Self::layout(self.words)) };
    }
}
