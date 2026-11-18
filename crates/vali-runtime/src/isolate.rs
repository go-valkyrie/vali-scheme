//! Isolates: what a host holds (ADR 0008).
//!
//! An [`Isolate`] is the unit of memory and of sandboxing: a heap, its
//! library instances, its run-time symbols, its limits. One thread at a
//! time uses it. It is `Send`, so a host may move it between threads, or
//! drive it from a future that moves, between uses.
//!
//! A host reaches the isolate's contents through its context, a [`Cx`],
//! which [`Isolate::with`] lends for the length of a closure. Holding the
//! context is what proves the isolate is not running Scheme code
//! elsewhere, which using a [`Handle`] requires (ADR 0003). Rust
//! procedures that Scheme calls are lent the same context, or its
//! strategy-erased view, [`bridge::Context`](crate::bridge::Context).
//!
//! Within the isolate, Scheme code runs as tasks (ADR 0008 #2): each has
//! its own stack and dynamic state, and they share everything else. Today
//! an isolate has one task.

use crate::cx::Cx;
use crate::handle::{Error, Handle};
use crate::page::HeapConfig;
use crate::strategy::Strategy;

/// An isolate: a heap and the libraries loaded into it, used by one
/// thread at a time.
pub struct Isolate<S: Strategy = crate::DefaultStrategy> {
    cx: Cx<S>,
}

impl<S: Strategy> std::fmt::Debug for Isolate<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Isolate").field("cx", &self.cx).finish()
    }
}

impl<S: Strategy> Isolate<S> {
    /// An isolate with a fresh heap set up by `config`, nothing loaded.
    pub fn new(config: &HeapConfig) -> Isolate<S> {
        Isolate {
            cx: Cx::new(config),
        }
    }

    /// Enters the isolate: runs `f` with its context, and returns what `f`
    /// returns. Entering is exclusive, as `&mut self` makes it.
    pub fn with<R>(&mut self, f: impl FnOnce(&mut Cx<S>) -> R) -> R {
        f(&mut self.cx)
    }

    /// Calls `procedure` with `args`, all handles of this isolate, and
    /// returns what it returns: [`Cx::call`] without entering first.
    pub fn call(&mut self, procedure: &Handle, args: &[Handle]) -> Result<Handle, Error> {
        self.cx.call(procedure, args)
    }
}

impl<S: Strategy> From<Cx<S>> for Isolate<S> {
    /// The isolate whose context `cx` is.
    fn from(cx: Cx<S>) -> Isolate<S> {
        Isolate { cx }
    }
}

/// An isolate moves between host threads between uses (ADR 0008 #4).
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Isolate<crate::NoCollection>>();
};
