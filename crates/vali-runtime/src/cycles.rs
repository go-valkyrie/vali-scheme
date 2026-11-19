//! Trial deletion (Bacon and Rajan, "Concurrent Cycle Collection in
//! Reference Counted Systems", synchronous form; ADR 0003): what counting
//! alone never frees, a cycle of objects that refer only to each other.
//!
//! An object whose count drops but stays above zero may have just lost its
//! last reference from outside a cycle, so it is buffered as a candidate.
//! Collecting takes the candidates and subtracts the references inside the
//! graph they reach (marking it gray); what still has a count is referred to
//! from outside, so its references are added back (black), and the rest
//! (white) is garbage.
//!
//! Every walk keeps its own work list instead of recursing, so a long list
//! cannot overflow the stack. Counts here are exact while the walk runs:
//! a strategy that defers the stack's references counts them in first.

use vali_abi::{Header, Tag, TypeCode};

use crate::value::Value;

/// What an object is to the collector while it runs. Between collections,
/// every object is black, or purple while it is a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Colour {
    /// In use, or not yet looked at.
    Black = 0,
    /// Possibly garbage: the collector is subtracting its inner references.
    Gray = 1,
    /// Garbage.
    White = 2,
    /// A candidate: its count dropped, but not to zero.
    Purple = 3,
}

impl Colour {
    pub(crate) fn from_bits(bits: u32) -> Self {
        match bits & 3 {
            0 => Colour::Black,
            1 => Colour::Gray,
            2 => Colour::White,
            _ => Colour::Purple,
        }
    }
}

/// `v`'s size in words, with the objects it refers to pushed onto the list:
/// the runtime's description of its objects.
pub(crate) type Describe = unsafe fn(Value, &mut Vec<Value>) -> usize;

/// How a counting strategy keeps an object's count, colour and buffered
/// flag.
pub(crate) trait Counted {
    /// The strategy's heap.
    type Heap;

    fn count(v: Value) -> usize;
    /// Whether `v`'s count is sticky: nothing counts it, so it is never
    /// freed. A pool object always is; so is a heap object whose count
    /// reached its maximum.
    fn stuck(v: Value) -> bool;
    /// Counts `v` up, as a reference is restored.
    fn increment(v: Value);
    /// Counts `v` down, as a reference inside a candidate graph is
    /// subtracted: never to below zero, and never buffering.
    fn decrement(v: Value);
    fn colour(v: Value) -> Colour;
    fn set_colour(v: Value, colour: Colour);
    fn buffered(v: Value) -> bool;
    fn set_buffered(v: Value, on: bool);
    /// Returns `v`'s memory, of `words` words, to the heap.
    ///
    /// # Safety
    ///
    /// Nothing may refer to `v`, and nothing may use it afterwards.
    unsafe fn reclaim(heap: &mut Self::Heap, v: Value, words: usize);
}

/// Whether an object of `v`'s type can be part of a cycle: whether it can
/// refer to an object that can refer back. Strings, symbols, numbers and
/// bytevectors refer only to bytes, so they are never candidates (the
/// Recycler's "green" objects).
pub(crate) fn may_cycle(v: Value) -> bool {
    match v.tag() {
        Tag::Pair | Tag::Closure => true,
        Tag::Other => {
            // SAFETY: an "other" object is live and starts with its header.
            let header = Header(unsafe { *(v.address() as *const u64) });
            matches!(
                header.type_code(),
                Some(
                    TypeCode::Vector
                        | TypeCode::Record
                        | TypeCode::Box
                        | TypeCode::RecordType
                        | TypeCode::Continuation
                        | TypeCode::Code
                )
            )
        }
        _ => false,
    }
}

/// The candidate buffer and the collector's work lists.
#[derive(Debug, Default)]
pub(crate) struct Cycles {
    candidates: Vec<Value>,
    work: Vec<Value>,
    black: Vec<Value>,
    children: Vec<Value>,
}

impl Cycles {
    /// How many candidates wait.
    pub(crate) fn candidates(&self) -> usize {
        self.candidates.len()
    }

    /// `v`'s count dropped but not to zero: it may be all that keeps a
    /// cycle alive, so it waits for the next collection, purple.
    #[inline]
    pub(crate) fn possible_root<C: Counted>(&mut self, v: Value) {
        if !may_cycle(v) {
            return;
        }
        C::set_colour(v, Colour::Purple);
        if !C::buffered(v) {
            C::set_buffered(v, true);
            self.candidates.push(v);
        }
    }

    /// Frees the candidates from the `from`th on that counting freed while
    /// they waited, found black with a count of zero, so the garbage an
    /// epoch makes is not kept until the next collection.
    ///
    /// # Safety
    ///
    /// Nothing may refer to an object counting freed.
    pub(crate) unsafe fn sweep<C: Counted>(
        &mut self,
        heap: &mut C::Heap,
        from: usize,
        describe: Describe,
    ) {
        let mut kept = from;
        for i in from..self.candidates.len() {
            let v = self.candidates[i];
            if C::colour(v) == Colour::Black && C::count(v) == 0 {
                C::set_buffered(v, false);
                // SAFETY: passed on to the caller, and the buffer no longer
                // refers to `v`.
                unsafe {
                    let words = describe(v, &mut self.children);
                    self.children.clear();
                    C::reclaim(heap, v, words);
                }
            } else {
                self.candidates[kept] = v;
                kept += 1;
            }
        }
        self.candidates.truncate(kept);
    }

    /// Frees every cycle of garbage the candidates reach.
    ///
    /// # Safety
    ///
    /// Every count must be exact: each reference to an object from the
    /// heap, the runtime and the stack counted once. An object counting
    /// freed while buffered must have been left black, with a count of zero,
    /// for this to free.
    ///
    /// `finalize` is called with each object the collection frees, before
    /// its memory goes; those counting freed earlier were finalized then.
    pub(crate) unsafe fn collect<C: Counted>(
        &mut self,
        heap: &mut C::Heap,
        describe: Describe,
        finalize: &mut dyn FnMut(Value),
    ) {
        let mut roots = std::mem::take(&mut self.candidates);
        // Mark the roots: subtract the references inside what each reaches.
        roots.retain(|&s| {
            if C::colour(s) == Colour::Purple {
                // SAFETY: a candidate is live.
                unsafe { self.mark_gray::<C>(s, describe) };
                return true;
            }
            C::set_buffered(s, false);
            if C::colour(s) == Colour::Black && C::count(s) == 0 {
                // Counting freed it while it waited here.
                // SAFETY: nothing refers to it, and the buffer no longer
                // does.
                unsafe {
                    let words = describe(s, &mut self.children);
                    self.children.clear();
                    C::reclaim(heap, s, words);
                }
            }
            false
        });
        for &s in &roots {
            // SAFETY: the roots and what they reach are live.
            unsafe { self.scan::<C>(s, describe) };
        }
        for &s in &roots {
            C::set_buffered(s, false);
            // SAFETY: what is white is garbage, and the buffer no longer
            // refers to `s`.
            unsafe { self.collect_white::<C>(heap, s, describe, finalize) };
        }
        roots.clear();
        // Keep the buffer's memory for the next round.
        if self.candidates.is_empty() {
            self.candidates = roots;
        }
    }

    /// Colours what `s` reaches gray, subtracting each reference among it.
    unsafe fn mark_gray<C: Counted>(&mut self, s: Value, describe: Describe) {
        if C::colour(s) == Colour::Gray {
            return;
        }
        C::set_colour(s, Colour::Gray);
        self.work.push(s);
        while let Some(x) = self.work.pop() {
            // SAFETY: what a live object refers to is live.
            unsafe { describe(x, &mut self.children) };
            for t in self.children.drain(..) {
                // A child with a stuck count is a leaf: never freed, it
                // keeps alive what it reaches, so its references are never
                // subtracted, and neither its count nor its colour changes.
                if C::stuck(t) {
                    continue;
                }
                C::decrement(t);
                if C::colour(t) != Colour::Gray {
                    C::set_colour(t, Colour::Gray);
                    self.work.push(t);
                }
            }
        }
    }

    /// Sorts the gray graph from `s`: what still has a count is referred to
    /// from outside, and so is all it reaches; the rest is white.
    unsafe fn scan<C: Counted>(&mut self, s: Value, describe: Describe) {
        self.work.push(s);
        while let Some(x) = self.work.pop() {
            if C::colour(x) != Colour::Gray {
                continue;
            }
            if C::count(x) > 0 {
                // SAFETY: as above.
                unsafe { self.scan_black::<C>(x, describe) };
            } else {
                C::set_colour(x, Colour::White);
                // SAFETY: as above.
                unsafe { describe(x, &mut self.work) };
            }
        }
    }

    /// Colours what `s` reaches black, restoring each reference among it.
    unsafe fn scan_black<C: Counted>(&mut self, s: Value, describe: Describe) {
        C::set_colour(s, Colour::Black);
        self.black.push(s);
        while let Some(x) = self.black.pop() {
            // SAFETY: as above.
            unsafe { describe(x, &mut self.children) };
            for t in self.children.drain(..) {
                if C::stuck(t) {
                    continue;
                }
                C::increment(t);
                if C::colour(t) != Colour::Black {
                    C::set_colour(t, Colour::Black);
                    self.black.push(t);
                }
            }
        }
    }

    /// Frees the white objects `s` reaches. A white candidate still in the
    /// buffer is left for its own turn, so nothing is freed twice.
    unsafe fn collect_white<C: Counted>(
        &mut self,
        heap: &mut C::Heap,
        s: Value,
        describe: Describe,
        finalize: &mut dyn FnMut(Value),
    ) {
        if C::colour(s) != Colour::White || C::buffered(s) {
            return;
        }
        C::set_colour(s, Colour::Black);
        self.work.push(s);
        while let Some(x) = self.work.pop() {
            // SAFETY: `x` is garbage, not yet freed: it was coloured black
            // as it was pushed, so it is pushed once.
            unsafe {
                let words = describe(x, &mut self.children);
                for t in self.children.drain(..) {
                    if !C::stuck(t) && C::colour(t) == Colour::White && !C::buffered(t) {
                        C::set_colour(t, Colour::Black);
                        self.work.push(t);
                    }
                }
                finalize(x);
                C::reclaim(heap, x, words);
            }
        }
    }
}
