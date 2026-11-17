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
}

pub(crate) enum Kind<S: Strategy> {
    Bytecode {
        /// The module the function belongs to.
        module: *const Loaded<S>,
        /// Its instructions, inside the module.
        code: *const u32,
        /// Its frame size.
        slots: u16,
    },
    Native(NativeFn<S>),
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
        }
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
        Arc::new_cyclic(|this| {
            let this = this.as_ptr();
            let functions = module
                .functions
                .iter()
                .enumerate()
                .map(|(i, f)| Code {
                    name: f
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{}#{i}", module.name).into()),
                    arity: f.arity,
                    kind: Kind::Bytecode {
                        module: this,
                        code: f.code.as_ptr(),
                        slots: f.slots,
                    },
                    calls: AtomicU32::new(0),
                })
                .collect();
            Loaded { module, functions }
        })
    }
}

/// One isolate's instance of a loaded module: its constants, created in the
/// isolate's heap, and its variables.
pub(crate) struct Instance<S: Strategy> {
    pub(crate) loaded: Arc<Loaded<S>>,
    pub(crate) constants: Vec<Value>,
    pub(crate) variables: Vec<Value>,
}

/// A module loaded into an isolate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) usize);
