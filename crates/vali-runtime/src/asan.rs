//! AddressSanitizer poisoning for the memory Vali carves up itself.
//!
//! ASan sees only the blocks the system allocator hands out, so it misses
//! a use of a freed object whose page is still live, and a write just past
//! a frame inside its stack segment. With the `asan` feature, the heap and
//! the stack poison the parts of their blocks nothing may touch (the
//! unused part of a page, a freed block, the stack below the lowest live
//! frame) and unpoison them as they are handed out, so ASan reports
//! accesses to them. Without the feature, both functions are empty, and
//! the calls compile away.
//!
//! The feature is for fuzz builds under `-Zsanitizer=address`, which needs
//! nightly. It is a Cargo feature rather than `cfg(sanitize = "address")`,
//! which is itself unstable: it would stop the crate building on stable.

/// Marks `bytes` bytes at `address` as off limits to ASan: any access
/// reports an error until [`unpoison`]. Both must be multiples of 8.
#[inline(always)]
pub(crate) fn poison<T>(address: *const T, bytes: usize) {
    #[cfg(feature = "asan")]
    {
        debug_assert!((address as usize).is_multiple_of(8) && bytes.is_multiple_of(8));
        // SAFETY: only changes what ASan reports; the memory is not touched.
        unsafe { __asan_poison_memory_region(address.cast(), bytes) };
    }
    #[cfg(not(feature = "asan"))]
    let _ = (address, bytes);
}

/// Makes `bytes` bytes at `address` accessible to ASan again.
#[inline(always)]
pub(crate) fn unpoison<T>(address: *const T, bytes: usize) {
    #[cfg(feature = "asan")]
    {
        debug_assert!((address as usize).is_multiple_of(8) && bytes.is_multiple_of(8));
        // SAFETY: as for `poison`.
        unsafe { __asan_unpoison_memory_region(address.cast(), bytes) };
    }
    #[cfg(not(feature = "asan"))]
    let _ = (address, bytes);
}

/// Poisons the words from `low` up to `high`, if there are any.
#[inline(always)]
pub(crate) fn poison_words(low: *const u64, high: *const u64) {
    if (low as usize) < (high as usize) {
        poison(low, high as usize - low as usize);
    }
}

/// Unpoisons the words from `low` up to `high`, if there are any.
#[inline(always)]
pub(crate) fn unpoison_words(low: *const u64, high: *const u64) {
    if (low as usize) < (high as usize) {
        unpoison(low, high as usize - low as usize);
    }
}

#[cfg(feature = "asan")]
unsafe extern "C" {
    fn __asan_poison_memory_region(address: *const std::ffi::c_void, size: usize);
    fn __asan_unpoison_memory_region(address: *const std::ffi::c_void, size: usize);
}
