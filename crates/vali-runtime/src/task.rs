//! A task: one thread of Scheme execution within an isolate (ADR 0008).
//!
//! An isolate runs its tasks concurrently but not in parallel; they share
//! its heap, its library instances and its symbols. What each needs for
//! itself is here: its stack, the continuation below the stack, the call
//! a Rust procedure left to be made, the `%catch`es in effect and the
//! dynamic state. Today an isolate has exactly one task, the current one,
//! which [`Cx`](crate::Cx) holds; switching tasks will swap it.

use crate::stack::{Segment, View};
use crate::value::Value;
use crate::vm::Catch;

/// A task's own state, apart from its isolate's.
pub(crate) struct Task {
    /// The stack: a chain of segments, the current one last, created on
    /// first use.
    pub(crate) stack: Vec<View>,
    /// The continuation the outermost live frame returns into, when that
    /// frame's caller is sealed: the one whose own frames hold the caller.
    /// `#f` otherwise.
    pub(crate) below: Value,
    /// A segment kept after the stack shrinks, so a call that keeps
    /// crossing a segment boundary does not allocate each time.
    pub(crate) spare: Option<Segment>,
    /// Words allocated to the stack, the spare included.
    pub(crate) stack_words: usize,
    /// A tail call a Rust procedure asked for, made when it returns.
    pub(crate) tail_call: Option<(Value, Vec<Value>)>,
    /// The `%catch`es whose thunks are running, innermost last.
    pub(crate) catches: Vec<Catch>,
    /// The dynamic state (scheme base) keeps: `dynamic-wind`'s winders,
    /// the exception handlers and the parameterization, each a list. Every
    /// host call starts with them empty (`continuations`).
    pub(crate) dynamic: [Value; 3],
}

impl Task {
    /// A task that has not run yet: no stack, empty dynamic state.
    pub(crate) fn new() -> Task {
        Task {
            stack: Vec::new(),
            below: Value::FALSE,
            spare: None,
            stack_words: 0,
            tail_call: None,
            catches: Vec::new(),
            dynamic: [Value::EMPTY_LIST; 3],
        }
    }
}
