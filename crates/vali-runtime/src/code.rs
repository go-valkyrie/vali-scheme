//! Loaded code: what a closure's first word points to (ADR 0002 #10).
//!
//! A [`Code`] describes one procedure: its name, its arity, and either its
//! bytecode or a Rust function. Bytecode descriptors live in a [`Loaded`]
//! module, shared and immutable once built, outside every heap (ADR 0008
//! #6); each isolate that loads the module keeps its own [`Instance`], with
//! its constants and variables. The JIT will add an entry point here that
//! tier-up replaces.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use vali_bytecode::{Arity, Module};

use crate::cx::Cx;
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;

/// A procedure implemented in Rust. It receives raw values, valid until the
/// next allocation.
pub type NativeFn<S> = fn(&mut Cx<S>, &[Value]) -> Result<Value, Error>;

/// One procedure's code.
pub struct Code<S: Strategy> {
    pub(crate) name: Box<str>,
    pub(crate) arity: Arity,
    pub(crate) kind: Kind<S>,
    /// Calls so far, for tier-up. Approximate under contention.
    pub(crate) calls: AtomicU32,
    /// The return address of frames that call out of this function's
    /// frames: a point in its marker stub, which debuggers name after it
    /// (ADR 0004 #6).
    pub(crate) stub: u64,
}

pub(crate) enum Kind<S: Strategy> {
    Bytecode {
        /// The module the function belongs to.
        module: *const Loaded<S>,
        /// Its instructions, inside the module.
        code: *const u32,
        /// Its frame size.
        slots: u16,
        /// Its index in its module, for its positions.
        index: u32,
    },
    Native(NativeFn<S>),
    /// A Rust function from a native library (ADR 0016), which works with
    /// any strategy.
    Bridge(std::sync::Arc<crate::bridge::NativeFunction>),
    /// `%catch`, which the VM handles itself: it calls a thunk, and calls a
    /// handler in its place if anything is raised inside (ADR 0004).
    Catch,
    /// `%call/cc`, which the VM handles itself: it seals the stack into a
    /// continuation and calls a procedure with it (ADR 0004).
    Capture,
    /// `%continue`, which returns a value to a continuation.
    Resume,
}

// SAFETY: the raw pointers point into the `Loaded` module that owns this
// descriptor, which is immutable once built and kept alive by every isolate
// that loaded it.
unsafe impl<S: Strategy> Send for Code<S> {}
// SAFETY: as above; the only mutable field is an atomic.
unsafe impl<S: Strategy> Sync for Code<S> {}

impl<S: Strategy> fmt::Debug for Code<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            Kind::Bytecode { .. } => "bytecode",
            Kind::Native(_) => "native",
            Kind::Bridge(_) => "bridge",
            Kind::Catch => "catch",
            Kind::Capture => "capture",
            Kind::Resume => "resume",
        };
        f.debug_struct("Code")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .field("kind", &kind)
            .finish()
    }
}

impl<S: Strategy> Code<S> {
    /// A descriptor for a Rust procedure.
    pub(crate) fn native(name: &str, arity: Arity, f: NativeFn<S>) -> Code<S> {
        Code {
            name: name.into(),
            arity,
            kind: Kind::Native(f),
            calls: AtomicU32::new(0),
            stub: crate::trampoline::resume(),
        }
    }

    /// A descriptor for a function from a native library.
    pub(crate) fn bridge(f: std::sync::Arc<crate::bridge::NativeFunction>) -> Code<S> {
        Code {
            name: f.name().into(),
            arity: f.arity(),
            kind: Kind::Bridge(f),
            calls: AtomicU32::new(0),
            stub: crate::trampoline::resume(),
        }
    }

    /// `%catch`, `%call/cc` and `%continue`: procedures the VM handles
    /// itself, since they work on the stack.
    pub(crate) fn control() -> [Code<S>; 3] {
        let code = |name: &str, arity, kind| Code {
            name: name.into(),
            arity: Arity::exactly(arity),
            kind,
            calls: AtomicU32::new(0),
            stub: crate::trampoline::resume(),
        };
        [
            code("%catch", 2, Kind::Catch),
            code("%call/cc", 1, Kind::Capture),
            code("%continue", 2, Kind::Resume),
        ]
    }

    /// The procedure's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A validated module, with a descriptor for each of its functions.
pub(crate) struct Loaded<S: Strategy> {
    pub(crate) module: Module,
    pub(crate) functions: Box<[Code<S>]>,
}

impl<S: Strategy> Loaded<S> {
    /// Builds the descriptors for `module`, which must be valid.
    pub(crate) fn new(module: Module) -> Arc<Loaded<S>> {
        let names: Vec<Box<str>> = module
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| {
                f.name
                    .clone()
                    .unwrap_or_else(|| format!("{}#{i}", module.name).into())
            })
            .collect();
        let stubs = crate::stubs::Stubs::new(&names.iter().map(|n| &**n).collect::<Vec<_>>());
        Arc::new_cyclic(|this| {
            let this = this.as_ptr();
            let functions = module
                .functions
                .iter()
                .zip(names)
                .enumerate()
                .map(|(i, (f, name))| Code {
                    name,
                    arity: f.arity,
                    kind: Kind::Bytecode {
                        module: this,
                        code: f.code.as_ptr(),
                        slots: f.slots,
                        index: i as u32,
                    },
                    calls: AtomicU32::new(0),
                    stub: stubs
                        .as_ref()
                        .map_or_else(crate::trampoline::resume, |s| s.resume(i)),
                })
                .collect();
            Loaded { module, functions }
        })
    }
}

/// A pointer to a variable's cell (ADR 0005 #4).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct Cell(pub(crate) *mut Value);

// SAFETY: a cell pointer points into storage its own isolate owns, which
// moves between threads only whole and is used by one at a time (ADR 0008).
unsafe impl Send for Cell {}

/// One isolate's instance of a loaded module: its constants, created in the
/// isolate's heap, the cells of the variables it defines, and a pointer to
/// the cell of each entry of its variable table.
pub(crate) struct Instance<S: Strategy> {
    pub(crate) loaded: Arc<Loaded<S>>,
    pub(crate) constants: Vec<Value>,
    /// Boxed so the cells stay put as instances are added. Code reaches
    /// them only through `variables`.
    #[expect(dead_code, reason = "owns the cells that `variables` points into")]
    pub(crate) cells: Box<[std::cell::UnsafeCell<Value>]>,
    pub(crate) variables: Box<[Cell]>,
}

/// A module loaded into an isolate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) usize);
