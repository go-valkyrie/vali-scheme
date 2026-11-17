//! Epochs, for the counting strategies (ADR 0003): count the stack's
//! references in, free what has no references at all, and count them out.
//!
//! An epoch runs only at a safepoint: a call, a loop's backward jump, or a
//! return to the host. There, every frame slot holds a valid value, so the
//! stack is scanned exactly: each frame's slots up to the call it is making,
//! and the innermost frame's every slot. Raw values in Rust stay valid
//! because no epoch runs inside a Rust procedure.

use crate::code::Kind;
use crate::cx::Cx;
use crate::strategy::Strategy;
use crate::value::Value;

impl<S: Strategy> Cx<S> {
    /// The values frames hold, from the innermost frame at `fp` out to the
    /// entry frame, and the handlers of waiting `%catch`es.
    ///
    /// # Safety
    ///
    /// `fp` must be a bytecode function's frame at a safepoint.
    pub(crate) unsafe fn stack_roots(&self, mut fp: *mut u64) -> Vec<Value> {
        let mut roots = Vec::new();
        // SAFETY: frames link to their callers up to the entry frame, and
        // each frame's slot 0 holds its closure.
        unsafe {
            let mut limit = self.frame_slots(fp);
            loop {
                for i in 0..limit {
                    let v = Value::from_raw(*fp.sub(i + 1));
                    if v.is_pointer() {
                        roots.push(v);
                    }
                }
                let position = *fp.add(2);
                if position == crate::vm::ENTRY {
                    break;
                }
                // The caller's slots from the call's base on hold this
                // frame's header and its own slots.
                limit = ((position >> 32) & 0xFFFF) as usize;
                fp = *fp as *mut u64;
            }
        }
        roots.extend(self.catch_handlers().filter(|v| v.is_pointer()));
        roots
    }

    /// How many slots the frame at `fp` has.
    ///
    /// # Safety
    ///
    /// `fp` must be a bytecode function's frame.
    unsafe fn frame_slots(&self, fp: *mut u64) -> usize {
        // SAFETY: passed on to the caller.
        let closure = Value::from_raw(unsafe { *fp.sub(1) });
        // SAFETY: as above.
        match unsafe { self.procedure_code(closure) }.map(|c| &c.kind) {
            Ok(Kind::Bytecode { slots, .. }) => usize::from(*slots),
            _ => 0,
        }
    }

    /// Runs an epoch, with `roots` the values the stack holds.
    pub(crate) fn epoch(&mut self, roots: &[Value]) {
        if !S::COUNTS {
            return;
        }
        self.safepoint();
        for &v in roots {
            S::retain(self.heap_mut(), v);
        }
        let mut work = Vec::new();
        for v in S::take_zct(self.heap_mut()) {
            S::leave_zct(self.heap_mut(), v);
            if S::count(self.heap(), v) == 0 {
                work.push(v);
                self.free_all(&mut work);
            }
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
                let t = self.object_type(v).expect("a counted object");
                let words = self.object_words(v, t);
                children.clear();
                self.for_each_child(v, t, |c| {
                    if c.is_pointer() {
                        children.push(c);
                    }
                });
                for &c in &children {
                    if S::decrement(self.heap_mut(), c) && !S::in_zct(self.heap(), c) {
                        work.push(c);
                    }
                }
                S::free(self.heap_mut(), v, words);
            }
        }
    }
}
