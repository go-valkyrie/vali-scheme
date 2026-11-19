//! Loaded code: what a closure's first word points to (ADR 0002 #10).
//!
//! A [`Code`] describes one procedure: its name, its arity, and either its
//! bytecode or a Rust function. Bytecode descriptors live in a [`Loaded`]
//! module, outside every heap (ADR 0008 #6); each isolate that loads the
//! module keeps its own [`Instance`], with its constants and variables.
//! A library's functions are converted when first called, and until then
//! their descriptors are [`Kind::Lazy`] (ADR 0017 #2); that change, and the
//! entry point the JIT will add, are what a descriptor's interior mutability
//! is for.

use std::cell::{OnceCell, UnsafeCell};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use vali_bytecode::{Arity, Constant, Converter, Export, Function, Module, Shape, Variable};

use crate::cx::Cx;
use crate::debug::{DebugFunction, DebugLayout, DebugStr};
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;

/// A procedure implemented in Rust. It receives raw values, valid until the
/// next allocation.
pub type NativeFn<S> = fn(&mut Cx<S>, &[Value]) -> Result<Value, Error>;

/// One procedure's code. C layout, so that its first field, which
/// debuggers read, is at its start.
#[repr(C)]
pub struct Code<S: Strategy> {
    /// What Vali's debugger extensions read (`debug`); it points into
    /// `name` and into the module that owns this descriptor. A function not
    /// converted yet has no frame size and no line table (0 and null) until
    /// its first call fills them in (`Code::converted`); a debugger sees
    /// the function as one that has not run.
    pub(crate) debug: UnsafeCell<DebugFunction>,
    pub(crate) name: Box<str>,
    pub(crate) arity: Arity,
    /// What the procedure is; a [`Kind::Lazy`] becomes a
    /// [`Kind::Bytecode`] on its first call and never changes again.
    /// Read it with [`Code::kind`].
    kind: UnsafeCell<Kind<S>>,
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
    /// A function of a module that converts its functions as they are first
    /// called, not called yet: calling it converts it, and makes this
    /// [`Kind::Bytecode`] (ADR 0017 #2).
    Lazy {
        /// The module the function belongs to.
        module: *const Loaded<S>,
        /// Its index in its module.
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
// descriptor, which is kept alive by every isolate that loaded it.
unsafe impl<S: Strategy> Send for Code<S> {}
// SAFETY: `kind` and `debug` change once, as the function is first called
// (`Code::converted`), by the one thread that runs the isolate; a module is
// not shared between isolates yet (ADR 0008 #6), and one that is must keep
// that change to one thread or make it atomic. The other mutable field is
// an atomic.
unsafe impl<S: Strategy> Sync for Code<S> {}

impl<S: Strategy> fmt::Debug for Code<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind() {
            Kind::Bytecode { .. } => "bytecode",
            Kind::Lazy { .. } => "lazy",
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
    /// This strategy's layout, as the debugger extensions read it.
    pub(crate) const DEBUG_LAYOUT: DebugLayout = DebugLayout::of(Cx::<S>::LAYOUT);

    /// A descriptor with no Scheme frames: for a procedure implemented in
    /// Rust, or one the VM handles itself.
    fn without_frames(name: &str, arity: Arity, kind: Kind<S>) -> Code<S> {
        let name: Box<str> = name.into();
        Code {
            // The name's bytes stay where they are as the box moves.
            debug: UnsafeCell::new(DebugFunction::native(&Self::DEBUG_LAYOUT, &name)),
            name,
            arity,
            kind: UnsafeCell::new(kind),
            calls: AtomicU32::new(0),
            stub: crate::trampoline::resume(),
        }
    }

    /// What the procedure is now.
    ///
    /// The reference must not be kept across anything that may convert this
    /// function, which a call does: copy out what is needed first.
    #[inline(always)]
    pub(crate) fn kind(&self) -> &Kind<S> {
        // SAFETY: `kind` changes only in `converted`, which no reference
        // from here outlives (see above).
        unsafe { &*self.kind.get() }
    }

    /// Whether the procedure is a Scheme function, converted or not.
    pub(crate) fn is_scheme(&self) -> bool {
        matches!(self.kind(), Kind::Bytecode { .. } | Kind::Lazy { .. })
    }

    /// Makes this a bytecode function, with `function`'s code, which
    /// `module` owns and keeps where it is: its first call has converted it
    /// (`Loaded::convert`). Its frame size and line table become what a
    /// debugger reads.
    ///
    /// # Safety
    ///
    /// No reference from [`Code::kind`] may be live, and this must be a
    /// [`Kind::Lazy`] of `module`.
    pub(crate) unsafe fn converted(
        &self,
        module: *const Loaded<S>,
        index: u32,
        function: &Function,
    ) {
        // SAFETY: passed on to the caller.
        unsafe {
            (*self.debug.get()).slots = u32::from(function.slots);
            (*self.debug.get()).positions = function.positions.as_ptr();
            (*self.debug.get()).positions_len = function.positions.len();
            *self.kind.get() = Kind::Bytecode {
                module,
                code: function.code.as_ptr(),
                slots: function.slots,
                index,
            };
        }
    }

    /// A descriptor for a Rust procedure.
    pub(crate) fn native(name: &str, arity: Arity, f: NativeFn<S>) -> Code<S> {
        Code::without_frames(name, arity, Kind::Native(f))
    }

    /// A descriptor for a function from a native library.
    pub(crate) fn bridge(f: std::sync::Arc<crate::bridge::NativeFunction>) -> Code<S> {
        let (name, arity) = (f.name().to_owned(), f.arity());
        Code::without_frames(&name, arity, Kind::Bridge(f))
    }

    /// `%catch`, `%call/cc` and `%continue`: procedures the VM handles
    /// itself, since they work on the stack.
    pub(crate) fn control() -> [Code<S>; 3] {
        let code =
            |name: &str, arity, kind| Code::without_frames(name, Arity::exactly(arity), kind);
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
///
/// It keeps what running the module needs and nothing it has been turned
/// into: the code that runs (under precise counting, the code the ownership
/// pass rewrote, not the code given), and not the constants, which load
/// builds into the isolate's pool or heap and returns to its caller to drop.
///
/// Sharing a `Loaded` between isolates (ADR 0008 #6) while each builds its
/// own pool (ADR 0020) would need every isolate to have the constants'
/// trees, so they would have to move into the shared part then, and only
/// each isolate's built copy would stay per isolate. Until something
/// shares one, nothing keeps them.
///
/// A module made lazy (`Loaded::new` with a converter) has the code of its
/// body only: each other function gets its own as it is first called
/// (`Loaded::convert`).
pub(crate) struct Loaded<S: Strategy> {
    /// The function that runs the module's body.
    pub(crate) body: u32,
    /// Its variable table, which linking reads and an unbound variable's
    /// error names.
    pub(crate) variables: Box<[Variable]>,
    exports: Box<[Export]>,
    /// The module's source names, which backtraces name.
    pub(crate) sources: Box<[Box<str>]>,
    /// The module's digest (`Module::fingerprint`), for a library, which is
    /// what `Cx::is_loaded_from` compares: nothing asks about a program's
    /// body or an evaluated module, so they take none.
    fingerprint: Option<[u8; 32]>,
    /// Its functions as they run: rewritten under precise counting
    /// (`vali_bytecode::ownership`), as given otherwise. Each carries the
    /// positions and scopes of its own code, which backtraces read. Empty
    /// until a lazy module's function is first called; set once.
    code: Box<[OnceCell<Function>]>,
    pub(crate) functions: Box<[Code<S>]>,
    /// What converts the functions of a lazy module.
    lazy: Option<Lazy>,
    /// The module's source names, as the debugger extensions read them;
    /// they point into `sources`. Only debuggers read it, through the
    /// pointers each function's `DebugFunction` holds.
    #[expect(dead_code, reason = "kept alive for debuggers, which read it in place")]
    debug_sources: Box<[DebugStr]>,
}

/// What a lazy module needs to convert a function.
struct Lazy {
    converter: Converter,
    /// The functions with no code, but their arity and free variables, for
    /// checking the code of each as it is converted.
    functions: Box<[Function]>,
    /// How many constants the module has, which its code may name.
    constants: usize,
}

// SAFETY: the cells of `code` are filled by the one thread that runs the
// isolate (see `Code`'s `Sync`), and read only after; the rest is immutable.
unsafe impl<S: Strategy> Sync for Loaded<S> {}

impl<S: Strategy> Loaded<S> {
    /// Function `index` as it runs: rewritten under precise counting.
    /// `None` if there is none, or it is a lazy module's and has not been
    /// called.
    pub(crate) fn function(&self, index: usize) -> Option<&Function> {
        self.code.get(index)?.get()
    }

    /// How many functions of the module have been converted: all of an
    /// eager module's, and a lazy module's that have been called, and its
    /// body.
    pub(crate) fn converted(&self) -> usize {
        self.code.iter().filter(|c| c.get().is_some()).count()
    }

    /// The index of the variable the module exports as `name`.
    pub(crate) fn export(&self, name: &str) -> Option<u16> {
        self.exports
            .iter()
            .find(|e| &*e.name == name)
            .map(|e| e.variable)
    }

    /// Whether `module` is the one this was loaded from.
    pub(crate) fn is_loaded_from(&self, module: &Module) -> bool {
        self.fingerprint == Some(module.fingerprint())
    }

    /// Converts function `index`, which `Kind::Lazy` says has not been, to
    /// bytecode, and makes its descriptor a bytecode function's. Under
    /// precise counting it places the ownership operations in the code, as
    /// loading an eager module does. Returns the bytes of Rust memory the
    /// code takes, for the caller to charge to the heap.
    ///
    /// # Safety
    ///
    /// No reference from `Code::kind` of this function may be live.
    pub(crate) unsafe fn convert(&self, index: usize) -> Result<usize, String> {
        let lazy = self
            .lazy
            .as_ref()
            .ok_or("a function of an eager module is not converted")?;
        let shape = Shape {
            constants: lazy.constants,
            variables: &self.variables,
            functions: &lazy.functions,
            sources: self.sources.len(),
        };
        let given = lazy
            .converter
            .convert(&shape, index as u32)
            .map_err(|e| e.to_string())?;
        let function = if S::PRECISE {
            let rewritten = vali_bytecode::ownership::insert_one(&given)
                .map_err(|e| format!("placing ownership operations: {e}"))?;
            // The code that runs is validated too, not only the code given.
            vali_bytecode::check_rewritten(&shape, index as u32, &given, &rewritten)
                .map_err(|e| format!("after placing ownership operations: {e}"))?;
            rewritten
        } else {
            given
        };
        let bytes = 4 * function.code.len() + std::mem::size_of_val(&*function.positions) + 128;
        let cell = &self.code[index];
        if cell.set(function).is_err() {
            return Err("the function is converted already".into());
        }
        let function = cell.get().expect("just set");
        // SAFETY: passed on to the caller; the code stays where it is in
        // the cell, which lives as long as this module.
        unsafe { self.functions[index].converted(self, index as u32, function) };
        Ok(bytes)
    }

    /// Builds the descriptors for `module`, which must be valid (for a lazy
    /// one, as far as `validate_lazy` checks it), placing ownership
    /// operations in its code under precise counting. A module with a
    /// `converter` has its functions but the body converted as they are
    /// first called. Returns the descriptors with the module's constants,
    /// which only the caller needs, to build. Only a `library` is given a
    /// digest.
    pub(crate) fn new(
        module: Module,
        converter: Option<Converter>,
        transient: bool,
        library: bool,
    ) -> Result<(Arc<Loaded<S>>, Vec<Constant>), String> {
        let lazy = converter.is_some();
        let body = module.body as usize;
        // Under precise counting the code that runs is rewritten: all of an
        // eager module's, the body of a lazy one.
        let owned: Option<Vec<Function>> = if S::PRECISE && !lazy {
            let functions = vali_bytecode::ownership::insert(&module)
                .map_err(|e| format!("placing ownership operations: {e}"))?;
            // The code that runs is validated too, not only the code given.
            vali_bytecode::validate_rewritten(&module, &functions)
                .map_err(|e| format!("after placing ownership operations: {e}"))?;
            Some(functions)
        } else {
            None
        };
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
        let fingerprint = library.then(|| module.fingerprint());
        let constants_len = module.constants.len();
        let Module {
            constants,
            variables,
            exports,
            mut functions,
            sources,
            ..
        } = module;
        let mut cells: Vec<OnceCell<Function>> = Vec::new();
        let lazy = match converter {
            Some(converter) => {
                let stub = header(&functions[body]);
                let given = std::mem::replace(&mut functions[body], stub);
                let ready = if S::PRECISE {
                    let rewritten = vali_bytecode::ownership::insert_one(&given)
                        .map_err(|e| format!("placing ownership operations: {e}"))?;
                    let shape = Shape {
                        constants: constants_len,
                        variables: &variables,
                        functions: &functions,
                        sources: sources.len(),
                    };
                    vali_bytecode::check_rewritten(&shape, body as u32, &given, &rewritten)
                        .map_err(|e| format!("after placing ownership operations: {e}"))?;
                    rewritten
                } else {
                    given
                };
                cells.resize_with(functions.len(), OnceCell::new);
                let _ = cells[body].set(ready);
                Some(Lazy {
                    converter,
                    functions: functions.into(),
                    constants: constants_len,
                })
            }
            None => {
                // Under precise counting the given functions go here, their
                // code, positions, scopes and inlined copies with them: the
                // rewritten ones carry their own.
                cells.extend(owned.unwrap_or(functions).into_iter().map(OnceCell::from));
                None
            }
        };
        let code: Box<[OnceCell<Function>]> = cells.into();
        // Named marker stubs only with native frames: without them, every
        // frame returns to the shared stub, and nothing is registered with
        // debuggers (ADR 0004 #6). A module freed once nothing refers to it
        // gets none either: stubs are registered for the process's life, so
        // its frames cannot be named in a debugger.
        let stubs = if cfg!(feature = "native-frames") && !transient {
            crate::stubs::Stubs::new(&names.iter().map(|n| &**n).collect::<Vec<_>>())
        } else {
            None
        };
        let sources: Box<[Box<str>]> = sources.into();
        let debug_sources: Box<[DebugStr]> = sources.iter().map(|s| DebugStr::of(s)).collect();
        let loaded = Arc::new_cyclic(|this| {
            let this = this.as_ptr();
            let functions = code
                .iter()
                .zip(names)
                .enumerate()
                .map(|(i, (cell, name))| {
                    // The name, code, positions and sources stay where they
                    // are as their owners move into the `Loaded`.
                    let mut debug = DebugFunction {
                        version: crate::debug::DEBUG_VERSION,
                        slots: 0,
                        layout: Self::debug_layout(),
                        name: DebugStr::of(&name),
                        positions: std::ptr::null(),
                        positions_len: 0,
                        sources: debug_sources.as_ptr(),
                        sources_len: debug_sources.len(),
                    };
                    let (arity, kind) = match cell.get() {
                        Some(f) => {
                            debug.slots = u32::from(f.slots);
                            debug.positions = f.positions.as_ptr();
                            debug.positions_len = f.positions.len();
                            (
                                f.arity,
                                Kind::Bytecode {
                                    module: this,
                                    code: f.code.as_ptr(),
                                    slots: f.slots,
                                    index: i as u32,
                                },
                            )
                        }
                        None => {
                            let lazy = lazy.as_ref().expect("only a lazy module has gaps");
                            (
                                lazy.functions[i].arity,
                                Kind::Lazy {
                                    module: this,
                                    index: i as u32,
                                },
                            )
                        }
                    };
                    Code {
                        debug: UnsafeCell::new(debug),
                        name,
                        arity,
                        kind: UnsafeCell::new(kind),
                        calls: AtomicU32::new(0),
                        stub: stubs
                            .as_ref()
                            .map_or_else(crate::trampoline::resume, |s| s.resume(i)),
                    }
                })
                .collect();
            Loaded {
                body: body as u32,
                variables: variables.into(),
                exports: exports.into(),
                sources,
                fingerprint,
                code,
                functions,
                lazy,
                debug_sources,
            }
        });
        Ok((loaded, constants))
    }

    /// The strategy's layout, for the debugger extensions.
    fn debug_layout() -> &'static DebugLayout {
        &Code::<S>::DEBUG_LAYOUT
    }
}

/// `function` with no code: what checking the others needs of it.
fn header(function: &Function) -> Function {
    Function {
        name: None,
        arity: function.arity,
        slots: 0,
        free: function.free,
        code: Vec::new(),
        positions: Vec::new(),
        inlined: Vec::new(),
        scopes: Vec::new(),
    }
}

/// A pointer to a variable's cell (ADR 0005 #4).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct Cell(pub(crate) *mut Value);

// SAFETY: a cell pointer points into storage its own isolate owns, which
// moves between threads only whole and is used by one at a time (ADR 0008).
unsafe impl Send for Cell {}

/// One isolate's instance of a loaded module: its constants, the cells of
/// the variables it defines, and a pointer to the cell of each entry of its
/// variable table.
pub(crate) struct Instance<S: Strategy> {
    pub(crate) loaded: Arc<Loaded<S>>,
    /// Where it is in the isolate's instances.
    pub(crate) index: usize,
    /// The module's object, if it is freed once nothing refers to it
    /// (`Cx::load_transient`), else `#f`. Closures of its functions keep
    /// it alive; it holds the constants and the cells' values.
    pub(crate) object: Value,
    pub(crate) constants: Vec<Value>,
    /// The closure direct calls give each function, made the first time
    /// one calls it and `#f` until then. Only functions with no free
    /// variables are called directly.
    pub(crate) statics: Box<[Value]>,
    /// Boxed so the cells stay put as instances are added. Code reaches
    /// them only through `variables`.
    pub(crate) cells: Box<[std::cell::UnsafeCell<Value>]>,
    pub(crate) variables: Box<[Cell]>,
}

impl<S: Strategy> Instance<S> {
    /// Calls `f` with each value the instance holds: what its module's
    /// object, if any, keeps alive.
    pub(crate) fn for_each_value(&self, f: &mut impl FnMut(Value)) {
        self.constants.iter().for_each(|&v| f(v));
        self.statics.iter().for_each(|&v| f(v));
        // SAFETY: the cells belong to this instance and hold valid values.
        self.cells.iter().for_each(|c| f(unsafe { *c.get() }));
    }
}

/// The modules loaded into an isolate. A module freed once nothing refers
/// to it leaves its slot to the next; its instance waits in the graveyard
/// until it is safe to drop it (`Instances::empty_graveyard`).
pub(crate) struct Instances<S: Strategy> {
    slots: Vec<Option<Box<Instance<S>>>>,
    spare: Vec<usize>,
    /// Which instance each loaded module has, by its address.
    by_loaded: std::collections::HashMap<usize, usize>,
    /// Retired instances, kept as long as a closure being freed could
    /// still be described, and as long as the interpreter could have
    /// registers pointing into one.
    #[allow(clippy::vec_box)] // their addresses must stay put
    graveyard: Vec<Box<Instance<S>>>,
}

impl<S: Strategy> Instances<S> {
    pub(crate) fn new() -> Self {
        Instances {
            slots: Vec::new(),
            spare: Vec::new(),
            by_loaded: std::collections::HashMap::new(),
            graveyard: Vec::new(),
        }
    }

    /// Adds the instance `make` builds, given its index.
    pub(crate) fn push(&mut self, make: impl FnOnce(usize) -> Instance<S>) -> usize {
        let index = self.spare.pop().unwrap_or(self.slots.len());
        let instance = Box::new(make(index));
        self.by_loaded
            .insert(Arc::as_ptr(&instance.loaded) as usize, index);
        if index == self.slots.len() {
            self.slots.push(Some(instance));
        } else {
            self.slots[index] = Some(instance);
        }
        index
    }

    /// The index of the instance of the loaded module at `loaded`.
    pub(crate) fn of_loaded(&self, loaded: usize) -> usize {
        self.by_loaded[&loaded]
    }

    /// How many modules are loaded now.
    pub(crate) fn live(&self) -> usize {
        self.slots.len() - self.spare.len()
    }

    /// How many slots there are, live or free.
    pub(crate) fn slots(&self) -> usize {
        self.slots.len()
    }

    /// The module at `index` has nothing referring to it: its instance
    /// goes to the graveyard, and its slot is free.
    pub(crate) fn retire(&mut self, index: usize) {
        if let Some(instance) = self.slots[index].take() {
            self.by_loaded
                .remove(&(Arc::as_ptr(&instance.loaded) as usize));
            self.graveyard.push(instance);
            self.spare.push(index);
        }
    }

    /// Drops the retired instances. Not while an epoch is running, or
    /// while the interpreter could be running code of one.
    pub(crate) fn empty_graveyard(&mut self) {
        self.graveyard.clear();
    }
}

impl<S: Strategy> std::ops::Index<usize> for Instances<S> {
    type Output = Instance<S>;

    fn index(&self, index: usize) -> &Instance<S> {
        self.slots[index].as_deref().expect("a loaded module")
    }
}

impl<S: Strategy> std::ops::IndexMut<usize> for Instances<S> {
    fn index_mut(&mut self, index: usize) -> &mut Instance<S> {
        self.slots[index].as_deref_mut().expect("a loaded module")
    }
}

/// A module loaded into an isolate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) usize);
