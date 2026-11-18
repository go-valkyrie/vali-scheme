//! The tier-0 VM (ADRs 0004 and 0005, `docs/bytecode.md`).
//!
//! Frames live on a stack segment that grows downward. A frame pointer
//! points at the frame's native record: the saved frame pointer, then the
//! return address, then the caller's bytecode position, at increasing
//! addresses. Slot `i` sits `i + 1` words below the frame pointer. A call
//! builds the callee's frame inside the caller's, over the slots the
//! caller reserved, so arguments are never copied.
//!
//! Dispatch is a `match` inside a loop, with the program counter and frame
//! pointer in locals. The validator has already checked every operand, so
//! the loop checks only what instructions must check at run time: types,
//! bounds, arity, and the stack limit.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use std::cell::UnsafeCell;
use vali_abi::{Header, Tag, TypeCode, data};

use vali_bytecode::{
    Arity, Constant, Link as VariableLink, Module, Opcode, field_d, field_e_signed, fields,
    validate,
};

use crate::code::{Cell, Code, Instance, Kind, Loaded, ModuleId, NativeFn};
use crate::cx::{Cx, get, put};
use crate::handle::{Error, Handle, SchemeError};
use crate::page::OutOfMemory;
use crate::stack::{SEGMENT_WORDS, Segment, View};
use crate::strategy::Strategy;
use crate::trampoline::{self, Link};
use crate::value::Value;
use std::ffi::c_void;
use vali_num::Integer;

/// The bytecode position stored in the entry frame's header: returning to
/// it leaves the VM.
pub(crate) const ENTRY: u64 = u64::MAX;

/// Set in a frame's position word when the frame is the first in its
/// segment, so returning from it drops back to the previous segment.
const BOUNDARY: u64 = 1 << 63;

/// Set in the position word of the frame `%catch` calls its thunk in, and
/// in the resume word of a native thunk's call: returning through it
/// removes the innermost catch.
const CATCH: u64 = 1 << 62;

/// Set in a frame's position word when its caller is sealed in a
/// continuation (`continuations`): returning from it copies the caller into
/// the live stack first. Such a frame is the outermost live one, and the
/// first in its segment.
pub(crate) const UNDERFLOW: u64 = 1 << 61;

/// Set in the resume word of the call `%call/cc` makes: once the
/// receiver's frame is set up, the stack from its caller out is sealed.
/// Never stored in a frame.
const CAPTURE: u64 = 1 << 60;

/// Set in the position word of the frame an error calls the raise
/// procedure in: the raise procedure never returns, so returning through
/// it is an error.
const RAISED: u64 = 1 << 59;

/// A `%catch` waiting for its thunk to return: where to call the handler
/// if something is raised first.
#[derive(Clone, Copy)]
pub(crate) struct Catch {
    /// The frame that called `%catch`, and the `base` of that call.
    pub(crate) fp: *mut u64,
    pub(crate) base: usize,
    /// Where that frame resumes.
    pub(crate) resume: u64,
    /// How many stack segments there were, while the frame is live.
    pub(crate) segments: usize,
    pub(crate) handler: Value,
    /// The continuation whose own frames hold the frame, once it is
    /// sealed; `#f` while it is live.
    pub(crate) record: Value,
}

// SAFETY: the frame pointer points into this isolate's own stack, which
// moves between threads only whole (ADR 0008).
unsafe impl Send for Catch {}

/// A frame's position word: where the caller resumes, and the caller's
/// `base`, so the result can be stored even when the frame was moved to a
/// new segment, with the flags `resume` carries. The low three bits stay
/// zero, so the word reads as a fixnum: a header left in a caller's slots
/// after its callee returns is never mistaken for a pointer when an epoch
/// scans them, as the saved frame pointer and the 8-aligned return address
/// beside it aren't either.
fn position(resume: u64, base: usize) -> u64 {
    ((resume & 0xFFFF_FFFF) << 3) | (base as u64) << 35 | (resume & (CATCH | BOUNDARY | UNDERFLOW))
}

/// The `base` a position word records.
pub(crate) fn position_base(position: u64) -> usize {
    ((position >> 35) & 0xFFFF) as usize
}

/// Where the caller resumes, from a position word.
pub(crate) fn position_resume(position: u64) -> u64 {
    (position >> 3) & 0xFFFF_FFFF
}

/// Whether a frame of `words` slots fits below `fp` without crossing
/// `limit`.
fn fits(fp: *mut u64, words: usize, limit: *mut u64) -> bool {
    (fp as usize).saturating_sub(limit as usize) / 8 >= words
}

/// What the trampoline passes to [`body`]. `record` must come first: the
/// trampoline stores its frame record's address there.
#[repr(C)]
struct EnterState<S: Strategy> {
    record: *mut u64,
    cx: *mut Cx<S>,
    entry: *mut u64,
    limit: *mut u64,
    argc: usize,
    result: Option<Result<Value, Error>>,
    panic: Option<Box<dyn std::any::Any + Send>>,
}

/// Runs the VM from inside the trampoline, and restores the trampoline's
/// frame record before returning to it, even if the VM panics.
unsafe extern "C-unwind" fn body<S: Strategy>(state: *mut c_void) {
    // SAFETY: the trampoline passes the `EnterState` `execute` gave it, with
    // `record` filled in.
    let state = unsafe { &mut *(state as *mut EnterState<S>) };
    let record = state.record;
    // SAFETY: `record` is the trampoline's frame record, two words.
    let link = unsafe {
        Link {
            record,
            host_fp: *record,
            host_return: *record.add(1),
            host_cfa: record as u64 + 16,
        }
    };
    // SAFETY: `cx` is the context that called `execute`, borrowed for its
    // duration.
    let cx = unsafe { &mut *state.cx };
    let (entry, limit, argc) = (state.entry, state.limit, state.argc);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `execute` set up the entry frame.
        unsafe { cx.interpret(&link, entry, limit, argc) }
    }));
    // SAFETY: as above; the trampoline pops these when it returns.
    unsafe {
        *record = link.host_fp;
        *record.add(1) = link.host_return;
    }
    match outcome {
        Ok(result) => state.result = Some(result),
        Err(panic) => state.panic = Some(panic),
    }
}

/// Clears the three slots at `base` of the frame at `fp`, where a returned
/// callee's header was: its saved frame pointer, return address and
/// position would otherwise read as fixnums, leaking addresses to the
/// caller's code. The header was never counted, so nothing is released.
///
/// # Safety
///
/// The frame must have the three slots.
#[inline(always)]
unsafe fn clear_header(fp: *mut u64, base: usize) {
    for i in base..base + 3 {
        // SAFETY: passed on to the caller.
        unsafe { *fp.sub(i + 1) = Value::UNSPECIFIED.raw() };
    }
}

#[inline(always)]
pub(crate) unsafe fn slot(fp: *mut u64, i: usize) -> Value {
    // SAFETY: the caller guarantees slot `i` is inside the frame.
    unsafe { Value::from_raw(*fp.sub(i + 1)) }
}

#[inline(always)]
unsafe fn set(fp: *mut u64, i: usize, v: Value) {
    // SAFETY: the caller guarantees slot `i` is inside the frame.
    unsafe { *fp.sub(i + 1) = v.raw() }
}

/// The number a numeric constant stands for, if it is a well-formed one: a
/// ratio of two integers in lowest terms that is not an integer, or a
/// complex number of two reals of the same exactness that is not real.
fn constant_number(constant: &Constant) -> Option<vali_num::Number> {
    use vali_num::{Number, arith};
    let integer = |c: &Constant| match c {
        Constant::Integer(n) => Some(Number::from(*n)),
        Constant::Bignum { negative, limbs } => {
            Some(Number::Integer(Integer::from_limbs(*negative, limbs)))
        }
        _ => None,
    };
    match constant {
        Constant::Ratio(n, d) => {
            let (n, d) = (integer(n)?, integer(d)?);
            let r = arith::div(&n, &d).ok()?;
            let lowest = arith::numerator(&r)
                .ok()
                .is_some_and(|m| arith::num_eq(&m, &n));
            (matches!(r, Number::Rational(_)) && lowest).then_some(r)
        }
        Constant::Complex(re, im) => {
            let part = |c: &Constant| match c {
                Constant::Flonum(x) => Some(Number::Real(*x)),
                Constant::Ratio(..) => constant_number(c),
                _ => integer(c),
            };
            let (re, im) = (part(re)?, part(im)?);
            if re.is_exact() != im.is_exact() {
                return None;
            }
            let z = arith::make_rectangular(&re, &im).ok()?;
            matches!(z, Number::Complex(_)).then_some(z)
        }
        _ => None,
    }
}

/// An error if `code` is `%catch`, `%call/cc` or `%continue` and does
/// not take `argc` arguments. Function entry checks every other kind; these
/// read their arguments straight from the call window, so a call with too
/// few would read past it.
fn control_arity<S: Strategy>(code: &Code<S>, argc: usize) -> Result<(), Error> {
    if matches!(code.kind, Kind::Catch | Kind::Capture | Kind::Resume) && !code.arity.accepts(argc)
    {
        return Err(Error::scheme(format!(
            "{}: wrong number of arguments: got {argc}",
            code.name
        )));
    }
    Ok(())
}

/// Where the instruction at `pc` of `code` came from, if its module says.
fn position_of<S: Strategy>(code: &Code<S>, pc: usize) -> Option<crate::handle::Position> {
    let Kind::Bytecode { module, index, .. } = code.kind else {
        return None;
    };
    // SAFETY: a function's descriptor points to the module that owns it,
    // which outlives it.
    let module = unsafe { &(*module).module };
    let function = module.functions.get(index as usize)?;
    let at = function
        .positions
        .partition_point(|p| p.pc as usize <= pc)
        .checked_sub(1)?;
    let position = function.positions[at];
    Some(crate::handle::Position {
        source: module.sources.get(usize::from(position.source))?.clone(),
        line: position.line,
        column: position.column,
    })
}

/// FNV-1a: a fixed, deterministic hash for symbol names (ADR 0001 #4).
fn symbol_hash(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// What the dispatch loop hands back to the call machinery.
enum Next {
    Call {
        base: usize,
        argc: usize,
        resume: u64,
    },
    TailCall {
        argc: usize,
    },
    Return(Value),
    /// A frame whose position word is `position` returned `value` to
    /// `caller`, which is sealed in a continuation: copy it in and resume
    /// it.
    Underflow {
        caller: *mut u64,
        position: u64,
        value: Value,
    },
    /// Something was raised.
    Raise(Error),
}

#[derive(Clone, Copy)]
pub(crate) enum Arith {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy)]
pub(crate) enum Compare {
    Lt,
    Le,
    Eq,
}

pub(crate) fn type_error(operation: &str, expected: &str, got: Value) -> Error {
    Error::scheme(format!("{operation}: expected {expected}, got {got:?}"))
}

/// A type error from `name`, naming what it got as `write` prints it.
pub(crate) fn wrong_type<S: Strategy>(cx: &Cx<S>, name: &str, expected: &str, got: Value) -> Error {
    Error::scheme(format!(
        "{name}: expected {expected}, got {}",
        cx.written(got)
    ))
}

/// Loading modules, and the procedures Rust code calls.
impl<S: Strategy> Cx<S> {
    /// Validates `module` and loads it into this isolate: its constants are
    /// created in the heap, and its variables are linked (ADR 0005 #4). A
    /// variable it defines gets a cell of its own, unbound until the body
    /// sets it; an imported one shares the cell of the loaded library that
    /// exports it; a global one shares the global's cell, which a host
    /// defines, before or after.
    pub fn load(&mut self, module: Module) -> Result<ModuleId, Error> {
        validate(&module)
            .map_err(|e| Error::scheme(format!("invalid bytecode in {}: {e}", module.name)))?;
        // Modules link to the primitives as they load, so the first load
        // installs them.
        self.install_primitives()?;
        let loaded = Loaded::<S>::new(module);
        let mut constants = Vec::with_capacity(loaded.module.constants.len());
        for constant in &loaded.module.constants {
            let value = self.constant(constant)?;
            self.hold(value);
            constants.push(value);
        }
        let defined = loaded
            .module
            .variables
            .iter()
            .filter(|v| v.link == VariableLink::Defined)
            .count();
        let cells: Box<[UnsafeCell<Value>]> = (0..defined)
            .map(|_| UnsafeCell::new(Value::UNBOUND))
            .collect();
        let mut next = 0;
        let mut variables = Vec::with_capacity(loaded.module.variables.len());
        for variable in &loaded.module.variables {
            variables.push(match &variable.link {
                VariableLink::Defined => {
                    next += 1;
                    Cell(cells[next - 1].get())
                }
                VariableLink::Global => Cell(self.global_cell(&variable.name)),
                VariableLink::Primitive => Cell(self.primitive_cell(&variable.name)),
                VariableLink::Native(library) => Cell(self.native_cell(library, &variable.name)?),
                VariableLink::Imported(library) => self.exported(library, &variable.name)?,
            });
        }
        let id = self.instances.len();
        self.instance_of.insert(Arc::as_ptr(&loaded) as usize, id);
        self.instances.push(Instance {
            loaded,
            constants,
            cells,
            variables: variables.into(),
        });
        Ok(ModuleId(id))
    }

    /// Makes the loaded module `module` the library with identity
    /// `identity`, which modules loaded later can import from.
    pub fn register_library(&mut self, identity: &str, module: ModuleId) {
        self.libraries.insert(identity.into(), module);
    }

    /// The loaded library with identity `identity`, if there is one.
    pub fn library(&self, identity: &str) -> Option<ModuleId> {
        self.libraries.get(identity).copied()
    }

    /// The bytecode of the loaded module `id`.
    pub fn module(&self, id: ModuleId) -> &vali_bytecode::Module {
        &self.instances[id.0].loaded.module
    }

    /// The cell the library `library` exports as `name`.
    fn exported(&self, library: &str, name: &str) -> Result<Cell, Error> {
        let Some(&ModuleId(id)) = self.libraries.get(library) else {
            return Err(Error::scheme(format!("library {library} is not loaded")));
        };
        let instance = &self.instances[id];
        let export = instance
            .loaded
            .module
            .exports
            .iter()
            .find(|e| &*e.name == name)
            .ok_or_else(|| Error::scheme(format!("library {library} does not export {name}")))?;
        Ok(instance.variables[usize::from(export.variable)])
    }

    /// The cell of the global `name`, made unbound if there is none yet.
    fn global_cell(&mut self, name: &str) -> *mut Value {
        self.globals
            .entry(name.into())
            .or_insert_with(|| Box::new(UnsafeCell::new(Value::UNBOUND)))
            .get()
    }

    /// The cell of the primitive `name`, made unbound if there is none
    /// yet, as for the syntax procedures outside an expander's isolate.
    fn primitive_cell(&mut self, name: &str) -> *mut Value {
        self.primitives
            .entry(name.into())
            .or_insert_with(|| Box::new(UnsafeCell::new(Value::UNBOUND)))
            .get()
    }

    /// The cell holding the function `name` of the native library
    /// `library`, as a procedure, made the first time a module links to it.
    fn native_cell(&mut self, library: &str, name: &str) -> Result<*mut Value, Error> {
        let key = (Box::from(library), Box::from(name));
        if let Some(cell) = self.native_cells.get(&key) {
            return Ok(cell.get());
        }
        let function = crate::bridge::native_function(self, library, name)?;
        let procedure = self.bridge_procedure(function)?;
        let cell = Box::new(UnsafeCell::new(Value::UNBOUND));
        let pointer = cell.get();
        self.native_cells.insert(key, cell);
        // SAFETY: the cell belongs to this isolate.
        unsafe { self.write_cell(pointer, procedure) };
        Ok(pointer)
    }

    /// A loaded module's body, as a procedure of no arguments, to call
    /// rather than run: a Rust procedure can tail-call it, where it could
    /// not run it.
    pub fn module_body(&mut self, module: ModuleId) -> Result<Handle, Error> {
        let instance = &self.instances[module.0];
        let body =
            &instance.loaded.functions[instance.loaded.module.body as usize] as *const Code<S>;
        let closure = self.closure_raw(body, &[])?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(closure) })
    }

    /// Runs a loaded module's body, returning its result.
    pub fn run(&mut self, module: ModuleId) -> Result<Handle, Error> {
        let instance = &self.instances[module.0];
        let body =
            &instance.loaded.functions[instance.loaded.module.body as usize] as *const Code<S>;
        let closure = self.closure_raw(body, &[])?;
        let result = self.execute(closure, &[])?;
        // SAFETY: the VM returns valid values.
        let result = unsafe { self.root(result) };
        // Every return to the host is a safepoint.
        if S::COUNTS {
            self.epoch(&[]);
        }
        Ok(result)
    }

    /// The values the VM holds besides the frames, which an epoch counts as
    /// the stack's: the handlers of the `%catch`es waiting for their thunks
    /// and the continuations that hold their frames, and the continuation
    /// below the stack.
    pub(crate) fn control_values(&self) -> impl Iterator<Item = Value> + '_ {
        self.catches
            .iter()
            .flat_map(|c| [c.handler, c.record])
            .chain([self.below])
            .chain(self.dynamic)
    }

    /// The value of a loaded module's variable, if it has one.
    pub fn variable(&mut self, module: ModuleId, name: &str) -> Option<Handle> {
        let instance = &self.instances[module.0];
        let index = instance
            .loaded
            .module
            .variables
            .iter()
            .position(|v| &*v.name == name)?;
        // SAFETY: cells belong to this isolate and hold valid values.
        let value = unsafe { *instance.variables[index].0 };
        if value == Value::UNBOUND {
            return None;
        }
        // SAFETY: variables hold valid values.
        Some(unsafe { self.root(value) })
    }

    /// Binds the global `name`, which variables of the same name in modules
    /// loaded later start out bound to.
    pub fn define(&mut self, name: &str, value: &Handle) -> Result<(), Error> {
        let value = self.get(value)?;
        // SAFETY: the cell belongs to this isolate.
        unsafe {
            let cell = self.global_cell(name);
            self.write_cell(cell, value);
        }
        Ok(())
    }

    /// From inside a Rust procedure: asks the VM to tail-call `procedure`
    /// with `args` in place of the Rust procedure's own result, which is
    /// then ignored (ADR 0004 #10). This is how a Rust procedure calls
    /// Scheme without running Scheme code inside itself.
    ///
    /// # Safety
    ///
    /// `procedure` and `args` must be values valid in this isolate, as a
    /// Rust procedure's own result must be.
    pub unsafe fn tail_call(&mut self, procedure: Value, args: Vec<Value>) {
        self.tail_call = Some((procedure, args));
    }

    /// The value bound to the global `name`, if there is one.
    pub fn global(&mut self, name: &str) -> Option<Handle> {
        // SAFETY: global cells hold valid values.
        let value = unsafe { *self.globals.get(name)?.get() };
        if value == Value::UNBOUND {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { self.root(value) })
    }

    /// The value of the primitive `name`, if there is one: a procedure
    /// implemented in Rust that `(vali primitive)` exports.
    pub fn primitive(&mut self, name: &str) -> Option<Handle> {
        // SAFETY: primitive cells hold valid values.
        let value = unsafe { *self.primitives.get(name)?.get() };
        if value == Value::UNBOUND {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { self.root(value) })
    }

    /// `%catch`, `%call/cc` and `%continue`, bound as the primitives of
    /// those names.
    pub(crate) fn define_control(&mut self) -> Result<(), Error> {
        for code in Code::control() {
            let code = Box::new(code);
            let pointer = &*code as *const Code<S>;
            let name = code.name().to_owned();
            self.natives.push(code);
            let closure = self.closure_raw(pointer, &[])?;
            // SAFETY: the cell belongs to this isolate.
            unsafe {
                let cell = self.primitive_cell(&name);
                self.write_cell(cell, closure);
            }
        }
        Ok(())
    }

    /// A procedure implemented in Rust, bound as the primitive `name`,
    /// which `(vali primitive)` exports. For Vali's own crates, which
    /// define the syntax and `eval` procedures this way; a host defines
    /// its procedures in a native library (ADR 0016).
    #[doc(hidden)]
    pub fn define_primitive(
        &mut self,
        name: &str,
        arity: Arity,
        f: NativeFn<S>,
    ) -> Result<(), Error> {
        let code = Box::new(Code::native(name, arity, f));
        let pointer = &*code as *const Code<S>;
        self.natives.push(code);
        let closure = self.closure_raw(pointer, &[])?;
        // SAFETY: the cell belongs to this isolate.
        unsafe {
            let cell = self.primitive_cell(name);
            self.write_cell(cell, closure);
        }
        Ok(())
    }

    /// A procedure implemented in Rust, bound to the global `name`.
    ///
    /// # Safety
    ///
    /// `f` must return only values valid in this isolate: its arguments,
    /// immediates, or objects the isolate made during the call. It must not
    /// keep a raw value past the call, since the isolate may free it. A
    /// bridge (ADR 0016) needs none of this.
    pub unsafe fn define_native(
        &mut self,
        name: &str,
        arity: Arity,
        f: NativeFn<S>,
    ) -> Result<Handle, Error> {
        let code = Box::new(Code::native(name, arity, f));
        let pointer = &*code as *const Code<S>;
        self.natives.push(code);
        let closure = self.closure_raw(pointer, &[])?;
        // SAFETY: the cell belongs to this isolate.
        unsafe {
            let cell = self.global_cell(name);
            self.write_cell(cell, closure);
        }
        // SAFETY: just allocated.
        Ok(unsafe { self.root(closure) })
    }

    /// Calls `procedure` with `args`.
    pub fn call(&mut self, procedure: &Handle, args: &[Handle]) -> Result<Handle, Error> {
        let procedure = self.get(procedure)?;
        let args = args
            .iter()
            .map(|a| self.get(a))
            .collect::<Result<Vec<_>, _>>()?;
        let result = self.execute(procedure, &args)?;
        // SAFETY: the VM returns valid values.
        let result = unsafe { self.root(result) };
        if S::COUNTS {
            self.epoch(&[]);
        }
        Ok(result)
    }

    /// The interned symbol named `name`.
    pub fn symbol(&mut self, name: &str) -> Result<Handle, Error> {
        let v = self.intern(name)?;
        // SAFETY: just interned.
        Ok(unsafe { self.root(v) })
    }

    /// A symbol's name.
    ///
    /// # Safety
    ///
    /// `v` must be a live symbol.
    pub unsafe fn symbol_name(&self, v: Value) -> &str {
        // SAFETY: passed on to the caller; a symbol's name is a string.
        unsafe { self.string_str(Value::from_raw(get(v, Self::LAYOUT.symbol_name()))) }
    }

    pub(crate) fn intern(&mut self, name: &str) -> Result<Value, OutOfMemory> {
        if let Some(&v) = self.symbols.get(name) {
            return Ok(v);
        }
        let string = self.string_raw(name, true)?;
        let v = self.new_object(TypeCode::Symbol, 2, data::symbol::INTERNED)?;
        // SAFETY: the symbol was just allocated with both fields.
        unsafe {
            self.init_field(v, Self::LAYOUT.symbol_name(), string);
            put(v, Self::LAYOUT.symbol_hash(), symbol_hash(name));
        }
        self.hold(v);
        self.symbols.insert(name.into(), v);
        Ok(v)
    }

    fn constant(&mut self, constant: &Constant) -> Result<Value, Error> {
        Ok(match constant {
            Constant::Integer(n) => self.integer_raw(&Integer::from(*n))?,
            Constant::Flonum(x) => self.flonum_raw(*x)?,
            Constant::Boolean(b) => Value::boolean(*b),
            Constant::Character(c) => Value::character(*c),
            Constant::EmptyList => Value::EMPTY_LIST,
            Constant::Unspecified => Value::UNSPECIFIED,
            Constant::String(s) => self.string_raw(s, true)?,
            Constant::Symbol(s) => self.intern(s)?,
            Constant::Keyword(s) => self.keyword_raw(s)?,
            Constant::Pair(car, cdr) => {
                let car = self.constant(car)?;
                let cdr = self.constant(cdr)?;
                // SAFETY: both are valid values.
                unsafe { self.cons_raw(car, cdr)? }
            }
            Constant::Bignum { negative, limbs } => {
                self.integer_raw(&Integer::from_limbs(*negative, limbs))?
            }
            Constant::Bytevector(bytes) => {
                let v = self.bytevector_raw(bytes)?;
                self.make_immutable(v);
                v
            }
            Constant::Ratio(..) | Constant::Complex(..) => {
                let n = constant_number(constant).ok_or_else(|| {
                    Error::scheme(format!("invalid numeric constant {constant:?}"))
                })?;
                self.number_raw(&n)?
            }
            Constant::Vector(items) => {
                // SAFETY: the fill is a valid value.
                let v = unsafe { self.make_vector_raw(items.len(), Value::UNSPECIFIED)? };
                for (i, item) in items.iter().enumerate() {
                    let x = self.constant(item)?;
                    // SAFETY: `v` has `items.len()` elements.
                    unsafe { self.vector_set(v, i, x) };
                }
                v
            }
        })
    }

    /// A new closure of `code`, capturing `free`.
    fn closure_raw(&mut self, code: *const Code<S>, free: &[Value]) -> Result<Value, OutOfMemory> {
        let v = self.new_object(TypeCode::Closure, 1 + free.len(), free.len() as u32)?;
        // SAFETY: the closure was just allocated with room for its code
        // pointer and free variables.
        unsafe {
            put(v, Self::LAYOUT.closure_code(), code as u64);
            for (i, x) in free.iter().enumerate() {
                self.init_field(v, Self::LAYOUT.closure_free(i), *x);
            }
        }
        Ok(v)
    }

    /// The code of `v`, if it is a procedure.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub(crate) unsafe fn procedure_code<'a>(&self, v: Value) -> Result<&'a Code<S>, Error> {
        if v.tag() != Tag::Closure {
            return Err(Error::scheme(format!("not a procedure: {v:?}")));
        }
        // SAFETY: a closure's first word points to a live descriptor, owned
        // by a loaded module or this isolate's natives, which outlive it.
        Ok(unsafe { &*(get(v, Self::LAYOUT.closure_code()) as *const Code<S>) })
    }

    /// The name of the procedure `v`, if it is one.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub(crate) unsafe fn procedure_name(&self, v: Value) -> Option<String> {
        // SAFETY: passed on to the caller.
        unsafe { self.procedure_code(v) }
            .ok()
            .map(|code| code.name().to_owned())
    }

    /// Whether `v` is a heap object of type `t` under the "other" tag.
    pub(crate) fn is_other(&self, v: Value, t: TypeCode) -> bool {
        // SAFETY: values in slots are valid, so an "other" pointer points to
        // a live object with a header.
        v.tag() == Tag::Other && unsafe { self.header(v, t) }.and_then(Header::type_code) == Some(t)
    }

    /// A bignum's value.
    ///
    /// # Safety
    ///
    /// `v` must be a live bignum.
    pub(crate) unsafe fn bignum_value(&self, v: Value) -> Integer {
        // SAFETY: passed on to the caller; a bignum is its header, its limb
        // count, then its limbs.
        unsafe {
            let negative = self
                .header(v, TypeCode::Bignum)
                .is_some_and(|h| h.data() & 1 != 0);
            let count = get(v, Self::LAYOUT.length(TypeCode::Bignum)) as usize;
            let limbs: Vec<u64> = (0..count)
                .map(|i| get(v, Self::LAYOUT.field(TypeCode::Bignum, 1 + i)))
                .collect();
            Integer::from_limbs(negative, &limbs)
        }
    }

    /// `n` as a value: a fixnum if it fits, a bignum otherwise.
    pub(crate) fn integer_raw(&mut self, n: &Integer) -> Result<Value, OutOfMemory> {
        if let Some(v) = n.to_i64().and_then(Value::fixnum) {
            return Ok(v);
        }
        let (negative, limbs) = n.limbs();
        let v = self.new_object(TypeCode::Bignum, 1 + limbs.len(), u32::from(negative))?;
        // SAFETY: the bignum was just allocated with room for its count and
        // limbs.
        unsafe {
            put(v, Self::LAYOUT.length(TypeCode::Bignum), limbs.len() as u64);
            for (i, &limb) in limbs.iter().enumerate() {
                put(v, Self::LAYOUT.field(TypeCode::Bignum, 1 + i), limb);
            }
        }
        Ok(v)
    }

    /// A `%catch`'s thunk returned: its catch is done. Out of line, so the
    /// return path it sits on stays small; inlined, it cost fib 10%.
    #[cold]
    #[inline(never)]
    fn catch_returned(&mut self) {
        if let Some(catch) = self.catches.pop()
            && S::PRECISE
        {
            S::release(self.heap_mut(), catch.handler);
            S::release(self.heap_mut(), catch.record);
        }
    }

    /// A `%catch` now waits for its thunk: under precise counting, it
    /// holds its handler as a reference of its own, since the thunk's frame
    /// overwrites the slot the handler came from, and its record.
    fn push_catch(&mut self, catch: Catch) {
        if S::PRECISE {
            S::retain(self.heap_mut(), catch.handler);
            S::retain(self.heap_mut(), catch.record);
        }
        self.catches.push(catch);
    }

    /// Stores `v` in slot `i` of the frame at `fp`. Under precise counting,
    /// `v` is counted in and the value it replaces out.
    ///
    /// # Safety
    ///
    /// Slot `i` must be inside the stack, holding a value counted under
    /// precise counting, or a word that reads as a fixnum.
    #[inline(always)]
    unsafe fn store(&mut self, fp: *mut u64, i: usize, v: Value) {
        // SAFETY: passed on to the caller.
        unsafe {
            if S::PRECISE {
                let old = slot(fp, i);
                S::write(self.heap_mut(), old, v);
            }
            set(fp, i, v);
        }
    }

    /// Under precise counting, counts slots `from` to `to` (exclusive) of
    /// the frame at `fp` out and clears them: they are dead. Every stack
    /// word outside a live frame is cleared this way, so a store over it
    /// counts nothing out.
    ///
    /// # Safety
    ///
    /// As `store`, for each slot.
    #[inline(always)]
    unsafe fn release_slots(&mut self, fp: *mut u64, from: usize, to: usize) {
        if S::PRECISE {
            for i in from..to {
                // SAFETY: passed on to the caller.
                unsafe {
                    let old = slot(fp, i);
                    set(fp, i, Value::UNSPECIFIED);
                    S::release(self.heap_mut(), old);
                }
            }
        }
    }

    /// How many slots the suspended bytecode frame at `fp` has.
    ///
    /// # Safety
    ///
    /// `fp` must be a bytecode function's frame.
    unsafe fn suspended_slots(&self, fp: *mut u64) -> usize {
        // SAFETY: passed on to the caller: slot 0 holds the frame's closure.
        match unsafe { self.procedure_code(slot(fp, 0)) }.map(|c| &c.kind) {
            Ok(Kind::Bytecode { slots, .. }) => usize::from(*slots),
            _ => 0,
        }
    }

    /// Under precise counting, counts out and clears the slots of every
    /// frame from the innermost, at `fp` with `slots` slots, out to `to`
    /// (exclusive), or to the outermost live frame (inclusive) if `to` is
    /// null. A frame's slots overlap its caller's, which are cleared after
    /// it.
    ///
    /// # Safety
    ///
    /// `fp` must be the innermost frame, `to` one of its callers or null.
    unsafe fn release_frames(&mut self, mut fp: *mut u64, mut slots: usize, to: *mut u64) {
        if !S::PRECISE {
            return;
        }
        // SAFETY: frames link to their callers up to the entry frame, and
        // a frame's header lies in its caller's slots, released only after
        // it is read.
        unsafe {
            while fp != to {
                self.release_slots(fp, 0, slots);
                let position = *fp.add(2);
                if position == ENTRY || position & UNDERFLOW != 0 {
                    break;
                }
                fp = *fp as *mut u64;
                slots = self.suspended_slots(fp);
            }
        }
    }

    /// What a handler sees for `error`: what Scheme code raised, or for an
    /// error from a Rust procedure, an error object with its message.
    fn condition(&mut self, error: Error) -> Result<Value, Error> {
        match error {
            Error::Scheme(e) => match e.raised {
                Some(raised) => self.get(&raised),
                None => {
                    let message = self.string_raw(&e.message, true)?;
                    Ok(self.error_object_raw(message, Value::EMPTY_LIST)?)
                }
            },
            Error::OutOfMemory => Err(Error::OutOfMemory),
            other => {
                let message = self.string_raw(&other.to_string(), true)?;
                Ok(self.error_object_raw(message, Value::EMPTY_LIST)?)
            }
        }
    }

    /// Adds a backtrace, read from the frames starting at `fp`, to a Scheme
    /// error.
    ///
    /// # Safety
    ///
    /// `fp` must be the frame pointer of a live frame.
    unsafe fn with_backtrace(&self, mut fp: *mut u64, mut pc: usize, error: Error) -> Error {
        let Error::Scheme(mut e) = error else {
            return error;
        };
        loop {
            // SAFETY: every frame's slot 0 holds its closure, and its header
            // links to its caller, up to the entry frame.
            unsafe {
                if let Ok(code) = self.procedure_code(slot(fp, 0)) {
                    e.backtrace.push(crate::handle::Frame {
                        name: code.name().to_owned(),
                        position: position_of(code, pc),
                    });
                }
                let position = *fp.add(2);
                if position == ENTRY {
                    break;
                }
                // The caller waits at the call before where it resumes.
                pc = (position_resume(position) as usize).saturating_sub(1);
                fp = *fp as *mut u64;
            }
        }
        Error::Scheme(e)
    }

    fn execute(&mut self, procedure: Value, args: &[Value]) -> Result<Value, Error> {
        if self.running {
            return Err(Error::scheme(
                "calling Scheme from inside Scheme code is not supported yet",
            ));
        }
        if args.len() > usize::from(u8::MAX) {
            return Err(Error::scheme("too many arguments"));
        }
        if self.stack.is_empty() {
            self.push_segment(SEGMENT_WORDS)?;
        }
        // Every host call starts in an empty dynamic environment, however
        // the last one ended.
        self.reset_dynamic();
        let segment = &self.stack[0];
        // The entry frame is a pseudo-frame at the top of the segment. The
        // word at its frame pointer will hold the host's stack pointer, for
        // unwinders; its slots 0 to 2 hold the callee's header, slot 3 the
        // procedure, and the arguments follow.
        // SAFETY: the segment has room for 5 + 255 words.
        let (entry, limit) = unsafe { (segment.top().sub(1), segment.limit()) };
        // SAFETY: as above.
        unsafe {
            self.store(entry, 3, procedure);
            for (i, &arg) in args.iter().enumerate() {
                self.store(entry, 4 + i, arg);
            }
        }
        self.running = true;
        let mut state = EnterState {
            record: std::ptr::null_mut(),
            cx: self as *mut Cx<S>,
            entry,
            limit,
            argc: args.len(),
            result: None,
            panic: None,
        };
        // SAFETY: `body` restores the trampoline's record before returning,
        // and the entry frame is set up as `interpret` expects.
        unsafe { trampoline::enter(&raw mut state as *mut c_void, body::<S>) };
        self.running = false;
        // A panic in the VM itself leaves the same state as an error would,
        // so the next call starts clean instead of resuming dead frames.
        self.finish_run();
        if let Some(panic) = state.panic {
            std::panic::resume_unwind(panic);
        }
        state.result.expect("the VM ran")
    }

    /// Resets what a run leaves behind, however it ended: waiting catches,
    /// the continuation below, the dynamic environment, a pending tail
    /// call, and chained or shared segments.
    fn finish_run(&mut self) {
        self.tail_call = None;
        while let Some(catch) = self.catches.pop() {
            if S::PRECISE {
                S::release(self.heap_mut(), catch.handler);
                S::release(self.heap_mut(), catch.record);
            }
        }
        self.set_below(Value::FALSE);
        self.reset_dynamic();
        // An error can leave segments chained; only the first is kept, and
        // only if it is whole, since the next run starts at its top.
        while self.stack.len() > 1 {
            self.pop_segment();
        }
        if !self.stack[0].is_whole() {
            self.drop_view();
        }
    }

    /// Chains a segment that holds at least `words` words, reusing the
    /// spare if it is big enough, and returns its top and limit.
    fn push_segment(&mut self, words: usize) -> Result<(*mut u64, *mut u64), Error> {
        let words = words.max(SEGMENT_WORDS);
        let segment = match self.spare.take() {
            Some(spare) if spare.words() >= words => spare,
            spare => {
                if let Some(spare) = spare {
                    self.stack_words -= spare.words();
                }
                if self.stack_words + words > self.max_stack_words {
                    return Err(Error::scheme(format!(
                        "stack overflow: the stack would exceed {} bytes",
                        self.max_stack_words * 8
                    )));
                }
                self.stack_words += words;
                Segment::new(words)
            }
        };
        let ends = (segment.top(), segment.limit());
        self.stack.push(View::whole(segment));
        Ok(ends)
    }

    /// Drops the current segment, keeping it as the spare if it can be
    /// reused, and returns the previous segment's limit.
    fn pop_segment(&mut self) -> *mut u64 {
        self.drop_view();
        self.stack.last().expect("the first segment stays").limit()
    }

    /// Drops the current segment from the stack, keeping it as the spare if
    /// it can be reused.
    fn drop_view(&mut self) {
        let view = self.stack.pop().expect("a segment to drop");
        let words = view.words();
        match view.reclaim() {
            Some(segment) => {
                if let Some(old) = self.spare.replace(segment) {
                    self.stack_words -= old.words();
                }
            }
            None => self.stack_words -= words,
        }
    }

    /// Copies the sealed frame at `f`, whose own slots are the first `own`,
    /// to the top of the current segment, or of a new one if it does not
    /// fit, with a header that returns to its caller where that is. Returns
    /// the copy and its segment's limit.
    ///
    /// # Safety
    ///
    /// `f` must be a frame of the continuation below the stack, and the
    /// current segment must hold no live frame.
    unsafe fn materialize(
        &mut self,
        link: &Link,
        f: *mut u64,
        own: usize,
    ) -> Result<(*mut u64, *mut u64), Error> {
        // SAFETY: passed on to the caller; the sealed frame is intact.
        unsafe {
            let position = *f.add(2);
            let slots = self.suspended_slots(f).max(own + 4);
            let header = if position == ENTRY { 4 } else { 3 };
            let view = self.stack.last().expect("a segment");
            let (mut top, mut limit) = (view.top(), view.limit());
            let mut flags = 0;
            if !fits(top.sub(header), slots, limit) {
                (top, limit) = self.push_segment(slots + 4)?;
                flags = BOUNDARY;
            }
            let copy = top.sub(header);
            if position == ENTRY {
                // Returning to the host returns to whichever host runs the
                // VM now.
                *copy = link.host_fp;
                *copy.add(1) = link.host_return;
                *copy.add(2) = ENTRY;
                *copy.add(3) = link.host_cfa;
            } else {
                *copy = *f;
                *copy.add(1) = *f.add(1);
                *copy.add(2) = (position & !BOUNDARY) | UNDERFLOW | flags;
            }
            // The copy is on the stack: precise counting counts its values
            // in, while the continuation keeps its own.
            for i in 0..own {
                let v = slot(f, i);
                set(copy, i, v);
                if S::PRECISE {
                    S::retain(self.heap_mut(), v);
                }
            }
            for i in own..slots {
                set(copy, i, Value::UNSPECIFIED);
            }
            if position == ENTRY {
                self.set_below(Value::FALSE);
            } else if position & UNDERFLOW != 0 {
                // The caller is the parent's.
                let parent = Self::sealed(self.below).expect("a continuation").parent;
                self.set_below(parent);
            }
            Ok((copy, limit))
        }
    }

    /// Checks that `%call/cc`, called with `argc` arguments, has a receiver
    /// it can call: a Scheme procedure that takes the continuation as a
    /// parameter of its own, so setting up its frame never fails once the
    /// stack is sealed.
    fn receiver(&self, receiver: Value, argc: usize) -> Result<(), Error> {
        // SAFETY: a valid value.
        let ok = argc == 1
            && matches!(unsafe { self.procedure_code(receiver) }, Ok(code)
                if matches!(code.kind, Kind::Bytecode { .. })
                    && code.arity.accepts(1)
                    && code.arity.required + code.arity.optional >= 1);
        if ok {
            Ok(())
        } else {
            Err(wrong_type(
                self,
                "%call/cc",
                "a Scheme procedure of one argument",
                receiver,
            ))
        }
    }

    /// What returning `value` to the continuation `k` comes to, once the
    /// live stack is dropped: copying its first frame in, or returning to
    /// the host. Under precise counting, `k` and `value` come counted in,
    /// and leave counted out.
    fn resume_into(&mut self, k: Value, value: Value) -> Next {
        let target = self.reinstate(k);
        if S::PRECISE {
            S::release(self.heap_mut(), k);
            S::release(self.heap_mut(), value);
        }
        match target {
            None => Next::Return(value),
            Some((caller, position)) => {
                if position & CATCH != 0 {
                    self.catch_returned();
                }
                if position & RAISED != 0 {
                    return Next::Raise(self.handler_returned());
                }
                Next::Underflow {
                    caller,
                    position,
                    value,
                }
            }
        }
    }

    /// The error for returning through a frame the raise procedure was
    /// called in: an escape, so it is not handed to the raise procedure
    /// again.
    fn handler_returned(&mut self) -> Error {
        let message = "the raise procedure returned";
        match self.string_raw(message, true) {
            // SAFETY: just allocated.
            Ok(s) => Error::escape(message, unsafe { self.root(s) }),
            Err(e) => e.into(),
        }
    }

    /// Drops the live stack, the innermost frame at `fp` with `slots`
    /// slots, or only the entry frame's values if `fp` is `top`, the entry
    /// frame. Returns the one segment left's limit.
    ///
    /// # Safety
    ///
    /// `fp` must be the innermost live frame.
    unsafe fn discard(&mut self, fp: *mut u64, slots: usize, top: *mut u64) -> *mut u64 {
        // SAFETY: passed on to the caller.
        unsafe {
            if fp == top {
                self.release_slots(top, 3, 3 + slots);
            } else {
                self.release_frames(fp, slots, std::ptr::null_mut());
            }
        }
        while self.stack.len() > 1 {
            self.drop_view();
        }
        self.stack[0].limit()
    }

    /// How many Scheme procedures have been entered so far: the fuel used
    /// (ADR 0009).
    pub fn fuel_used(&self) -> u64 {
        self.fuel_used
    }

    /// Sets how many procedures the isolate may enter, counting from when
    /// it was made, or no limit. Past it, a run ends with
    /// [`Error::OutOfFuel`], which Scheme code cannot catch (ADR 0009 #21).
    pub fn set_fuel_limit(&mut self, limit: Option<u64>) {
        self.fuel_limit = limit;
    }

    /// Sets the most memory the stack may take, in bytes. Deep recursion
    /// beyond it is an error.
    pub fn set_max_stack_bytes(&mut self, bytes: usize) {
        self.max_stack_words = bytes / 8;
    }

    /// Sets up a bytecode function's frame at `fp`, which the caller has
    /// made sure fits: checks the arity, fills omitted optional arguments,
    /// builds the rest list, and clears the other slots.
    ///
    /// # Safety
    ///
    /// `fp` must point to a frame whose slots 0 to `argc` hold the closure
    /// and arguments.
    unsafe fn enter(&mut self, code: &Code<S>, fp: *mut u64, argc: usize) -> Result<(), Error> {
        let Kind::Bytecode { slots, .. } = code.kind else {
            unreachable!("only bytecode functions are entered");
        };
        let arity = code.arity;
        if !arity.accepts(argc) {
            return Err(Error::scheme(format!(
                "{}: wrong number of arguments: expected {}{}{}, got {argc}",
                code.name,
                arity.required,
                if arity.optional > 0 {
                    format!(" to {}", arity.required + arity.optional)
                } else {
                    String::new()
                },
                if arity.rest { " or more" } else { "" },
            )));
        }
        let slots = usize::from(slots);
        let fixed = usize::from(arity.required) + usize::from(arity.optional);
        // SAFETY: every slot written is inside the frame, which the caller
        // made sure fits; arguments beyond the frame were written by the
        // caller inside its own frame, or copied with it to a new segment.
        unsafe {
            for i in argc + 1..=fixed {
                self.store(fp, i, Value::DEFAULT);
            }
            if arity.rest {
                let mut list = Value::EMPTY_LIST;
                for i in (fixed + 1..=argc).rev() {
                    list = self.cons_raw(slot(fp, i), list)?;
                }
                self.store(fp, fixed + 1, list);
                // The rest of the arguments are in the list now; those
                // past the frame's slots are cleared, the others below.
                self.release_slots(fp, slots.max(fixed + 2), argc + 1);
            }
            for i in arity.parameter_slots() + 1..slots {
                self.store(fp, i, Value::UNSPECIFIED);
            }
        }
        self.fuel_used += 1;
        code.calls.fetch_add(1, Ordering::Relaxed);
        if self.fuel_limit.is_some_and(|limit| self.fuel_used > limit) {
            return Err(Error::OutOfFuel);
        }
        Ok(())
    }

    /// Calls a Rust procedure with the `argc` arguments in slots `first`
    /// and up of the frame at `fp`. Up to eight arguments are passed from a
    /// buffer on the Rust stack, without allocating.
    ///
    /// # Safety
    ///
    /// The slots must be inside the frame.
    unsafe fn call_native(
        &mut self,
        code: &Code<S>,
        fp: *mut u64,
        first: usize,
        argc: usize,
    ) -> Result<Value, Error> {
        if !code.arity.accepts(argc) {
            return Err(Error::scheme(format!(
                "{}: wrong number of arguments: got {argc}",
                code.name
            )));
        }
        let mut small = [Value::UNSPECIFIED; 8];
        let large: Vec<Value>;
        // SAFETY: passed on to the caller.
        let args: &[Value] = unsafe {
            if argc <= small.len() {
                for (i, arg) in small.iter_mut().take(argc).enumerate() {
                    *arg = slot(fp, first + i);
                }
                &small[..argc]
            } else {
                large = (0..argc).map(|i| slot(fp, first + i)).collect();
                &large
            }
        };
        // A tail call only stands for a result the procedure returns: one
        // left by a procedure that failed would hold values the next
        // collection may free, for the next Rust procedure to call.
        self.tail_call = None;
        let result = match &code.kind {
            // A panic in a primitive becomes an error naming it, as in a
            // bridge, so it never unwinds through the frames it ran in.
            Kind::Native(f) => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self, args)))
                    .unwrap_or_else(|panic| {
                        Err(Error::scheme(format!(
                            "{}: panicked: {}",
                            code.name,
                            crate::bridge::panic_message(&*panic)
                        )))
                    })
            }
            Kind::Bridge(f) => f.invoke(self, args),
            _ => unreachable!("only Rust procedures are called here"),
        };
        if result.is_err() {
            self.tail_call = None;
        }
        result
    }

    /// A procedure calling `f`.
    pub(crate) fn bridge_procedure(
        &mut self,
        f: std::sync::Arc<crate::bridge::NativeFunction>,
    ) -> Result<Value, Error> {
        let code = Box::new(Code::bridge(f));
        let pointer = &*code as *const Code<S>;
        self.natives.push(code);
        Ok(self.closure_raw(pointer, &[])?)
    }

    /// The constants and variables of the loaded module at `module`.
    fn instance_registers(&mut self, module: usize) -> (*const Value, *const Cell) {
        let index = self.instance_of[&module];
        let instance = &self.instances[index];
        (instance.constants.as_ptr(), instance.variables.as_ptr())
    }

    /// Runs until the entry frame at `top` receives its result, keeping the
    /// trampoline's frame record in `link` pointing at the current frame.
    ///
    /// # Safety
    ///
    /// `top` must be the entry frame at the top of the stack segment ending
    /// at `limit`, with the procedure in its slot 3 and `argc` arguments
    /// after it.
    unsafe fn interpret(
        &mut self,
        link: &Link,
        top: *mut u64,
        mut limit: *mut u64,
        argc: usize,
    ) -> Result<Value, Error> {
        let mut fp = top;
        // SAFETY: `top` is the entry frame's frame pointer, inside the
        // segment.
        unsafe { *top = link.host_cfa };
        // Read before the first instruction only by a backtrace.
        let mut pc: usize = 0;
        let mut code: *const u32 = std::ptr::null();
        let mut constants: *const Value = std::ptr::null();
        let mut variables: *const Cell = std::ptr::null();
        let mut current_module = 0usize;
        let mut next = Next::Call {
            base: 0,
            argc,
            resume: ENTRY,
        };
        // Ordinary frames return into their caller's own marker stub; these
        // two are shared.
        let (entry_resume_address, boundary_resume_address) =
            (trampoline::entry_resume(), trampoline::boundary_resume());
        // The marker stub of the current frame, which frames it calls
        // return to. The entry pseudo-frame has none: its callee returns to
        // the host.
        let mut current_stub = 0u64;
        // Under precise counting, how many slots the innermost frame has,
        // to count out as it leaves; while the entry pseudo-frame is the
        // innermost, how many of its words from slot 3 hold values.
        let mut frame_slots = argc + 1;

        // SAFETY (for the whole loop): the validator checked every slot,
        // constant, variable, function and branch operand; frames are set up
        // by `enter`, which checks the stack limit; heap accesses check
        // types first.
        unsafe {
            // Switch the registers to a bytecode function's code, and to its
            // module's constants and variables if the module changes.
            macro_rules! switch_to {
                ($callee:expr) => {{
                    let Kind::Bytecode {
                        module,
                        code: words,
                        ..
                    } = $callee.kind
                    else {
                        unreachable!("only bytecode functions are switched to");
                    };
                    code = words;
                    if module as usize != current_module {
                        current_module = module as usize;
                        (constants, variables) = self.instance_registers(current_module);
                    }
                }};
            }
            // The marker stub that names a frame with this position word: the
            // stub for the bottom frame, for the first frame of a chained
            // segment, or for any other. Frames it calls return to it.
            macro_rules! stub_of {
                ($position:expr, $code:expr) => {{
                    let position = $position;
                    if position == ENTRY {
                        entry_resume_address
                    } else if position & (BOUNDARY | UNDERFLOW) != 0 {
                        // Its caller sits anywhere in memory.
                        boundary_resume_address
                    } else {
                        $code.stub
                    }
                }};
            }
            // Make the frame at `fp`, whose stub is `stub`, the current one,
            // and point the trampoline's frame record at it.
            macro_rules! link_to {
                ($fp:expr, $stub:expr) => {{
                    current_stub = $stub;
                    if cfg!(feature = "native-frames") {
                        *link.record = $fp as u64;
                        *link.record.add(1) = current_stub;
                    }
                }};
            }
            'run: loop {
                // Something was raised: the call machinery handles it, in one
                // place, so the many places that fail stay small. Defined
                // inside the loop, so it can continue it: labels in macros are
                // hygienic.
                macro_rules! fail {
                    ($e:expr) => {{
                        next = Next::Raise($e);
                        continue 'run;
                    }};
                }
                macro_rules! attempt {
                    ($e:expr) => {
                        match $e {
                            Ok(v) => v,
                            Err(e) => fail!(Error::from(e)),
                        }
                    };
                }
                // The call machinery: set up the frame `next` asks for.
                match next {
                    Next::Underflow {
                        caller,
                        position,
                        value,
                    } => {
                        // The caller is sealed: copy it in, the stack's
                        // outermost frame now, and resume it. Failing to
                        // find room ends the run: the stack is empty.
                        let base = position_base(position);
                        (fp, limit) = self.materialize(link, caller, base)?;
                        self.store(fp, base + 3, value);
                        clear_header(fp, base);
                        pc = position_resume(position) as usize;
                        let caller = attempt!(self.procedure_code(slot(fp, 0)));
                        if S::PRECISE
                            && let Kind::Bytecode { slots, .. } = caller.kind
                        {
                            frame_slots = usize::from(slots).max(base + 4);
                        }
                        link_to!(fp, stub_of!(*fp.add(2), caller));
                        switch_to!(caller);
                    }
                    Next::Raise(error)
                        if fp != top
                            && self.dynamic[crate::continuations::HANDLERS]
                                != Value::EMPTY_LIST
                            && error.reaches_handlers()
                            && self.raise_hook.is_some() =>
                    {
                        // A handler is installed: call the raise procedure
                        // with the condition where the error happened,
                        // past the innermost frame's slots, so handlers
                        // run in its dynamic environment (R7RS 6.11). It
                        // never returns.
                        let condition = self.condition(error)?;
                        let hook = self.get(self.raise_hook.as_ref().expect("checked"))?;
                        let code = self
                            .procedure_code(hook)
                            .expect("the raise procedure is a procedure");
                        let Kind::Bytecode { slots, .. } = code.kind else {
                            unreachable!("the raise procedure is a Scheme procedure");
                        };
                        let base = if S::PRECISE {
                            frame_slots
                        } else {
                            self.suspended_slots(fp)
                        };
                        let needed = usize::from(slots).max(2);
                        let mut position = position(0, base) | RAISED;
                        let mut callee_fp = fp.sub(base + 3);
                        if fits(callee_fp, needed, limit) {
                            if S::PRECISE {
                                self.release_slots(fp, base, base + 3);
                            }
                        } else {
                            // Failing to find room ends the run.
                            let (new_top, new_limit) = self.push_segment(needed + 4)?;
                            callee_fp = new_top.sub(3);
                            limit = new_limit;
                            position |= BOUNDARY;
                        }
                        *callee_fp = fp as u64;
                        *callee_fp.add(1) = current_stub;
                        *callee_fp.add(2) = position;
                        self.store(callee_fp, 0, hook);
                        self.store(callee_fp, 1, condition);
                        self.enter(code, callee_fp, 1)?;
                        if S::PRECISE {
                            frame_slots = needed;
                        }
                        fp = callee_fp;
                        link_to!(fp, stub_of!(position, code));
                        pc = 0;
                        switch_to!(code);
                    }
                    Next::Raise(error) => {
                        // Unwind to the innermost `%catch` and call its
                        // handler with the condition in place of the `%catch`
                        // call; with none, or for an exit, return the error
                        // to the host.
                        let catch = match error {
                            Error::Exit(_) | Error::OutOfFuel => None,
                            _ => self.catches.pop(),
                        };
                        let Some(catch) = catch else {
                            if fp == top {
                                self.release_slots(top, 3, 3 + frame_slots);
                                return Err(error);
                            }
                            let error = self.with_backtrace(fp, pc, error);
                            self.release_frames(fp, frame_slots, std::ptr::null_mut());
                            return Err(error);
                        };
                        let condition = self.condition(error)?;
                        if catch.record == Value::FALSE {
                            if fp != catch.fp {
                                self.release_frames(fp, frame_slots, catch.fp);
                            }
                            while self.stack.len() > catch.segments {
                                self.pop_segment();
                            }
                            limit = self.stack.last().expect("a segment").limit();
                            fp = catch.fp;
                        } else {
                            // The frame is sealed: drop the whole live
                            // stack, and copy the frame in from the
                            // continuation that holds it.
                            self.discard(fp, frame_slots, top);
                            self.set_below(catch.record);
                            if S::PRECISE {
                                S::release(self.heap_mut(), catch.record);
                            }
                            (fp, limit) = self.materialize(link, catch.fp, catch.base)?;
                        }
                        self.store(fp, catch.base + 3, catch.handler);
                        self.store(fp, catch.base + 4, condition);
                        if S::PRECISE {
                            S::release(self.heap_mut(), catch.handler);
                            frame_slots = self.suspended_slots(fp);
                        }
                        let caller = self
                            .procedure_code(slot(fp, 0))
                            .expect("a frame's slot 0 holds its procedure");
                        link_to!(fp, stub_of!(*fp.add(2), caller));
                        switch_to!(caller);
                        next = Next::Call {
                            base: catch.base,
                            argc: 1,
                            resume: catch.resume,
                        };
                        continue;
                    }
                    Next::Call { base, argc, resume } => {
                        // A call is a safepoint.
                        if S::COUNTS && fp != top && S::epoch_due(self.heap()) {
                            let roots = self.stack_roots(fp);
                            self.epoch(&roots);
                        }
                        let procedure = slot(fp, base + 3);
                        let callee = attempt!(self.procedure_code(procedure));
                        attempt!(control_arity(callee, argc));
                        match callee.kind {
                            Kind::Catch => {
                                // `(%catch thunk handler)`: note where to
                                // call the handler, then call the thunk,
                                // marking its frame so its return removes
                                // the note.
                                if resume == ENTRY {
                                    fail!(Error::scheme("%catch: cannot be called from Rust"));
                                }
                                self.push_catch(Catch {
                                    fp,
                                    base,
                                    resume,
                                    segments: self.stack.len(),
                                    handler: slot(fp, base + 5),
                                    record: Value::FALSE,
                                });
                                self.store(fp, base + 3, slot(fp, base + 4));
                                next = Next::Call {
                                    base,
                                    argc: 0,
                                    resume: resume | CATCH,
                                };
                                continue;
                            }
                            Kind::Capture => {
                                // `(%call/cc receiver)`: call the receiver,
                                // which takes the continuation in its
                                // frame; once that frame is set up, the
                                // stack from here out is sealed.
                                if resume == ENTRY {
                                    fail!(Error::scheme("%call/cc: cannot be called from Rust"));
                                }
                                let receiver = slot(fp, base + 4);
                                attempt!(self.receiver(receiver, argc));
                                // What this frame holds past the call is
                                // dead, and soon not this frame's.
                                self.release_slots(fp, base + 5, frame_slots);
                                self.store(fp, base + 3, receiver);
                                self.store(fp, base + 4, Value::FALSE);
                                next = Next::Call {
                                    base,
                                    argc: 1,
                                    resume: resume | CAPTURE,
                                };
                                continue;
                            }
                            Kind::Resume => {
                                // `(%continue k value)`: drop the stack and
                                // return `value` to `k`.
                                if resume == ENTRY {
                                    fail!(Error::scheme("%continue: cannot be called from Rust"));
                                }
                                let (k, value) = (slot(fp, base + 4), slot(fp, base + 5));
                                if argc != 2 || Self::sealed(k).is_none() {
                                    fail!(wrong_type(self, "%continue", "a continuation", k));
                                }
                                if S::PRECISE {
                                    S::retain(self.heap_mut(), k);
                                    S::retain(self.heap_mut(), value);
                                }
                                self.discard(fp, frame_slots, top);
                                next = self.resume_into(k, value);
                                if let Next::Return(value) = next {
                                    return Ok(value);
                                }
                                continue;
                            }
                            Kind::Native(_) | Kind::Bridge(_) => {
                                debug_assert!(
                                    resume == ENTRY || resume & (CAPTURE | UNDERFLOW) == 0
                                );
                                let result = attempt!(self.call_native(callee, fp, base + 4, argc));
                                if let Some((procedure, args)) = self.tail_call.take() {
                                    // The Rust procedure asked for a tail
                                    // call: make it in its place.
                                    if !fits(fp.sub(base + 3), args.len() + 2, limit) {
                                        fail!(Error::scheme(
                                            "too many arguments to fit in this stack segment"
                                        ));
                                    }
                                    self.store(fp, base + 3, procedure);
                                    for (i, &arg) in args.iter().enumerate() {
                                        self.store(fp, base + 4 + i, arg);
                                    }
                                    if S::PRECISE && resume == ENTRY {
                                        frame_slots = frame_slots.max(args.len() + 1);
                                    }
                                    next = Next::Call {
                                        base,
                                        argc: args.len(),
                                        resume,
                                    };
                                    continue;
                                }
                                if resume == ENTRY {
                                    self.release_slots(fp, base + 3, base + 3 + frame_slots);
                                    return Ok(result);
                                }
                                if resume & CATCH != 0 {
                                    self.catch_returned();
                                }
                                // The arguments are dead.
                                self.release_slots(fp, base + 4, base + 4 + argc);
                                self.store(fp, base + 3, result);
                                pc = (resume & !CATCH) as usize;
                            }
                            Kind::Bytecode { slots, .. } => {
                                let needed = usize::from(slots).max(argc + 1);
                                let mut callee_fp = fp.sub(base + 3);
                                let mut position = if resume == ENTRY {
                                    ENTRY
                                } else {
                                    position(resume, base)
                                };
                                if !fits(callee_fp, needed, limit) {
                                    // Chain a segment, and move the
                                    // procedure and arguments into it.
                                    let (top, new_limit) = attempt!(self.push_segment(needed + 4));
                                    // The bottom frame keeps the host's
                                    // stack pointer in the word above its
                                    // header.
                                    let new_fp = top.sub(if position == ENTRY { 4 } else { 3 });
                                    for i in 0..=argc {
                                        // Moved, so counted as they were.
                                        set(new_fp, i, slot(callee_fp, i));
                                        if S::PRECISE {
                                            set(callee_fp, i, Value::UNSPECIFIED);
                                        }
                                    }
                                    callee_fp = new_fp;
                                    limit = new_limit;
                                    if position != ENTRY {
                                        position |= BOUNDARY;
                                    }
                                }
                                if position == ENTRY {
                                    *callee_fp = link.host_fp;
                                    *callee_fp.add(1) = link.host_return;
                                    *callee_fp.add(3) = link.host_cfa;
                                } else {
                                    if S::PRECISE && callee_fp == fp.sub(base + 3) {
                                        // The header goes in the caller's
                                        // slots, which may hold values.
                                        self.release_slots(fp, base, base + 3);
                                    }
                                    *callee_fp = fp as u64;
                                    *callee_fp.add(1) = current_stub;
                                }
                                *callee_fp.add(2) = position;
                                if let Err(e) = self.enter(callee, callee_fp, argc) {
                                    if position != ENTRY && position & BOUNDARY != 0 {
                                        // The error is raised in the
                                        // caller: drop the new segment.
                                        self.release_slots(callee_fp, 0, needed);
                                        limit = self.pop_segment();
                                    }
                                    fail!(e);
                                }
                                if S::PRECISE {
                                    frame_slots = needed;
                                }
                                fp = callee_fp;
                                if resume != ENTRY && resume & CAPTURE != 0 {
                                    // Seal the stack from the caller out,
                                    // keeping the receiver's segment if it
                                    // got one of its own.
                                    let caller = *fp as *mut u64;
                                    let k = attempt!(self.capture(
                                        caller,
                                        (position & !BOUNDARY) | UNDERFLOW,
                                        true,
                                        caller.sub(base),
                                        usize::from(position & BOUNDARY != 0),
                                    ));
                                    position |= UNDERFLOW;
                                    *fp.add(2) = position;
                                    self.store(fp, 1, k);
                                }
                                link_to!(fp, stub_of!(position, callee));
                                pc = 0;
                                switch_to!(callee);
                            }
                        }
                    }
                    Next::TailCall { argc } => {
                        let callee = attempt!(self.procedure_code(slot(fp, 0)));
                        attempt!(control_arity(callee, argc));
                        match callee.kind {
                            Kind::Catch => {
                                // As a tail call, the handler takes this
                                // frame's place in its caller, and the thunk
                                // runs in this frame.
                                let position = *fp.add(2);
                                if position == ENTRY {
                                    fail!(Error::scheme("%catch: cannot be called from Rust"));
                                }
                                let segments = if position & BOUNDARY != 0 {
                                    self.stack.len() - 1
                                } else {
                                    self.stack.len()
                                };
                                // A sealed caller is held by the
                                // continuation below the stack.
                                let record = if position & UNDERFLOW != 0 {
                                    self.below
                                } else {
                                    Value::FALSE
                                };
                                self.push_catch(Catch {
                                    fp: *fp as *mut u64,
                                    base: position_base(position),
                                    resume: position_resume(position),
                                    segments,
                                    handler: slot(fp, 2),
                                    record,
                                });
                                self.store(fp, 0, slot(fp, 1));
                                *fp.add(2) = position | CATCH;
                                next = Next::TailCall { argc: 0 };
                                continue;
                            }
                            Kind::Capture => {
                                // The continuation is this frame's: what
                                // its caller does with its result. The
                                // receiver runs in this frame.
                                let receiver = slot(fp, 1);
                                attempt!(self.receiver(receiver, argc));
                                let position = *fp.add(2);
                                let k = if position == ENTRY || position & UNDERFLOW != 0 {
                                    // Its caller is the host, or sealed
                                    // already.
                                    attempt!(self.capture(*fp as *mut u64, position, false, fp, 0))
                                } else {
                                    let position = (position & !BOUNDARY) | UNDERFLOW;
                                    let k = attempt!(self.capture(
                                        *fp as *mut u64,
                                        position,
                                        true,
                                        fp.add(3),
                                        0
                                    ));
                                    *fp.add(2) = position;
                                    k
                                };
                                self.store(fp, 0, receiver);
                                self.store(fp, 1, k);
                                next = Next::TailCall { argc: 1 };
                                continue;
                            }
                            Kind::Resume => {
                                let (k, value) = (slot(fp, 1), slot(fp, 2));
                                if argc != 2 || Self::sealed(k).is_none() {
                                    fail!(wrong_type(self, "%continue", "a continuation", k));
                                }
                                if S::PRECISE {
                                    S::retain(self.heap_mut(), k);
                                    S::retain(self.heap_mut(), value);
                                }
                                self.discard(fp, frame_slots, top);
                                next = self.resume_into(k, value);
                                if let Next::Return(value) = next {
                                    return Ok(value);
                                }
                                continue;
                            }
                            Kind::Native(_) | Kind::Bridge(_) => {
                                let result = attempt!(self.call_native(callee, fp, 1, argc));
                                if let Some((procedure, args)) = self.tail_call.take() {
                                    if !fits(fp, args.len() + 2, limit) {
                                        fail!(Error::scheme(
                                            "too many arguments to fit in this stack segment"
                                        ));
                                    }
                                    self.store(fp, 0, procedure);
                                    for (i, &arg) in args.iter().enumerate() {
                                        self.store(fp, 1 + i, arg);
                                    }
                                    if S::PRECISE {
                                        frame_slots = frame_slots.max(args.len() + 1);
                                    }
                                    next = Next::TailCall { argc: args.len() };
                                    continue;
                                }
                                next = Next::Return(result);
                                continue;
                            }
                            Kind::Bytecode { slots, .. } => {
                                let needed = usize::from(slots).max(argc + 1);
                                if !fits(fp, needed, limit) {
                                    // Move this frame to a new segment. If it
                                    // was the first in its segment, that
                                    // segment is left empty, so drop it first.
                                    let saved = *fp;
                                    let return_to = *fp.add(1);
                                    let old = *fp.add(2);
                                    // The procedure and arguments move, so
                                    // counted as they were; the rest of the
                                    // old frame is dead.
                                    self.release_slots(fp, argc + 1, frame_slots);
                                    let moved: Vec<Value> =
                                        (0..=argc).map(|i| slot(fp, i)).collect();
                                    if S::PRECISE {
                                        for i in 0..=argc {
                                            set(fp, i, Value::UNSPECIFIED);
                                        }
                                    }
                                    let position = if old == ENTRY {
                                        ENTRY
                                    } else if old & BOUNDARY != 0 {
                                        self.pop_segment();
                                        old
                                    } else if old & UNDERFLOW != 0 {
                                        // The first frame of the segment
                                        // under the live stack, left empty.
                                        old | BOUNDARY
                                    } else {
                                        let base = (saved as usize - fp as usize) / 8 - 3;
                                        position(position_resume(old), base)
                                            | BOUNDARY
                                            | (old & CATCH)
                                    };
                                    let (top, new_limit) = attempt!(self.push_segment(needed + 4));
                                    fp = top.sub(if position == ENTRY { 4 } else { 3 });
                                    limit = new_limit;
                                    *fp = saved;
                                    *fp.add(1) = return_to;
                                    *fp.add(2) = position;
                                    if position == ENTRY {
                                        *fp.add(3) = link.host_cfa;
                                    }
                                    for (i, &v) in moved.iter().enumerate() {
                                        set(fp, i, v);
                                    }
                                    if cfg!(feature = "native-frames") {
                                        *link.record = fp as u64;
                                    }
                                }
                                attempt!(self.enter(callee, fp, argc));
                                if S::PRECISE {
                                    // What the old function's frame had
                                    // past the new one's is dead.
                                    self.release_slots(fp, needed, frame_slots);
                                    frame_slots = needed;
                                }
                                // A different function now runs in this frame.
                                link_to!(fp, stub_of!(*fp.add(2), callee));
                                pc = 0;
                                switch_to!(callee);
                            }
                        }
                    }
                    Next::Return(value) => {
                        let position = *fp.add(2);
                        // The returning frame is dead: its value is counted
                        // out with it, and back in where it is stored.
                        self.release_slots(fp, 0, frame_slots);
                        if position == ENTRY {
                            return Ok(value);
                        }
                        let caller = *fp as *mut u64;
                        // One test for the rare cases.
                        if position & (CATCH | UNDERFLOW | RAISED) != 0 {
                            if position & CATCH != 0 {
                                self.catch_returned();
                            }
                            if position & RAISED != 0 {
                                next = Next::Raise(self.handler_returned());
                                continue;
                            }
                            if position & UNDERFLOW != 0 {
                                if position & BOUNDARY != 0 {
                                    self.pop_segment();
                                }
                                next = Next::Underflow {
                                    caller,
                                    position,
                                    value,
                                };
                                continue;
                            }
                        }
                        let base = position_base(position);
                        self.store(caller, base + 3, value);
                        clear_header(caller, base);
                        if position & BOUNDARY != 0 {
                            limit = self.pop_segment();
                        }
                        fp = caller;
                        pc = position_resume(position) as usize;
                        let caller = attempt!(self.procedure_code(slot(fp, 0)));
                        if S::PRECISE
                            && let Kind::Bytecode { slots, .. } = caller.kind
                        {
                            frame_slots = usize::from(slots);
                        }
                        link_to!(fp, stub_of!(*fp.add(2), caller));
                        switch_to!(caller);
                    }
                }

                // The dispatch loop, until a call or return.
                next = loop {
                    let word = *code.add(pc);
                    let (a, b, c) = fields(word);
                    let (a, b, c) = (usize::from(a), usize::from(b), usize::from(c));
                    macro_rules! branch {
                        ($condition:expr) => {{
                            if $condition {
                                pc += 2;
                            } else {
                                let offset = *code.add(pc + 1) as i32;
                                pc = (pc + 2).wrapping_add_signed(offset as isize);
                            }
                        }};
                    }
                    macro_rules! compare {
                        ($op:expr, $x:expr, $y:expr, $fast:tt) => {{
                            let (x, y) = ($x, $y);
                            if x.tag() == Tag::Fixnum && y.tag() == Tag::Fixnum {
                                (x.raw() as i64) $fast (y.raw() as i64)
                            } else {
                                attempt!(self.compare($op, x, y))
                            }
                        }};
                    }
                    macro_rules! arith {
                        ($op:expr, $x:expr, $y:expr, $fast:expr) => {{
                            let (x, y): (Value, Value) = ($x, $y);
                            let fast: Option<i64> =
                                if x.tag() == Tag::Fixnum && y.tag() == Tag::Fixnum {
                                    $fast(x.raw() as i64, y.raw() as i64)
                                } else {
                                    None
                                };
                            match fast {
                                Some(word) => Value::from_raw(word as u64),
                                None => attempt!(self.arith($op, x, y)),
                            }
                        }};
                    }
                    let Some(op) = Opcode::from_u8(word as u8) else {
                        unreachable!("validated code has only known opcodes");
                    };
                    match op {
                        Opcode::Mov => {
                            self.store(fp, a, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::LoadConstant => {
                            self.store(fp, a, *constants.add(usize::from(field_d(word))));
                            pc += 1;
                        }
                        Opcode::LoadInt => {
                            let n = field_d(word) as i16;
                            self.store(fp, a, Value::fixnum(i64::from(n)).expect("16 bits fit"));
                            pc += 1;
                        }
                        Opcode::GetVariable => {
                            let index = usize::from(field_d(word));
                            let value = *(*variables.add(index)).0;
                            if value == Value::UNBOUND {
                                let closure = slot(fp, 0);
                                let Kind::Bytecode { module, .. } =
                                    attempt!(self.procedure_code(closure)).kind
                                else {
                                    unreachable!()
                                };
                                let loaded: &Loaded<S> = &*module;
                                let name = &loaded.module.variables[index].name;
                                // A REPL global is named by its REPL name.
                                let name = name.strip_prefix("repl:").unwrap_or(name);
                                fail!(Error::scheme(format!("unbound variable: {name}")));
                            }
                            self.store(fp, a, value);
                            pc += 1;
                        }
                        Opcode::SetVariable => {
                            self.write_cell(
                                (*variables.add(usize::from(field_d(word)))).0,
                                slot(fp, a),
                            );
                            pc += 1;
                        }
                        Opcode::Closure => {
                            let function = *code.add(pc + 1) as usize;
                            let count = b;
                            let free: Vec<Value> = (0..count)
                                .map(|i| {
                                    let w = *code.add(pc + 2 + i / 4);
                                    slot(fp, usize::from((w >> (8 * (i % 4))) as u8))
                                })
                                .collect();
                            let current = attempt!(self.procedure_code(slot(fp, 0)));
                            let Kind::Bytecode { module, .. } = current.kind else {
                                unreachable!()
                            };
                            let loaded: &Loaded<S> = &*module;
                            let target = &loaded.functions[function] as *const Code<S>;
                            let closure = attempt!(self.closure_raw(target, &free));
                            self.store(fp, a, closure);
                            pc += 2 + count.div_ceil(4);
                        }
                        Opcode::Free => {
                            let closure = slot(fp, 0);
                            self.store(
                                fp,
                                a,
                                Value::from_raw(get(closure, Self::LAYOUT.closure_free(b))),
                            );
                            pc += 1;
                        }
                        Opcode::MakeBox => {
                            let cell = attempt!(self.new_object(TypeCode::Box, 1, 0));
                            self.init_field(
                                cell,
                                Self::LAYOUT.field(TypeCode::Box, 0),
                                slot(fp, b),
                            );
                            self.store(fp, a, cell);
                            pc += 1;
                        }
                        Opcode::Unbox => {
                            let cell = slot(fp, b);
                            if !self.is_other(cell, TypeCode::Box) {
                                fail!(type_error("unbox", "a box", cell));
                            }
                            self.store(
                                fp,
                                a,
                                Value::from_raw(get(cell, Self::LAYOUT.field(TypeCode::Box, 0))),
                            );
                            pc += 1;
                        }
                        Opcode::SetBox => {
                            let cell = slot(fp, a);
                            if !self.is_other(cell, TypeCode::Box) {
                                fail!(type_error("set-box", "a box", cell));
                            }
                            self.write_field(
                                cell,
                                Self::LAYOUT.field(TypeCode::Box, 0),
                                slot(fp, b),
                            );
                            pc += 1;
                        }
                        Opcode::Jump => {
                            let offset = field_e_signed(word);
                            // A loop's backward jump is a safepoint.
                            if S::COUNTS && offset < 0 && S::epoch_due(self.heap()) {
                                let roots = self.stack_roots(fp);
                                self.epoch(&roots);
                            }
                            pc = (pc + 1).wrapping_add_signed(offset as isize);
                        }
                        Opcode::If => branch!(slot(fp, a).is_true()),
                        Opcode::IfEq => branch!(slot(fp, a) == slot(fp, b)),
                        Opcode::IfLt => branch!(compare!(Compare::Lt, slot(fp, a), slot(fp, b), <)),
                        Opcode::IfLe => {
                            branch!(compare!(Compare::Le, slot(fp, a), slot(fp, b), <=))
                        }
                        Opcode::IfNumEq => {
                            branch!(compare!(Compare::Eq, slot(fp, a), slot(fp, b), ==))
                        }
                        Opcode::IfLtImm => {
                            let imm = Value::fixnum(i64::from(b as u8 as i8)).expect("fits");
                            branch!(compare!(Compare::Lt, slot(fp, a), imm, <))
                        }
                        Opcode::IfNull => branch!(slot(fp, a) == Value::EMPTY_LIST),
                        Opcode::IfPair => branch!(slot(fp, a).is_pair()),
                        Opcode::Call => {
                            break Next::Call {
                                base: a,
                                argc: b,
                                resume: (pc + 1) as u64,
                            };
                        }
                        Opcode::TailCall => {
                            for i in 0..=b {
                                self.store(fp, i, slot(fp, a + i));
                            }
                            break Next::TailCall { argc: b };
                        }
                        Opcode::Return => break Next::Return(slot(fp, a)),
                        Opcode::Add => {
                            let v = arith!(Arith::Add, slot(fp, b), slot(fp, c), i64::checked_add);
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Sub => {
                            let v = arith!(Arith::Sub, slot(fp, b), slot(fp, c), i64::checked_sub);
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Mul => {
                            let v =
                                arith!(Arith::Mul, slot(fp, b), slot(fp, c), |x: i64, y: i64| {
                                    (x >> 3).checked_mul(y)
                                });
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::AddImm => {
                            let imm = Value::fixnum(i64::from(c as u8 as i8)).expect("fits");
                            let v = arith!(Arith::Add, slot(fp, b), imm, i64::checked_add);
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::SubImm => {
                            let imm = Value::fixnum(i64::from(c as u8 as i8)).expect("fits");
                            let v = arith!(Arith::Sub, slot(fp, b), imm, i64::checked_sub);
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Lt => {
                            let r = compare!(Compare::Lt, slot(fp, b), slot(fp, c), <);
                            self.store(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::Le => {
                            let r = compare!(Compare::Le, slot(fp, b), slot(fp, c), <=);
                            self.store(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::NumEq => {
                            let r = compare!(Compare::Eq, slot(fp, b), slot(fp, c), ==);
                            self.store(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::Eq => {
                            self.store(fp, a, Value::boolean(slot(fp, b) == slot(fp, c)));
                            pc += 1;
                        }
                        Opcode::Not => {
                            self.store(fp, a, Value::boolean(!slot(fp, b).is_true()));
                            pc += 1;
                        }
                        Opcode::Cons => {
                            let pair = attempt!(self.cons_raw(slot(fp, b), slot(fp, c)));
                            self.store(fp, a, pair);
                            pc += 1;
                        }
                        Opcode::Car => {
                            let pair = slot(fp, b);
                            if !pair.is_pair() {
                                fail!(type_error("car", "a pair", pair));
                            }
                            self.store(fp, a, self.car(pair));
                            pc += 1;
                        }
                        Opcode::Cdr => {
                            let pair = slot(fp, b);
                            if !pair.is_pair() {
                                fail!(type_error("cdr", "a pair", pair));
                            }
                            self.store(fp, a, self.cdr(pair));
                            pc += 1;
                        }
                        Opcode::SetCar => {
                            let pair = slot(fp, a);
                            if !pair.is_pair() {
                                fail!(type_error("set-car!", "a pair", pair));
                            }
                            self.set_car(pair, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::SetCdr => {
                            let pair = slot(fp, a);
                            if !pair.is_pair() {
                                fail!(type_error("set-cdr!", "a pair", pair));
                            }
                            self.set_cdr(pair, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::IsNull => {
                            self.store(fp, a, Value::boolean(slot(fp, b) == Value::EMPTY_LIST));
                            pc += 1;
                        }
                        Opcode::IsPair => {
                            self.store(fp, a, Value::boolean(slot(fp, b).is_pair()));
                            pc += 1;
                        }
                        Opcode::MakeVector => {
                            let len = slot(fp, b);
                            let Some(n) = len.as_fixnum().filter(|n| *n >= 0) else {
                                fail!(type_error("make-vector", "a non-negative fixnum", len));
                            };
                            let v = attempt!(self.make_vector_raw(n as usize, slot(fp, c)));
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::VectorRef | Opcode::VectorSet => {
                            let (vector, index) = if op == Opcode::VectorRef {
                                (slot(fp, b), slot(fp, c))
                            } else {
                                (slot(fp, a), slot(fp, b))
                            };
                            let name = if op == Opcode::VectorRef {
                                "vector-ref"
                            } else {
                                "vector-set!"
                            };
                            if !self.is_other(vector, TypeCode::Vector) {
                                fail!(type_error(name, "a vector", vector));
                            }
                            let len = self.vector_length(vector);
                            let Some(i) = index.as_fixnum().filter(|i| (0..len as i64).contains(i))
                            else {
                                fail!(Error::scheme(format!(
                                    "{name}: index {index:?} is out of range for a vector of length {len}"
                                )));
                            };
                            if op == Opcode::VectorRef {
                                self.store(fp, a, self.vector_ref(vector, i as usize));
                            } else {
                                self.vector_set(vector, i as usize, slot(fp, c));
                            }
                            pc += 1;
                        }
                        Opcode::VectorLength => {
                            let vector = slot(fp, b);
                            if !self.is_other(vector, TypeCode::Vector) {
                                fail!(type_error("vector-length", "a vector", vector));
                            }
                            let len = self.vector_length(vector) as i64;
                            self.store(fp, a, Value::fixnum(len).expect("lengths fit"));
                            pc += 1;
                        }
                        Opcode::SetFree => {
                            let closure = slot(fp, a);
                            let count = if closure.tag() == Tag::Closure {
                                self.header(closure, TypeCode::Closure)
                                    .map_or(0, |h| h.data() as usize)
                            } else {
                                fail!(type_error("set-free", "a closure", closure));
                            };
                            if b >= count {
                                fail!(Error::scheme(format!(
                                    "set-free: the closure has {count} free variables, not {}",
                                    b + 1
                                )));
                            }
                            // `letrec` patches closures its own module made;
                            // another module's closures are not this code's
                            // to change.
                            let own = matches!(
                                self.procedure_code(closure).map(|c| &c.kind),
                                Ok(Kind::Bytecode { module, .. }) if *module as usize == current_module
                            );
                            if !own {
                                fail!(Error::scheme(
                                    "set-free: the closure belongs to another module"
                                ));
                            }
                            self.write_field(closure, Self::LAYOUT.closure_free(b), slot(fp, c));
                            pc += 1;
                        }
                    }
                };
            }
        }
    }
}

impl From<SchemeError> for Error {
    fn from(e: SchemeError) -> Error {
        Error::Scheme(Box::new(e))
    }
}

impl Handle {
    /// The exact integer, if this is a fixnum or a bignum.
    pub fn to_integer<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<Integer, Error> {
        let v = cx.get(self)?;
        if let Some(n) = v.as_fixnum() {
            return Ok(Integer::from(n));
        }
        if cx.is_other(v, TypeCode::Bignum) {
            // SAFETY: `v` is a live bignum.
            return Ok(unsafe { cx.bignum_value(v) });
        }
        Err(Error::WrongType {
            expected: "an exact integer",
        })
    }
}
