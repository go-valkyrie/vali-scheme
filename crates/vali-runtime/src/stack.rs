//! Stack segments (ADR 0004): memory for a task's frames, growing downward
//! like a native stack. When a frame does not fit in the current segment,
//! the VM chains a new one; returning from that frame drops back to the
//! previous segment.
//!
//! Capturing a continuation seals the frames on the stack: they become the
//! continuation's, and the stack goes on below them in the same segment. So
//! a segment's memory is shared, and the stack holds views of it: a segment
//! and the top of the part the stack may use.

use std::alloc::{self, Layout};
use std::ptr::NonNull;
use std::sync::Arc;

use crate::asan;

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
// SAFETY: as above: shared only among its isolate's stack and
// continuations.
unsafe impl Sync for Segment {}

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

/// The part of a segment the stack may use: below `top`. Above it, if
/// anything, are frames a continuation sealed.
pub(crate) struct View {
    memory: Arc<Segment>,
    top: *mut u64,
}

// SAFETY: as for `Segment`; `top` points into it.
unsafe impl Send for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View")
            .field("words", &self.memory.words)
            .field("whole", &self.is_whole())
            .finish()
    }
}

impl View {
    /// All of `segment`.
    pub(crate) fn whole(segment: Segment) -> View {
        let top = segment.top();
        View {
            memory: Arc::new(segment),
            top,
        }
    }

    /// The same segment, used below `top` only.
    pub(crate) fn below(&self, top: *mut u64) -> View {
        View {
            memory: Arc::clone(&self.memory),
            top,
        }
    }

    /// Where the stack may start.
    pub(crate) fn top(&self) -> *mut u64 {
        self.top
    }

    /// The lowest word.
    pub(crate) fn limit(&self) -> *mut u64 {
        self.memory.limit()
    }

    /// How many words the segment holds.
    pub(crate) fn words(&self) -> usize {
        self.memory.words
    }

    /// The segment, to keep alive.
    pub(crate) fn memory(&self) -> Arc<Segment> {
        Arc::clone(&self.memory)
    }

    /// Whether this is all of a segment nothing else uses.
    pub(crate) fn is_whole(&self) -> bool {
        self.top == self.memory.top() && Arc::strong_count(&self.memory) == 1
    }

    /// The segment back, to reuse, if this is all of it and nothing else
    /// uses it. A segment part of which was sealed is not reused: words a
    /// continuation left there would read as counted values.
    pub(crate) fn reclaim(self) -> Option<Segment> {
        if self.top != self.memory.top() {
            return None;
        }
        Arc::try_unwrap(self.memory).ok()
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // The allocator may hand the block out again.
        asan::unpoison(self.base.as_ptr(), self.words * 8);
        // SAFETY: allocated in `new` with this layout.
        unsafe { alloc::dealloc(self.base.as_ptr() as *mut u8, Self::layout(self.words)) };
    }
}
