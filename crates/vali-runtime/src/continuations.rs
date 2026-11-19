//! Continuations (ADR 0004): full, multi-shot `call/cc` on stack segments,
//! after Hieb, Dybvig and Bruggeman.
//!
//! Capturing a continuation copies nothing. The frames on the stack are
//! sealed: they become the continuation's, never written again, and the
//! stack goes on below them in the same segment. A frame whose caller is
//! sealed has `UNDERFLOW` in its position word; returning from it copies
//! the caller into the live stack and resumes the copy, one frame at a
//! time, so frames that are never returned through are never copied.
//! Reinstating a continuation drops the live stack and returns into the
//! continuation's first frame the same way.
//!
//! A continuation is a heap object of type `Continuation` whose record,
//! [`Sealed`], sits in the isolate's object table. It owns the frames it
//! sealed, from its first out to one whose caller is sealed already or is
//! the host, and refers to the continuation that holds that caller: its
//! parent. The values its own frames hold are its children, counted like
//! any object's, so the strategies free continuations and collect their
//! cycles like any other objects.

use std::sync::Arc;

use vali_abi::{Header, Tag, TypeCode};

use vali_bytecode::Arity;

use crate::code::NativeFn;
use crate::cx::{Cx, get, put};
use crate::handle::Error;
use crate::rust_objects::RustObject;
use crate::stack::Segment;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::{Catch, ENTRY, UNDERFLOW, position_base, slot};

/// What a continuation holds.
pub(crate) struct Sealed {
    /// The segments its own frames are in, kept alive.
    #[allow(dead_code)]
    memory: Vec<Arc<Segment>>,
    /// Its first frame, and the position word of a callee returning to it,
    /// with `UNDERFLOW`; `ENTRY` if it returns to the host.
    pub(crate) fp: *mut u64,
    pub(crate) position: u64,
    /// How many frames from `fp` out are its own. The rest are its parent's.
    pub(crate) frames: usize,
    /// The continuation that holds its outermost frame's caller, or `#f`.
    pub(crate) parent: Value,
    /// The `%catch`es waiting when it was captured. A catch whose frame it
    /// owns has `#f` for its record, which stands for the continuation
    /// itself, so it never refers to itself.
    pub(crate) catches: Vec<Catch>,
}

// SAFETY: the frame pointer points into segments it keeps alive, which
// move between threads only with their isolate.
unsafe impl Send for Sealed {}

impl RustObject for Sealed {
    const NAME: &'static str = "continuation";
}

impl Sealed {
    /// Calls `f` with each value the continuation holds: its own frames'
    /// slots, its parent, and its catches' handlers and records.
    ///
    /// # Safety
    ///
    /// The continuation's segments must hold its frames as sealed.
    pub(crate) unsafe fn for_each_value(&self, mut f: impl FnMut(Value)) {
        let mut fp = self.fp;
        let mut own = position_base(self.position);
        for i in 0..self.frames {
            // SAFETY: passed on to the caller: each frame keeps its own
            // slots, and its header links it to its caller.
            unsafe {
                for j in 0..own {
                    f(slot(fp, j));
                }
                if i + 1 < self.frames {
                    // A frame's own slots are those below its callee's
                    // header, at the base the callee's position word holds.
                    own = position_base(*fp.add(2));
                    fp = *fp as *mut u64;
                }
            }
        }
        f(self.parent);
        for catch in &self.catches {
            f(catch.handler);
            f(catch.record);
        }
    }
}

impl<S: Strategy> Cx<S> {
    /// The record of `k`, if it is a continuation that has not been freed.
    pub(crate) fn sealed<'a>(k: Value) -> Option<&'a Sealed> {
        if k.tag() != Tag::Other {
            return None;
        }
        let t = TypeCode::Continuation;
        let header = Self::LAYOUT.header(t)?;
        // SAFETY: a valid value tagged "other" is a live object starting
        // with its header; a continuation's second field points to its
        // record, which lives as long as it does, or is null once freeing
        // has dropped the record (`Cx::retire`).
        unsafe {
            if Header(get(k, header)).type_code() != Some(t) {
                return None;
            }
            (get(k, Self::LAYOUT.field(t, 1)) as *const Sealed).as_ref()
        }
    }

    /// Makes `k` the continuation below the live stack, counting it in and
    /// the old one out under precise counting.
    pub(crate) fn set_below(&mut self, k: Value) {
        if S::PRECISE {
            S::retain(self.heap_mut(), k);
            let old = self.task.below;
            S::release(self.heap_mut(), old);
        }
        self.task.below = k;
    }

    /// Seals the stack into a new continuation that returns to the frame
    /// at `fp` as a callee with position word `position` would.
    ///
    /// If `owned`, `fp` and its callers out to the outermost live frame are
    /// live, and become the continuation's: every segment up to the one
    /// `fp` is in, except the last `live` the stack keeps, and the stack
    /// goes on below `split` in that one. Otherwise `fp` is sealed already,
    /// or `position` is `ENTRY`, and nothing is sealed.
    ///
    /// # Safety
    ///
    /// `fp` must be a frame at a call, and when `owned`, every frame from it
    /// out must hold valid values in its own slots.
    pub(crate) unsafe fn capture(
        &mut self,
        fp: *mut u64,
        position: u64,
        owned: bool,
        split: *mut u64,
        live: usize,
    ) -> Result<Value, Error> {
        let t = TypeCode::Continuation;
        // Allocated first, so failing leaves the stack as it was.
        let k = self.new_object(t, 2, 0)?;
        let mut frames = 0;
        let mut memory = Vec::new();
        let parent;
        if owned {
            let mut f = fp;
            let mut own = position_base(position);
            loop {
                frames += 1;
                // The frames' values become the continuation's. Precise
                // counting counted them on the stack, which no longer holds
                // them; the others count them in.
                if !S::PRECISE {
                    for i in 0..own {
                        // SAFETY: passed on to the caller.
                        S::retain(self.heap_mut(), unsafe { slot(f, i) });
                    }
                }
                // SAFETY: as above: frames link to their callers.
                let p = unsafe { *f.add(2) };
                if p == ENTRY || p & UNDERFLOW != 0 {
                    break;
                }
                own = position_base(p);
                f = unsafe { *f as *mut u64 };
            }
            let kept: Vec<_> = self
                .task
                .stack
                .drain(self.task.stack.len() - live..)
                .collect();
            let last = self.task.stack.pop().expect("a segment holds the frames");
            for view in self.task.stack.drain(..) {
                self.task.stack_words -= view.words();
                memory.push(view.memory());
            }
            memory.push(last.memory());
            self.task.stack.push(last.below(split));
            self.task.stack.extend(kept);
            // The continuation takes over the stack's reference to what
            // lies below it.
            parent = std::mem::replace(&mut self.task.below, Value::FALSE);
            if !S::PRECISE {
                S::retain(self.heap_mut(), parent);
            }
            // The waiting catches' frames are sealed now.
            for i in 0..self.task.catches.len() {
                if self.task.catches[i].record == Value::FALSE {
                    self.task.catches[i].record = k;
                    if S::PRECISE {
                        S::retain(self.heap_mut(), k);
                    }
                }
            }
        } else if position == ENTRY {
            parent = Value::FALSE;
        } else {
            parent = self.task.below;
            S::retain(self.heap_mut(), parent);
        }
        let mut catches = Vec::with_capacity(self.task.catches.len());
        for i in 0..self.task.catches.len() {
            let mut catch = self.task.catches[i];
            if catch.record == k {
                catch.record = Value::FALSE;
            }
            S::retain(self.heap_mut(), catch.handler);
            S::retain(self.heap_mut(), catch.record);
            catches.push(catch);
        }
        if owned {
            self.set_below(k);
        }
        let sealed = Box::new(Sealed {
            memory,
            fp,
            position,
            frames,
            parent,
            catches,
        });
        let pointer = &*sealed as *const Sealed;
        let index = self.objects.insert(sealed);
        // SAFETY: just allocated with both fields.
        unsafe {
            put(k, Self::LAYOUT.field(t, 0), index as u64);
            put(k, Self::LAYOUT.field(t, 1), pointer as u64);
        }
        Ok(k)
    }

    /// Makes `k`'s catches the waiting ones, and the continuation that
    /// holds its first frame the one below the stack, which the caller has
    /// emptied. Returns where `k` returns to: a sealed frame, and the
    /// position word of a callee returning there, or `None` for the host.
    pub(crate) fn reinstate(&mut self, k: Value) -> Option<(*mut u64, u64)> {
        let sealed = Self::sealed(k).expect("a continuation");
        while let Some(catch) = self.task.catches.pop() {
            if S::PRECISE {
                S::release(self.heap_mut(), catch.handler);
                S::release(self.heap_mut(), catch.record);
            }
        }
        for catch in &sealed.catches {
            let mut catch = *catch;
            if catch.record == Value::FALSE {
                catch.record = k;
            }
            if S::PRECISE {
                S::retain(self.heap_mut(), catch.handler);
                S::retain(self.heap_mut(), catch.record);
            }
            self.task.catches.push(catch);
        }
        if sealed.position == ENTRY {
            self.set_below(Value::FALSE);
            return None;
        }
        self.set_below(if sealed.frames > 0 { k } else { sealed.parent });
        Some((sealed.fp, sealed.position))
    }
}

/// The dynamic state's registers: what `dynamic` holds, by index.
pub(crate) const WINDERS: usize = 0;
pub(crate) const HANDLERS: usize = 1;
pub(crate) const PARAMETERIZATION: usize = 2;

impl<S: Strategy> Cx<S> {
    /// Sets register `i` of the dynamic state, counting under precise
    /// counting, which counts what the VM holds.
    pub(crate) fn set_dynamic(&mut self, i: usize, v: Value) {
        if S::PRECISE {
            S::retain(self.heap_mut(), v);
            let old = self.task.dynamic[i];
            S::release(self.heap_mut(), old);
        }
        self.task.dynamic[i] = v;
    }

    /// Empties the dynamic state, as every host call starts and ends.
    pub(crate) fn reset_dynamic(&mut self) {
        for i in 0..self.task.dynamic.len() {
            self.set_dynamic(i, Value::EMPTY_LIST);
        }
    }
}

/// The dynamic state's registers as Scheme procedures, for (scheme base):
/// `(%winders)`, `(%set-winders! list)` and so on.
pub(crate) fn dynamic_table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    fn get<S: Strategy, const I: usize>(cx: &mut Cx<S>, _: &[Value]) -> Result<Value, Error> {
        Ok(cx.task.dynamic[I])
    }
    fn set<S: Strategy, const I: usize>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
        cx.set_dynamic(I, args[0]);
        Ok(Value::UNSPECIFIED)
    }
    vec![
        ("%winders", Arity::exactly(0), get::<S, WINDERS>),
        ("%set-winders!", Arity::exactly(1), set::<S, WINDERS>),
        ("%handlers", Arity::exactly(0), get::<S, HANDLERS>),
        ("%set-handlers!", Arity::exactly(1), set::<S, HANDLERS>),
        (
            "%parameterization",
            Arity::exactly(0),
            get::<S, PARAMETERIZATION>,
        ),
        (
            "%set-parameterization!",
            Arity::exactly(1),
            set::<S, PARAMETERIZATION>,
        ),
        ("%set-raise-handler!", Arity::exactly(1), set_raise_handler),
        ("%exit", Arity::exactly(1), exit),
    ]
}

/// `(%exit obj)`: ends the run, which no `%catch` stops, with the exit code
/// `obj` stands for (R7RS 6.14): 0 for `#t`, 1 for `#f` or anything else
/// that is not a small exact integer. (vali exit) runs the after thunks
/// first.
fn exit<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let code = match args[0].as_boolean() {
        Some(true) => 0,
        Some(false) => 1,
        None => args[0]
            .as_fixnum()
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(1),
    };
    Err(Error::Exit(code))
}

/// `(%set-raise-handler! raise)`: makes `raise`, a Scheme procedure of one
/// argument, what an error calls where it happens while a handler is
/// installed.
fn set_raise_handler<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let raise = args[0];
    // SAFETY: a valid value.
    let ok = matches!(unsafe { cx.procedure_code(raise) }, Ok(code)
        if code.is_scheme() && code.arity.accepts(1));
    if !ok {
        return Err(crate::vm::wrong_type(
            cx,
            "%set-raise-handler!",
            "a Scheme procedure of one argument",
            raise,
        ));
    }
    // SAFETY: as above.
    cx.raise_hook = Some(unsafe { cx.root(raise) });
    Ok(Value::UNSPECIFIED)
}
