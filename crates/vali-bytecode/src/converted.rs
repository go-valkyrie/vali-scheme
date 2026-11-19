//! A module converted whole or as its functions are first called.

use std::ops::Deref;

use crate::{LazyModule, Module};

/// What converting a module gives: a [`Module`] whose every function is
/// converted, or a [`LazyModule`] whose functions are converted as they are
/// first called (ADR 0017 #2).
///
/// It dereferences to the module, for what is known without converting
/// anything: its name, variables, exports, sources and digest. A lazy
/// module's functions other than its body have no code there.
#[derive(Clone, Debug, PartialEq)]
pub enum Converted {
    /// Every function is converted.
    Eager(Module),
    /// The functions are converted as they are called.
    Lazy(LazyModule),
}

impl Converted {
    /// The module, with no code for a lazy module's functions but the body.
    pub fn module(&self) -> &Module {
        match self {
            Converted::Eager(module) => module,
            Converted::Lazy(lazy) => &lazy.module,
        }
    }

    /// The module, mutably, for setting its digest.
    pub fn module_mut(&mut self) -> &mut Module {
        match self {
            Converted::Eager(module) => module,
            Converted::Lazy(lazy) => &mut lazy.module,
        }
    }
}

impl Deref for Converted {
    type Target = Module;

    fn deref(&self) -> &Module {
        self.module()
    }
}

impl From<Module> for Converted {
    fn from(module: Module) -> Converted {
        Converted::Eager(module)
    }
}

impl From<LazyModule> for Converted {
    fn from(lazy: LazyModule) -> Converted {
        Converted::Lazy(lazy)
    }
}
