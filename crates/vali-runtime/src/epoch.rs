//! Epochs, for the counting strategies (ADR 0003): count the stack's
//! references in, free what has no references at all, and count them out.
//! Precise counting has counted the stack's references all along, so its
//! epochs scan no stack.
//!
//! An epoch runs only at a safepoint: a call, a loop's backward jump, or a
//! return to the host. There, every frame slot holds a valid value, so the
//! stack is scanned exactly: each frame's slots up to the call it is making,
//! and the innermost frame's every slot. A header a returned callee left in
//! its caller's slots reads as fixnums: an aligned frame pointer, an aligned
//! return address and a position word whose low bits are zero. Raw values in
//! Rust stay valid because no epoch runs inside a Rust procedure.

use crate::code::Kind;
use crate::cx::Cx;
use crate::strategy::Strategy;
use crate::value::Value;

impl<S: Strategy> Cx<S> {
    /// The values frames hold, from the innermost frame at `fp` out to the
    /// entry frame, and the handlers of waiting `%catch`es. It also clears
    /// the live frames' words that no frame scans: see below.
    ///
    /// Deferred counting never counts out a slot, so a word a returned
    /// callee left in its caller's frame still holds its reference,
    /// uncounted. An epoch that does not scan the word (one that runs in a
    /// later, smaller callee, which does not reach it) may free the object,
    /// and a later epoch that scans the caller's frame whole, as it does the
    /// innermost, would retain the freed object. So an epoch clears the
    /// words of the frames' extent past the innermost frame's: there are no
    /// others, since each callee's header and slots start at its caller's
    /// call base. A new frame clears its own locals as it is entered, so
    /// words outside every frame need nothing.
    ///
    /// # Safety
    ///
    /// `fp` must be a bytecode function's frame at a safepoint.
    pub(crate) unsafe fn stack_roots(&self, mut fp: *mut u64) -> Vec<Value> {
        let mut roots = Vec::new();
        // Precise counting counts the stack's references, and the handlers'.
        if S::PRECISE {
            return roots;
        }
        // SAFETY: frames link to their callers up to the entry frame, and
        // each frame's slot 0 holds its closure.
        unsafe {
            let mut limit = self.frame_slots(fp);
            // In the segment being walked: the lowest word any frame's
            // slots reach, and where the scanned words end, which are those
            // of its innermost frame: the others' lie above.
            let mut lowest = fp.sub(limit);
            let mut scanned = lowest;
            loop {
                lowest = lowest.min(fp.sub(self.frame_slots(fp)));
                for i in 0..limit {
                    let v = Value::from_raw(*fp.sub(i + 1));
                    if v.is_pointer() {
                        roots.push(v);
                    }
                }
                // Frames past the outermost live one are sealed: the
                // continuation below the stack holds their values.
                let position = *fp.add(2);
                let end = position == crate::vm::ENTRY || position & crate::vm::UNDERFLOW != 0;
                // The words of this segment's frames that none scanned: the
                // words of live frames, which belong to no deeper frame.
                if end || position & crate::vm::BOUNDARY != 0 {
                    if lowest < scanned {
                        std::ptr::write_bytes(lowest, 0, scanned.offset_from(lowest) as usize);
                    }
                    if end {
                        break;
                    }
                }
                // The caller's slots from the call's base on hold this
                // frame's header and its own slots.
                limit = crate::vm::position_base(position);
                fp = *fp as *mut u64;
                if position & crate::vm::BOUNDARY != 0 {
                    // The caller is in the segment before: its words are
                    // its own.
                    lowest = fp.sub(self.frame_slots(fp));
                    scanned = fp.sub(limit);
                }
            }
        }
        roots.extend(self.control_values().filter(|v| v.is_pointer()));
        roots
    }

    /// How many slots the frame at `fp` has.
    ///
    /// # Safety
    ///
    /// `fp` must be a bytecode function's frame.
    unsafe fn frame_slots(&self, fp: *mut u64) -> usize {
        // SAFETY: passed on to the caller.
        let closure = unsafe { Value::from_raw(*fp.sub(1)) };
        // SAFETY: as above.
        match unsafe { self.procedure_code(closure) }.map(|c| &c.kind) {
            Ok(Kind::Bytecode { slots, .. }) => usize::from(*slots),
            _ => 0,
        }
    }

    /// Runs an epoch, with `roots` the values the stack holds.
    pub(crate) fn epoch(&mut self, roots: &[Value]) {
        self.run_epoch(roots, false);
    }

    /// Frees all the garbage there is now, cycles included, under a
    /// counting strategy: an epoch with a cycle collection, which otherwise
    /// runs once the isolate has allocated as much as was live after the
    /// last. Does nothing while Scheme code runs, as when a Rust procedure
    /// calls it.
    pub fn collect_garbage(&mut self) {
        if !self.running {
            self.run_epoch(&[], true);
        }
    }

    fn run_epoch(&mut self, roots: &[Value], force_cycles: bool) {
        if !S::COUNTS {
            return;
        }
        self.safepoint();
        for &v in roots {
            S::retain(self.heap_mut(), v);
        }
        let mut work = Vec::new();
        // Newest first: a new structure's parents are newer than their
        // children, so each child is freed when its turn comes, instead of
        // leaving the table referred to, a cycle candidate. (Oldest first
        // runs churning code about 10% faster without cycle collection, but
        // buffers each child of a new structure before its parent frees it.)
        let mut zct = S::take_zct(self.heap_mut());
        while let Some(v) = zct.pop() {
            if S::leave_zct(self.heap_mut(), v) {
                work.push(v);
                self.free_all(&mut work);
            }
        }
        // A Rust object or a continuation in a garbage cycle drops what it
        // holds in the table as it is freed.
        let (heap, mut tables) = self.heap_and_tables();
        let mut emptied = 0;
        let mut finalize = |v| emptied += Self::finalize_in(&mut tables, v);
        // SAFETY: the stack's references are counted in, and the table is
        // empty.
        unsafe { S::collect_cycles(heap, Self::describe, &mut finalize, force_cycles) };
        if emptied > 0 {
            S::release_external(self.heap_mut(), emptied * crate::cx::SYNTAX_SLOT_BYTES);
        }
        for &v in roots {
            S::release(self.heap_mut(), v);
        }
        S::epoch_done(self.heap_mut());
    }

    /// Frees each object in `work`, and what it referred to that no longer
    /// has references, except what still waits in the zero-count table.
    fn free_all(&mut self, work: &mut Vec<Value>) {
        let mut children = Vec::new();
        while let Some(v) = work.pop() {
            // SAFETY: `v` is a live object nothing refers to: its count is
            // zero and no frame holds it.
            unsafe {
                let words = Self::describe(v, &mut children);
                for c in children.drain(..) {
                    if S::release_child(self.heap_mut(), c) {
                        work.push(c);
                    }
                }
                // Nothing refers to it any more, even if a cycle candidate
                // keeps its memory a while.
                self.finalize(v);
                S::free(self.heap_mut(), v, words);
            }
        }
    }
}
