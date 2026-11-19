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

use std::sync::atomic::Ordering;

use std::cell::UnsafeCell;
use vali_abi::{Header, Tag, TypeCode, data};

use vali_bytecode::{
    Arity, Constant, Converted, Converter, LazyModule, Link as VariableLink, Module, Opcode,
    field_d, field_e_signed, fields, validate, validate_lazy,
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
pub(crate) const BOUNDARY: u64 = 1 << 63;

/// Set in the position word of the frame `%catch` calls its thunk in, and
/// in the resume word of a native thunk's call: returning through it
/// removes the innermost catch.
const CATCH: u64 = 1 << 62;

/// Set in a frame's position word when its caller is sealed in a
/// continuation (`continuations`): returning from it copies the caller into
/// the live stack first. Such a frame is the outermost live one, and the
/// first in its segment.
pub(crate) const UNDERFLOW: u64 = 1 << 61;

/// Where a position word keeps the resume address, and how many bits it
/// has; the debugger extensions read these (`debug`).
pub(crate) const POSITION_RESUME_SHIFT: u32 = 3;
pub(crate) const POSITION_RESUME_BITS: u32 = 32;

/// Where a position word keeps the call's `base`, and how many bits it has.
pub(crate) const POSITION_BASE_SHIFT: u32 = 35;
pub(crate) const POSITION_BASE_BITS: u32 = 16;

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
    ((resume & ((1 << POSITION_RESUME_BITS) - 1)) << POSITION_RESUME_SHIFT)
        | (base as u64) << POSITION_BASE_SHIFT
        | (resume & (CATCH | BOUNDARY | UNDERFLOW))
}

/// The `base` a position word records.
pub(crate) fn position_base(position: u64) -> usize {
    ((position >> POSITION_BASE_SHIFT) & ((1 << POSITION_BASE_BITS) - 1)) as usize
}

/// Where the caller resumes, from a position word.
pub(crate) fn position_resume(position: u64) -> u64 {
    (position >> POSITION_RESUME_SHIFT) & ((1 << POSITION_RESUME_BITS) - 1)
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
    // Tell debuggers this thread runs Scheme code now, and not any more
    // once it stops, even by a panic (`interpret` keeps `fp` and `pc` there
    // current from then on).
    let debug = cx.debug.state();
    // SAFETY: the record lives as long as `cx`, which outlives this call.
    unsafe {
        (*debug)
            .thread
            .store(crate::debug::thread_id(), Ordering::Relaxed);
        (*debug).pc.store(0, Ordering::Relaxed);
        (*debug).fp.store(entry as u64, Ordering::Relaxed);
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `execute` set up the entry frame.
        unsafe { cx.interpret(&link, entry, limit, argc) }
    }));
    // SAFETY: as above.
    unsafe { (*debug).fp.store(0, Ordering::Relaxed) };
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
pub(crate) fn constant_number(constant: &Constant) -> Option<vali_num::Number> {
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

/// The error of calling the Scheme function `code` with `argc` arguments it
/// does not take.
fn arity_error<S: Strategy>(code: &Code<S>, argc: usize) -> Error {
    let arity = code.arity;
    Error::scheme(format!(
        "{}: wrong number of arguments: expected {}{}{}, got {argc}",
        code.name,
        arity.required,
        if arity.optional > 0 {
            format!(" to {}", arity.required + arity.optional)
        } else {
            String::new()
        },
        if arity.rest { " or more" } else { "" },
    ))
}

/// An error if `code` is `%catch`, `%call/cc` or `%continue` and does
/// not take `argc` arguments. Function entry checks every other kind; these
/// read their arguments straight from the call window, so a call with too
/// few would read past it.
fn control_arity<S: Strategy>(code: &Code<S>, argc: usize) -> Result<(), Error> {
    if matches!(*code.kind(), Kind::Catch | Kind::Capture | Kind::Resume)
        && !code.arity.accepts(argc)
    {
        return Err(Error::scheme(format!(
            "{}: wrong number of arguments: got {argc}",
            code.name
        )));
    }
    Ok(())
}

/// The backtrace frames of `code` stopped at instruction `pc`, innermost
/// first: one for each inlined copy the instruction is in, at the
/// instruction or the call the next copy in replaced, then `code`'s own.
fn frames_of<S: Strategy>(code: &Code<S>, pc: usize) -> Vec<crate::handle::Frame> {
    let frame = |name: &str, position| crate::handle::Frame {
        name: name.to_owned(),
        position,
    };
    let Kind::Bytecode { module, index, .. } = *code.kind() else {
        return vec![frame(code.name(), None)];
    };
    // SAFETY: a function's descriptor points to the module that owns it,
    // which outlives it.
    let loaded = unsafe { &*module };
    // The code that runs: under precise counting, `pc` is the rewritten
    // code's.
    let Some(function) = loaded.function(index as usize) else {
        return vec![frame(code.name(), None)];
    };
    let located = |source: u16, line: u32, column: u32| {
        Some(crate::handle::Position {
            source: loaded.sources.get(usize::from(source))?.clone(),
            line,
            column,
        })
    };
    let mut position = function
        .positions
        .partition_point(|p| p.pc as usize <= pc)
        .checked_sub(1)
        .and_then(|at| {
            let p = function.positions[at];
            located(p.source, p.line, p.column)
        });
    let mut scope = function
        .scopes
        .partition_point(|s| s.pc as usize <= pc)
        .checked_sub(1)
        .map_or(0, |at| function.scopes[at].inlined);
    let mut frames = Vec::new();
    // The validator checked that each copy's caller comes before it.
    while let Some(copy) = scope
        .checked_sub(1)
        .and_then(|i| function.inlined.get(i as usize))
    {
        frames.push(frame(copy.name.as_deref().unwrap_or("(inlined)"), position));
        position = copy
            .call
            .and_then(|(s, line, column)| located(s, line, column));
        scope = copy.caller;
    }
    frames.push(frame(code.name(), position));
    frames
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
        /// Whether the procedure is known to be a closure of a bytecode
        /// function that takes `argc` arguments: a direct call's.
        direct: bool,
    },
    TailCall {
        argc: usize,
        /// As for [`Next::Call`].
        direct: bool,
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
    /// built into the pool, outside the heap (ADR 0020), and its variables
    /// are linked (ADR 0005 #4). A
    /// variable it defines gets a cell of its own, unbound until the body
    /// sets it; an imported one shares the cell of the loaded library that
    /// exports it; a global one shares the global's cell, which a host
    /// defines, before or after.
    pub fn load(&mut self, module: Module) -> Result<ModuleId, Error> {
        self.load_module(module, None, false, false).map(ModuleId)
    }

    /// As [`load`](Cx::load), for a module that is a library, which
    /// [`is_loaded_from`](Cx::is_loaded_from) can later be asked about: it
    /// takes a digest of the module, which costs about a quarter of loading
    /// it.
    pub fn load_library(&mut self, module: Module) -> Result<ModuleId, Error> {
        self.load_module(module, None, false, true).map(ModuleId)
    }

    /// As [`load_library`](Cx::load_library), for a module that converts
    /// its functions as they are first called (ADR 0017 #2): only its body
    /// is converted and validated now. A function's first call converts,
    /// checks and, under precise counting, rewrites it, then runs it; the
    /// error of one that cannot be converted, which only a converter bug
    /// causes, is raised there, naming the function. A debugger sees a
    /// function not called yet with no frame size and no line table.
    pub fn load_lazy(&mut self, lazy: LazyModule) -> Result<ModuleId, Error> {
        let LazyModule { module, converter } = lazy;
        self.load_module(module, Some(converter), false, true)
            .map(ModuleId)
    }

    /// [`load_library`](Cx::load_library) or [`load_lazy`](Cx::load_lazy),
    /// whichever `library` is.
    pub fn load_converted(&mut self, library: Converted) -> Result<ModuleId, Error> {
        match library {
            Converted::Eager(module) => self.load_library(module),
            Converted::Lazy(lazy) => self.load_lazy(lazy),
        }
    }

    /// The frame size and line table length a debugger would read for the
    /// Scheme procedure `procedure` (`DebugFunction`): both 0 until a lazy
    /// module's function is first called. For tests.
    #[doc(hidden)]
    pub fn debug_function(&self, procedure: &Handle) -> Option<(u32, usize)> {
        let value = self.get(procedure).ok()?;
        // SAFETY: a valid value.
        let code = unsafe { self.procedure_code(value) }.ok()?;
        // SAFETY: `debug` changes only as the function converts.
        let debug = unsafe { &*code.debug.get() };
        Some((debug.slots, debug.positions_len))
    }

    /// How many of the functions of the module `id` have been converted: all
    /// of an eager module's, and a lazy module's body and those called.
    pub fn converted_functions(&self, id: ModuleId) -> usize {
        self.instances[id.0].loaded.converted()
    }

    /// Loads `module`, which is freed once nothing refers to it, and
    /// returns its body as a procedure of no arguments, to call: evaluated
    /// code, which would otherwise stay loaded as long as the isolate does
    /// (#31).
    ///
    /// Nothing refers to the module when no closure of its functions is
    /// left, which covers the frames and continuations running in it, and
    /// the cells it defines hold no reference that keeps it: cycles through
    /// them are collected with the rest. The module is a heap object that
    /// each closure of its functions holds, as it holds its free variables
    /// (ADR 0005), so the strategy frees it and, with it, what it holds:
    /// its constants, which are counted objects built in the heap, and the
    /// values of its cells. The runtime drops its instance at the next load
    /// or return to the host after that.
    ///
    /// Under a strategy that never frees (`NoCollection`) nothing can say
    /// when nothing refers to a module, so this is `load` and `module_body`:
    /// the module stays loaded for the isolate's life.
    ///
    /// A module that calls functions directly (`call-direct`) is refused:
    /// the closure a direct call gives its callee is kept with the module's
    /// instance, so it would refer to its own module for ever.
    pub fn load_transient(&mut self, module: Module) -> Result<Handle, Error> {
        if !S::COUNTS {
            let id = self.load(module)?;
            return self.module_body(id);
        }
        for function in &module.functions {
            let mut pc = 0;
            while pc < function.code.len() {
                let Ok((instruction, len)) = vali_bytecode::Instruction::decode(&function.code, pc)
                else {
                    break;
                };
                if matches!(
                    instruction.opcode(),
                    Opcode::CallDirect | Opcode::TailCallDirect
                ) {
                    return Err(Error::scheme(format!(
                        "{}: direct calls are not supported in a module that is freed once nothing refers to it",
                        module.name
                    )));
                }
                pc += len;
            }
        }
        // What the module's instance, descriptors and code take outside the
        // heap, counted as allocated, so that epochs come as often as they
        // would if the heap held it.
        let rust_bytes = 1024
            + module
                .functions
                .iter()
                .map(|f| 4 * f.code.len() + 64)
                .sum::<usize>();
        let id = self.load_module(module, None, true, false)?;
        S::charge(self.heap_mut(), rust_bytes);
        let instance = &self.instances[id];
        let body = &instance.loaded.functions[instance.loaded.body as usize] as *const Code<S>;
        let owner = instance.object;
        let closure = self.closure_owned(body, &[], owner)?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(closure) })
    }

    /// Loads `module`: for a module freed once nothing refers to it, its
    /// constants are built in the heap and its object made.
    fn load_module(
        &mut self,
        module: Module,
        converter: Option<Converter>,
        transient: bool,
        library: bool,
    ) -> Result<usize, Error> {
        // Nothing runs code of a retired instance here: it was running
        // frames or holding closures only until it was freed.
        self.instances.empty_graveyard();
        // A lazy module's other functions are checked as they are converted.
        let valid = if converter.is_some() {
            validate_lazy(&module)
        } else {
            validate(&module)
        };
        valid.map_err(|e| Error::scheme(format!("invalid bytecode in {}: {e}", module.name)))?;
        // Modules link to the primitives as they load, so the first load
        // installs them.
        self.install_primitives()?;
        let name = module.name.clone();
        let (loaded, trees) = Loaded::<S>::new(module, converter, transient, library)
            .map_err(|e| Error::scheme(format!("invalid bytecode in {name}: {e}")))?;
        let mut constants = Vec::with_capacity(trees.len());
        for constant in &trees {
            if transient {
                let value = self.heap_constant(constant);
                let value = match value {
                    Ok(value) => value,
                    Err(e) => {
                        self.release_all(&constants);
                        return Err(e);
                    }
                };
                // Held by the module's object, which releases it as it is
                // freed.
                self.hold(value);
                constants.push(value);
            } else {
                constants.push(self.pool_constant(constant)?);
            }
        }
        // Built: the trees go, leaving the instance the values alone.
        drop(trees);
        let defined = loaded
            .variables
            .iter()
            .filter(|v| v.link == VariableLink::Defined)
            .count();
        let cells: Box<[UnsafeCell<Value>]> = (0..defined)
            .map(|_| UnsafeCell::new(Value::UNBOUND))
            .collect();
        let mut next = 0;
        let mut variables = Vec::with_capacity(loaded.variables.len());
        for variable in &loaded.variables {
            variables.push(match &variable.link {
                VariableLink::Defined => {
                    next += 1;
                    Cell(cells[next - 1].get())
                }
                VariableLink::Global => Cell(self.global_cell(&variable.name)),
                VariableLink::Primitive => Cell(self.primitive_cell(&variable.name)),
                VariableLink::Native(library) => match self.native_cell(library, &variable.name) {
                    Ok(cell) => Cell(cell),
                    Err(e) => {
                        self.release_all(&constants);
                        return Err(e);
                    }
                },
                VariableLink::Imported(library) => match self.exported(library, &variable.name) {
                    Ok(cell) => cell,
                    Err(e) => {
                        self.release_all(&constants);
                        return Err(e);
                    }
                },
            });
        }
        let statics = vec![Value::FALSE; loaded.functions.len()].into();
        let id = self.instances.push(|index| Instance {
            loaded,
            index,
            object: Value::FALSE,
            constants,
            statics,
            cells,
            variables: variables.into(),
        });
        if transient {
            let instance = &self.instances[id] as *const Instance<S>;
            let object = match self.new_object(TypeCode::Code, 1, 0) {
                Ok(object) => object,
                Err(e) => {
                    let constants = std::mem::take(&mut self.instances[id].constants);
                    self.release_all(&constants);
                    self.instances.retire(id);
                    return Err(e.into());
                }
            };
            // SAFETY: just allocated with its one field.
            unsafe {
                put(
                    object,
                    Self::LAYOUT.field(TypeCode::Code, 0),
                    instance as u64,
                )
            };
            self.instances[id].object = object;
        }
        Ok(id)
    }

    /// Drops the references `load_module` took to constants it has built.
    fn release_all(&mut self, values: &[Value]) {
        for &v in values {
            if v.is_pointer() {
                S::release(self.heap_mut(), v);
            }
        }
    }

    /// Makes the loaded module `module` the library with identity
    /// `identity`, which modules loaded later can import from.
    ///
    /// A library the compiler will link programs against must have been
    /// loaded with [`load_library`](Cx::load_library). One loaded with plain
    /// `load` has no digest, so [`is_loaded_from`](Cx::is_loaded_from) is
    /// false for it, and a later `vali_compiler` program importing it fails
    /// with "already loaded, compiled differently".
    pub fn register_library(&mut self, identity: &str, module: ModuleId) {
        self.libraries.insert(identity.into(), module);
    }

    /// The loaded library with identity `identity`, if there is one.
    pub fn library(&self, identity: &str) -> Option<ModuleId> {
        self.libraries.get(identity).copied()
    }

    /// Whether the loaded module `id` was loaded from `module`: the same
    /// code, constants and debug information. A loaded module keeps only
    /// what runs, so this compares digests (`Module::fingerprint`); a module
    /// not loaded with `load_library` has no digest and matches nothing.
    pub fn is_loaded_from(&self, id: ModuleId, module: &vali_bytecode::Module) -> bool {
        self.instances[id.0].loaded.is_loaded_from(module)
    }

    /// The cell the library `library` exports as `name`.
    fn exported(&self, library: &str, name: &str) -> Result<Cell, Error> {
        let Some(&ModuleId(id)) = self.libraries.get(library) else {
            return Err(Error::scheme(format!("library {library} is not loaded")));
        };
        let instance = &self.instances[id];
        let variable = instance
            .loaded
            .export(name)
            .ok_or_else(|| Error::scheme(format!("library {library} does not export {name}")))?;
        Ok(instance.variables[usize::from(variable)])
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
        let body = &instance.loaded.functions[instance.loaded.body as usize] as *const Code<S>;
        let closure = self.closure_raw(body, &[])?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(closure) })
    }

    /// Runs a loaded module's body, returning its result.
    pub fn run(&mut self, module: ModuleId) -> Result<Handle, Error> {
        let instance = &self.instances[module.0];
        let body = &instance.loaded.functions[instance.loaded.body as usize] as *const Code<S>;
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
        self.task
            .catches
            .iter()
            .flat_map(|c| [c.handler, c.record])
            .chain([self.task.below])
            .chain(self.task.dynamic)
    }

    /// The value of a loaded module's variable, if it has one.
    pub fn variable(&mut self, module: ModuleId, name: &str) -> Option<Handle> {
        let instance = &self.instances[module.0];
        let index = instance
            .loaded
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
        self.task.tail_call = Some((procedure, args));
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

    pub(crate) fn intern(&mut self, name: &str) -> Result<Value, Error> {
        self.intern_with(name, true)
    }

    /// The symbol named `name` for the runtime's own use, such as the names
    /// of its error-object type: not stopped by `max_symbols`, so that the
    /// error a cap raises can always be built. There are only a few.
    pub(crate) fn intern_internal(&mut self, name: &str) -> Result<Value, Error> {
        self.intern_with(name, false)
    }

    fn intern_with(&mut self, name: &str, limited: bool) -> Result<Value, Error> {
        if let Some(&v) = self.symbols.get(name) {
            return Ok(v);
        }
        if limited {
            self.reserve_name(name)?;
        }
        let made = self.new_symbol(name);
        if made.is_err() && limited {
            self.release_rust_bytes(crate::cx::name_entry_bytes(name));
        }
        made
    }

    fn new_symbol(&mut self, name: &str) -> Result<Value, Error> {
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

    /// A new closure of `code`, capturing `free`, of a module that is freed
    /// once nothing refers to it: its last field is the module's object,
    /// `owner`, which it keeps alive.
    fn closure_owned(
        &mut self,
        code: *const Code<S>,
        free: &[Value],
        owner: Value,
    ) -> Result<Value, OutOfMemory> {
        debug_assert!(
            free.len() < data::closure::OWNED as usize,
            "the count of free variables must not reach the owner flag"
        );
        let v = self.new_object(
            TypeCode::Closure,
            2 + free.len(),
            free.len() as u32 | data::closure::OWNED,
        )?;
        // SAFETY: the closure was just allocated with room for its code
        // pointer, free variables and owner.
        unsafe {
            put(v, Self::LAYOUT.closure_code(), code as u64);
            for (i, x) in free.iter().enumerate() {
                self.init_field(v, Self::LAYOUT.closure_free(i), *x);
            }
            self.init_field(v, Self::LAYOUT.closure_free(free.len()), owner);
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
        if let Some(catch) = self.task.catches.pop()
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
        self.task.catches.push(catch);
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

    /// Moves the value in slot `from` to slot `to` of the frame at `fp`,
    /// clearing `from`. Under precise counting the reference moves with it,
    /// and the value `to` held is counted out.
    ///
    /// # Safety
    ///
    /// As `store`, for both slots, which must differ.
    #[inline(always)]
    unsafe fn shift(&mut self, fp: *mut u64, from: usize, to: usize) {
        // SAFETY: passed on to the caller.
        unsafe {
            let v = Self::take(fp, from);
            let old = slot(fp, to);
            set(fp, to, v);
            S::release(self.heap_mut(), old);
        }
    }

    /// Takes the value in slot `i` of the frame at `fp`, clearing the slot:
    /// under precise counting, its reference is the caller's now.
    ///
    /// # Safety
    ///
    /// Slot `i` must be inside the stack.
    #[inline(always)]
    unsafe fn take(fp: *mut u64, i: usize) -> Value {
        // SAFETY: passed on to the caller.
        unsafe {
            let v = slot(fp, i);
            set(fp, i, Value::UNSPECIFIED);
            v
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
        match unsafe { self.procedure_code(slot(fp, 0)) }.map(|c| c.kind()) {
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
                if position & RAISED != 0 {
                    // The raise procedure's frame sits past all of the
                    // frame it was called in. That one may not be a
                    // bytecode function's: a Rust procedure that fails
                    // when tail-called keeps the frame it was called in,
                    // and its procedure and arguments, which
                    // `suspended_slots` cannot count.
                    slots = slots.max(position_base(position));
                }
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
                    Ok(self.condition_object_raw(e.kind, message, Value::EMPTY_LIST)?)
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
                    e.backtrace.extend(frames_of(code, pc));
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
        if let Some(what) = &self.poisoned {
            return Err(Error::Poisoned(what.clone()));
        }
        if self.running {
            return Err(Error::scheme(
                "calling Scheme from inside Scheme code is not supported yet",
            ));
        }
        if args.len() > usize::from(u8::MAX) {
            return Err(Error::scheme("too many arguments"));
        }
        if self.task.stack.is_empty() {
            // The entry frame, and 255 arguments after it.
            self.push_segment(5 + 255)?;
        }
        // Every host call starts in an empty dynamic environment, however
        // the last one ended.
        self.reset_dynamic();
        let segment = &self.task.stack[0];
        // The entry frame and its arguments: slots 0 to 2, and 3 to 258.
        crate::asan::unpoison_words(
            // SAFETY: the segment has room for 5 + 255 words.
            unsafe { segment.top().sub(5 + 255) },
            segment.top(),
        );
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
        // No frame runs now, so no register points into a module freed
        // while it did.
        self.instances.empty_graveyard();
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
        self.task.tail_call = None;
        while let Some(catch) = self.task.catches.pop() {
            if S::PRECISE {
                S::release(self.heap_mut(), catch.handler);
                S::release(self.heap_mut(), catch.record);
            }
        }
        self.set_below(Value::FALSE);
        self.reset_dynamic();
        // An error can leave segments chained; only the first is kept, and
        // only if it is whole, since the next run starts at its top.
        while self.task.stack.len() > 1 {
            self.pop_segment();
        }
        if !self.task.stack[0].is_whole() {
            self.drop_view();
        }
    }

    /// Chains a segment that holds at least `words` words, reusing the
    /// spare if it is big enough, and returns its top and limit.
    fn push_segment(&mut self, words: usize) -> Result<(*mut u64, *mut u64), Error> {
        let requested = words;
        let words = words.max(SEGMENT_WORDS);
        let segment = match self.task.spare.take() {
            Some(spare) if spare.words() >= words => spare,
            spare => {
                if let Some(spare) = spare {
                    self.task.stack_words -= spare.words();
                }
                if self.task.stack_words + words > self.max_stack_words {
                    return Err(Error::scheme(format!(
                        "stack overflow: the stack would exceed {} bytes",
                        self.max_stack_words * 8
                    )));
                }
                self.task.stack_words += words;
                Segment::new(words)
            }
        };
        let ends = (segment.top(), segment.limit());
        // Nothing may touch the segment but the frame about to go at its
        // top: `enter` unpoisons each frame as it is set up.
        crate::asan::poison_words(ends.1, ends.0);
        // SAFETY: `words` fit in the segment, which is at least this long.
        crate::asan::unpoison_words(unsafe { ends.0.sub(requested) }, ends.0);
        self.task.stack.push(View::whole(segment));
        Ok(ends)
    }

    /// Drops the current segment, keeping it as the spare if it can be
    /// reused, and returns the previous segment's limit.
    fn pop_segment(&mut self) -> *mut u64 {
        self.drop_view();
        self.task
            .stack
            .last()
            .expect("the first segment stays")
            .limit()
    }

    /// Drops the current segment from the stack, keeping it as the spare if
    /// it can be reused.
    fn drop_view(&mut self) {
        let view = self.task.stack.pop().expect("a segment to drop");
        let words = view.words();
        match view.reclaim() {
            Some(segment) => {
                if let Some(old) = self.task.spare.replace(segment) {
                    self.task.stack_words -= old.words();
                }
            }
            None => self.task.stack_words -= words,
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
            let view = self.task.stack.last().expect("a segment");
            let (mut top, mut limit) = (view.top(), view.limit());
            let mut flags = 0;
            if !fits(top.sub(header), slots, limit) {
                (top, limit) = self.push_segment(slots + 4)?;
                flags = BOUNDARY;
            }
            let copy = top.sub(header);
            crate::asan::unpoison_words(copy.wrapping_sub(slots), copy.add(header));
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
                let parent = Self::sealed(self.task.below)
                    .expect("a continuation")
                    .parent;
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
                if code.is_scheme()
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
    /// the host. Under precise counting, `k` and `value` come counted in.
    /// `k` leaves counted out, and so does `value`, unless it goes on to a
    /// frame in [`Next::Underflow`], which takes its reference over.
    fn resume_into(&mut self, k: Value, value: Value) -> Next {
        let target = self.reinstate(k);
        if S::PRECISE {
            S::release(self.heap_mut(), k);
        }
        match target {
            None => {
                if S::PRECISE {
                    S::release(self.heap_mut(), value);
                }
                Next::Return(value)
            }
            Some((caller, position)) => {
                if position & CATCH != 0 {
                    self.catch_returned();
                }
                if position & RAISED != 0 {
                    if S::PRECISE {
                        S::release(self.heap_mut(), value);
                    }
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
        while self.task.stack.len() > 1 {
            self.drop_view();
        }
        self.task.stack[0].limit()
    }

    /// How many Scheme procedures have been entered so far: the fuel used
    /// (ADR 0009).
    pub fn fuel_used(&self) -> u64 {
        self.fuel_used
    }

    /// What the isolate has done since it was made or last reset, with the
    /// `counters` feature; `None` without it.
    pub fn counters(&self) -> Option<crate::Counters> {
        #[cfg(feature = "counters")]
        return Some(self.counters);
        #[cfg(not(feature = "counters"))]
        None
    }

    /// Starts the counts again from zero.
    pub fn reset_counters(&mut self) {
        #[cfg(feature = "counters")]
        {
            self.counters = crate::Counters::default();
        }
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

    /// The lowest word any frame uses, from the frame at `fp` out to the
    /// outermost one in its segment. A frame's slots overlap its callers'
    /// where it was called, so a caller's own end may lie above a deeper
    /// one's: poisoning below it alone would poison slots of a frame
    /// farther out.
    ///
    /// # Safety
    ///
    /// `fp` must be a live frame.
    #[cfg(feature = "asan")]
    unsafe fn lowest_live(&self, mut fp: *mut u64) -> *mut u64 {
        let mut lowest = fp;
        // SAFETY: passed on to the caller; frames link to their callers.
        unsafe {
            loop {
                lowest = lowest.min(fp.wrapping_sub(self.suspended_slots(fp)));
                let position = *fp.add(2);
                if position == ENTRY || position & (BOUNDARY | UNDERFLOW) != 0 {
                    return lowest;
                }
                fp = *fp as *mut u64;
            }
        }
    }

    /// Debug builds only: checks that the frame at `fp` is intact where
    /// the interpreter is about to call or return from it, for
    /// the fuzzer's sake (`vali_test::fuzz`): bytecode that corrupts the
    /// stack without crashing would otherwise go unseen. Slot 0 must still
    /// hold a closure of `running`, the function whose code runs, and the
    /// frame must lie inside its segment, which ends at `limit`. The entry
    /// pseudo-frame at `top` has neither. A Rust procedure in slot 0 is
    /// let through: a tail call to one leaves it there for the return.
    ///
    /// # Panics
    ///
    /// If the frame is corrupt.
    #[cfg(debug_assertions)]
    fn check_frame(
        &self,
        fp: *mut u64,
        top: *mut u64,
        limit: *mut u64,
        running: *const Code<S>,
        at: &str,
    ) {
        if fp == top {
            return;
        }
        // SAFETY: slot 0 of a bytecode frame is inside it, and the
        // tag is checked before the closure is read.
        let code = unsafe { self.procedure_code(slot(fp, 0)) }
            .unwrap_or_else(|_| panic!("corrupt frame at {at}: slot 0 holds no closure"));
        let Kind::Bytecode { slots, .. } = *code.kind() else {
            // A tail call to a Rust procedure leaves it in slot 0 for the
            // return that follows.
            return;
        };
        assert!(
            std::ptr::eq(code, running),
            "corrupt frame at {at}: slot 0 holds a closure of {}, but {} is running",
            code.name(),
            // SAFETY: `running` is the code switched to for this frame.
            unsafe { &*running }.name()
        );
        assert!(
            fits(fp, usize::from(slots), limit),
            "corrupt frame at {at}: {} has {slots} slots, which cross the segment's limit",
            code.name()
        );
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
        let Kind::Bytecode { slots, .. } = *code.kind() else {
            unreachable!("only bytecode functions are entered");
        };
        let arity = code.arity;
        if !arity.accepts(argc) {
            return Err(arity_error(code, argc));
        }
        let slots = usize::from(slots);
        // The frame, and its header, may go where the stack has not been
        // since the last words there were poisoned.
        crate::asan::unpoison_words(fp.wrapping_sub(slots.max(argc + 1)), fp.wrapping_add(3));
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
                // The rest of the arguments are in the list now.
                self.release_slots(fp, fixed + 2, argc + 1);
            }
            // The locals: words of the caller's that hold no reference
            // (the ownership pass sees to that under precise counting), or
            // words outside every frame, which are cleared.
            for i in arity.parameter_slots() + 1..slots {
                debug_assert!(!S::PRECISE || !slot(fp, i).is_pointer());
                set(fp, i, Value::UNSPECIFIED);
            }
        }
        self.fuel_used += 1;
        #[cfg(feature = "counters")]
        {
            self.counters.calls += 1;
            self.counters.largest_frame = self.counters.largest_frame.max(slots as u64);
        }
        code.calls.fetch_add(1, Ordering::Relaxed);
        if self.fuel_limit.is_some_and(|limit| self.fuel_used > limit) {
            return Err(Error::OutOfFuel);
        }
        Ok(())
    }

    /// Converts `code` to bytecode if it is a function of a lazy module that
    /// has not been called (`Kind::Lazy`), charging the heap for what the
    /// code takes; for any other it does nothing. Called with the procedure
    /// it is about to call, so nothing refers to its `kind`.
    #[cold]
    #[inline(never)]
    fn convert_lazy(&mut self, code: &Code<S>) -> Result<(), Error> {
        let Kind::Lazy { module, index } = *code.kind() else {
            return Ok(());
        };
        // SAFETY: a function's descriptor points to the module that owns it,
        // which outlives it; the one reference to `kind` was copied from.
        let converted = unsafe { (*module).convert(index as usize) };
        match converted {
            Ok(bytes) => {
                S::charge(self.heap_mut(), bytes);
                Ok(())
            }
            Err(e) => Err(Error::scheme(format!(
                "{}: cannot convert to bytecode: {e}",
                code.name
            ))),
        }
    }

    /// Records where the interpreter is, for a debugger's extensions to read
    /// (`crate::debug`). Called only on the paths that leave the loop and
    /// can stop in native code, never in an instruction's fast path, so the
    /// loop keeps `fp` and `pc` in registers.
    #[inline]
    fn debug_at(&self, fp: *mut u64, pc: usize) {
        let debug = self.debug.state();
        // SAFETY: the record lives as long as `self`.
        unsafe {
            (*debug).fp.store(fp as u64, Ordering::Relaxed);
            (*debug).pc.store(pc as u64, Ordering::Relaxed);
        }
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
        #[cfg(feature = "counters")]
        {
            self.counters.native_calls += 1;
        }
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
        self.task.tail_call = None;
        let result = match code.kind() {
            // A primitive works on raw heap words, so a panic in one may
            // leave an object half-updated for Scheme code to reach. It
            // poisons the isolate instead of becoming a condition, unlike a
            // bridge's panic, which cannot touch the heap. It is caught
            // here so it never unwinds through the frames it ran in.
            Kind::Native(f) => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self, args)))
                    .unwrap_or_else(|panic| {
                        let what = format!(
                            "{}: panicked: {}",
                            code.name,
                            crate::bridge::panic_message(&*panic)
                        );
                        self.poisoned = Some(what.clone());
                        Err(Error::Poisoned(what))
                    })
            }
            Kind::Bridge(f) => f.invoke(self, args),
            _ => unreachable!("only Rust procedures are called here"),
        };
        if result.is_err() {
            self.task.tail_call = None;
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

    /// The constants, variables and direct-call closures of the loaded
    /// module at `module`.
    fn instance_registers(
        &mut self,
        module: usize,
    ) -> (*const Value, *const Cell, *mut Value, u64) {
        let index = self.instances.of_loaded(module);
        let instance = &mut self.instances[index];
        (
            instance.constants.as_ptr(),
            instance.variables.as_ptr(),
            instance.statics.as_mut_ptr(),
            // 0 for a module that is not freed once nothing refers to it.
            if instance.object == Value::FALSE {
                0
            } else {
                instance.object.raw()
            },
        )
    }

    /// Makes the closure direct calls give function `function` of the
    /// loaded module at `module`, which has no free variables, and keeps it
    /// with the module's instance.
    fn make_static(&mut self, module: usize, function: usize) -> Result<Value, Error> {
        let index = self.instances.of_loaded(module);
        if self.instances[index].object != Value::FALSE {
            return Err(Error::scheme(
                "direct calls are not supported in a module that is freed once nothing refers to it",
            ));
        }
        let target = &self.instances[index].loaded.functions[function] as *const Code<S>;
        let closure = self.closure_raw(target, &[])?;
        self.hold(closure);
        self.instances[index].statics[function] = closure;
        Ok(closure)
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
        let mut statics: *mut Value = std::ptr::null_mut();
        // The object of the current module if it is freed once nothing
        // refers to it, else 0: closures made here keep it alive.
        let mut owner = 0u64;
        let mut current_module = 0usize;
        // Debug builds: the function the current frame runs, to check that
        // slot 0 still holds a closure of it (`check_frame`).
        #[cfg(debug_assertions)]
        let mut running: *const Code<S> = std::ptr::null();
        let mut next = Next::Call {
            direct: false,
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
                    } = *$callee.kind()
                    else {
                        unreachable!("only bytecode functions are switched to");
                    };
                    code = words;
                    #[cfg(debug_assertions)]
                    {
                        running = $callee as *const Code<S>;
                    }
                    if module as usize != current_module {
                        current_module = module as usize;
                        (constants, variables, statics, owner) =
                            self.instance_registers(current_module);
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
                        (fp, limit) = match self.materialize(link, caller, base) {
                            Ok(ends) => ends,
                            Err(e) => {
                                if S::PRECISE {
                                    S::release(self.heap_mut(), value);
                                }
                                return Err(e);
                            }
                        };
                        // Under precise counting the value comes with a
                        // reference of its own, into a slot the copy left
                        // clear.
                        set(fp, base + 3, value);
                        clear_header(fp, base);
                        pc = position_resume(position) as usize;
                        let caller = attempt!(self.procedure_code(slot(fp, 0)));
                        if S::PRECISE
                            && let Kind::Bytecode { slots, .. } = *caller.kind()
                        {
                            frame_slots = usize::from(slots).max(base + 4);
                        }
                        link_to!(fp, stub_of!(*fp.add(2), caller));
                        switch_to!(caller);
                    }
                    Next::Raise(error)
                        if fp != top
                            && self.task.dynamic[crate::continuations::HANDLERS]
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
                        self.convert_lazy(code)?;
                        let Kind::Bytecode { slots, .. } = *code.kind() else {
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
                            // Past the innermost frame's slots: nothing has
                            // used it.
                            crate::asan::unpoison_words(
                                callee_fp.wrapping_sub(needed),
                                callee_fp.wrapping_add(3),
                            );
                            if S::PRECISE {
                                self.release_slots(fp, base, base + 3);
                            }
                        } else {
                            // Failing to find room ends the run.
                            self.debug_at(fp, pc);
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
                        self.debug_at(fp, pc);
                        // Unwind to the innermost `%catch` and call its
                        // handler with the condition in place of the `%catch`
                        // call; with none, or for an exit, return the error
                        // to the host.
                        let catch = match error {
                            Error::Exit(_) | Error::OutOfFuel | Error::Poisoned(_) => None,
                            _ => self.task.catches.pop(),
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
                            while self.task.stack.len() > catch.segments {
                                self.pop_segment();
                            }
                            limit = self.task.stack.last().expect("a segment").limit();
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
                            direct: false,
                            base: catch.base,
                            argc: 1,
                            resume: catch.resume,
                        };
                        continue;
                    }
                    Next::Call {
                        base,
                        argc,
                        resume,
                        direct,
                    } => {
                        #[cfg(debug_assertions)]
                        self.check_frame(fp, top, limit, running, "a call");
                        // A call is a safepoint.
                        if S::COUNTS && fp != top && S::epoch_due(self.heap()) {
                            let roots = self.stack_roots(fp);
                            self.epoch(&roots);
                        }
                        if S::PRECISE && resume != ENTRY {
                            // The callee's header goes in these slots of
                            // the caller's, which may hold dead values; and
                            // whatever the callee is, they hold none after.
                            self.release_slots(fp, base, base + 3);
                        }
                        let procedure = slot(fp, base + 3);
                        let callee = if direct {
                            // The validator checked the function's arity.
                            &*(get(procedure, Self::LAYOUT.closure_code()) as *const Code<S>)
                        } else {
                            let callee = attempt!(self.procedure_code(procedure));
                            attempt!(control_arity(callee, argc));
                            callee
                        };
                        match *callee.kind() {
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
                                    segments: self.task.stack.len(),
                                    handler: slot(fp, base + 5),
                                    record: Value::FALSE,
                                });
                                self.store(fp, base + 3, slot(fp, base + 4));
                                // The arguments are dead: the thunk was
                                // copied, and the catch holds the handler.
                                self.release_slots(fp, base + 4, base + 4 + argc);
                                next = Next::Call {
                                    direct: false,
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
                                    direct: false,
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
                                // A debugger can stop in the Rust procedure.
                                self.debug_at(fp, pc);
                                let result = attempt!(self.call_native(callee, fp, base + 4, argc));
                                if let Some((procedure, args)) = self.task.tail_call.take() {
                                    // The Rust procedure asked for a tail
                                    // call: make it in its place.
                                    if !fits(fp.sub(base + 3), args.len() + 2, limit) {
                                        fail!(Error::scheme(
                                            "too many arguments to fit in this stack segment"
                                        ));
                                    }
                                    // The arguments may reach below the frame.
                                    crate::asan::unpoison_words(
                                        fp.wrapping_sub(base + 4 + args.len()),
                                        fp,
                                    );
                                    self.store(fp, base + 3, procedure);
                                    for (i, &arg) in args.iter().enumerate() {
                                        self.store(fp, base + 4 + i, arg);
                                    }
                                    // Arguments it does not pass on are dead.
                                    self.release_slots(fp, base + 4 + args.len(), base + 4 + argc);
                                    if S::PRECISE && resume == ENTRY {
                                        frame_slots = frame_slots.max(args.len() + 1);
                                    }
                                    next = Next::Call {
                                        direct: false,
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
                            Kind::Lazy { .. } => {
                                // Not converted yet: do that, and call it
                                // again, now bytecode.
                                if !callee.arity.accepts(argc) {
                                    fail!(arity_error(callee, argc));
                                }
                                self.debug_at(fp, pc);
                                attempt!(self.convert_lazy(callee));
                                next = Next::Call {
                                    direct,
                                    base,
                                    argc,
                                    resume,
                                };
                                continue;
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
                                    let (top, new_limit) = {
                                        self.debug_at(fp, pc);
                                        attempt!(self.push_segment(needed + 4))
                                    };
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
                    Next::TailCall { argc, direct } => {
                        let callee = if direct {
                            &*(get(slot(fp, 0), Self::LAYOUT.closure_code()) as *const Code<S>)
                        } else {
                            let callee = attempt!(self.procedure_code(slot(fp, 0)));
                            attempt!(control_arity(callee, argc));
                            callee
                        };
                        match *callee.kind() {
                            Kind::Catch => {
                                // As a tail call, the handler takes this
                                // frame's place in its caller, and the thunk
                                // runs in this frame.
                                let position = *fp.add(2);
                                if position == ENTRY {
                                    fail!(Error::scheme("%catch: cannot be called from Rust"));
                                }
                                let segments = if position & BOUNDARY != 0 {
                                    self.task.stack.len() - 1
                                } else {
                                    self.task.stack.len()
                                };
                                // A sealed caller is held by the
                                // continuation below the stack.
                                let record = if position & UNDERFLOW != 0 {
                                    self.task.below
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
                                self.release_slots(fp, 1, 1 + argc);
                                *fp.add(2) = position | CATCH;
                                next = Next::TailCall {
                                    argc: 0,
                                    direct: false,
                                };
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
                                next = Next::TailCall {
                                    argc: 1,
                                    direct: false,
                                };
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
                                self.debug_at(fp, pc);
                                let result = attempt!(self.call_native(callee, fp, 1, argc));
                                if let Some((procedure, args)) = self.task.tail_call.take() {
                                    if !fits(fp, args.len() + 2, limit) {
                                        fail!(Error::scheme(
                                            "too many arguments to fit in this stack segment"
                                        ));
                                    }
                                    // The arguments may reach below the frame.
                                    crate::asan::unpoison_words(
                                        fp.wrapping_sub(1 + args.len()),
                                        fp,
                                    );
                                    self.store(fp, 0, procedure);
                                    for (i, &arg) in args.iter().enumerate() {
                                        self.store(fp, 1 + i, arg);
                                    }
                                    // Arguments it does not pass on are dead.
                                    self.release_slots(fp, 1 + args.len(), 1 + argc);
                                    if S::PRECISE {
                                        frame_slots = frame_slots.max(args.len() + 1);
                                    }
                                    next = Next::TailCall {
                                        argc: args.len(),
                                        direct: false,
                                    };
                                    continue;
                                }
                                if S::PRECISE {
                                    // The arguments are dead, and the result
                                    // returns with a reference of its own.
                                    self.release_slots(fp, 1, 1 + argc);
                                    S::retain(self.heap_mut(), result);
                                }
                                next = Next::Return(result);
                                continue;
                            }
                            Kind::Lazy { .. } => {
                                // Not converted yet: do that, and call it
                                // again, now bytecode.
                                if !callee.arity.accepts(argc) {
                                    fail!(arity_error(callee, argc));
                                }
                                self.debug_at(fp, pc);
                                attempt!(self.convert_lazy(callee));
                                next = Next::TailCall { argc, direct };
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
                                    let (top, new_limit) = {
                                        self.debug_at(fp, pc);
                                        attempt!(self.push_segment(needed + 4))
                                    };
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
                        #[cfg(debug_assertions)]
                        self.check_frame(fp, top, limit, running, "a return");
                        let position = *fp.add(2);
                        // Where the frame ends, for poisoning what it had once
                        // it has returned (slot 0 is taken below).
                        #[cfg(feature = "asan")]
                        let dead_low = fp.wrapping_sub(self.suspended_slots(fp));
                        if S::PRECISE {
                            // The returning frame's code dropped everything
                            // but its closure, and `value` comes with a
                            // reference of its own.
                            debug_assert!(
                                (1..frame_slots).all(|i| !slot(fp, i).is_pointer()),
                                "a returning frame holds a reference"
                            );
                            let closure = Self::take(fp, 0);
                            S::release(self.heap_mut(), closure);
                        }
                        if position == ENTRY {
                            if S::PRECISE {
                                S::release(self.heap_mut(), value);
                            }
                            return Ok(value);
                        }
                        let caller = *fp as *mut u64;
                        // One test for the rare cases.
                        if position & (CATCH | UNDERFLOW | RAISED) != 0 {
                            if position & CATCH != 0 {
                                self.catch_returned();
                            }
                            if position & RAISED != 0 {
                                if S::PRECISE {
                                    S::release(self.heap_mut(), value);
                                }
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
                        // The frame is dead now it has returned to a caller in
                        // its own segment: nothing may touch what it had below
                        // its caller's.
                        #[cfg(feature = "asan")]
                        if position & (BOUNDARY | UNDERFLOW) == 0 {
                            crate::asan::poison_words(dead_low, self.lowest_live(caller));
                        }
                        let base = position_base(position);
                        // The value's reference moves into the slot, where
                        // the callee's closure was, or which its move to
                        // another segment cleared.
                        debug_assert!(!S::PRECISE || !slot(caller, base + 3).is_pointer());
                        set(caller, base + 3, value);
                        clear_header(caller, base);
                        if position & BOUNDARY != 0 {
                            limit = self.pop_segment();
                        }
                        fp = caller;
                        pc = position_resume(position) as usize;
                        let caller = attempt!(self.procedure_code(slot(fp, 0)));
                        if S::PRECISE
                            && let Kind::Bytecode { slots, .. } = *caller.kind()
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
                    #[cfg(feature = "counters")]
                    {
                        self.counters.instructions += 1;
                    }
                    let Some(op) = Opcode::from_u8(word as u8) else {
                        unreachable!("validated code has only known opcodes");
                    };
                    // One instruction, its fields given. The match is
                    // written once and expanded twice: for instructions as
                    // they come, and, from the arm for a `wide` prefix, for
                    // the instruction after it with its fields widened. So
                    // a wide prefix costs nothing until there is one.
                    macro_rules! execute {
                        ($mode:tt, $op:expr, $a:expr, $b:expr, $c:expr, $word:expr, $wide:expr) => {{
                            let (op, a, b, c, word, wide): (Opcode, usize, usize, usize, u32, bool) =
                                ($op, $a, $b, $c, $word, $wide);
                            let _ = wide;
                    match op {
                        Opcode::Mov => {
                            self.store(fp, a, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::Move => {
                            // Only the ownership pass places it, under
                            // precise counting: the reference moves.
                            debug_assert!(S::PRECISE && a != b);
                            self.shift(fp, b, a);
                            pc += 1;
                        }
                        Opcode::Drop => {
                            debug_assert!(S::PRECISE);
                            let v = Self::take(fp, a);
                            S::release(self.heap_mut(), v);
                            pc += 1;
                        }
                        Opcode::LoadConstant => {
                            self.store(fp, a, *constants.add(usize::from(field_d(word))));
                            pc += 1;
                        }
                        Opcode::LoadInt => {
                            let n = field_d(word) as i16;
                            let v = Value::fixnum(i64::from(n)).expect("16 bits fit");
                            self.store(fp, a, v);
                            pc += 1;
                        }
                        Opcode::GetVariable => {
                            let index = usize::from(field_d(word));
                            let value = *(*variables.add(index)).0;
                            if value == Value::UNBOUND {
                                let closure = slot(fp, 0);
                                let Kind::Bytecode { module, .. } =
                                    *attempt!(self.procedure_code(closure)).kind()
                                else {
                                    unreachable!()
                                };
                                let loaded: &Loaded<S> = &*module;
                                let name = &loaded.variables[index].name;
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
                            // Slots a byte each, or two bytes when wide.
                            let per_word = if wide { 2 } else { 4 };
                            let bits = 32 / per_word;
                            let mask = (1u32 << bits) - 1;
                            let free: Vec<Value> = (0..count)
                                .map(|i| {
                                    let w = *code.add(pc + 2 + i / per_word);
                                    slot(fp, ((w >> (bits * (i % per_word))) & mask) as usize)
                                })
                                .collect();
                            let current = attempt!(self.procedure_code(slot(fp, 0)));
                            let Kind::Bytecode { module, .. } = *current.kind() else {
                                unreachable!()
                            };
                            let loaded: &Loaded<S> = &*module;
                            let target = &loaded.functions[function] as *const Code<S>;
                            let closure = if owner == 0 {
                                attempt!(self.closure_raw(target, &free))
                            } else {
                                attempt!(self.closure_owned(target, &free, Value::from_raw(owner)))
                            };
                            self.store(fp, a, closure);
                            pc += 2 + count.div_ceil(per_word);
                        }
                        Opcode::Wide => wide_arm!($mode, a, b, c),
                        Opcode::Free => {
                            let closure = slot(fp, 0);
                            let v = Value::from_raw(get(closure, Self::LAYOUT.closure_free(b)));
                            self.store(fp, a, v);
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
                            let v = Value::from_raw(get(cell, Self::LAYOUT.field(TypeCode::Box, 0)));
                            self.store(fp, a, v);
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
                                direct: false,
                                base: a,
                                argc: b,
                                resume: (pc + 1) as u64,
                            };
                        }
                        Opcode::TailCall => {
                            if S::PRECISE {
                                // The procedure and arguments move down,
                                // counting out the old closure and whatever
                                // else they move over.
                                if a != 0 {
                                    for i in 0..=b {
                                        self.shift(fp, a + i, i);
                                    }
                                }
                            } else {
                                for i in 0..=b {
                                    set(fp, i, slot(fp, a + i));
                                }
                            }
                            break Next::TailCall { argc: b, direct: false };
                        }
                        Opcode::CallDirect => {
                            let function = *code.add(pc + 1) as usize;
                            let mut closure = *statics.add(function);
                            if closure == Value::FALSE {
                                closure = attempt!(self.make_static(current_module, function));
                            }
                            self.store(fp, a + 3, closure);
                            break Next::Call {
                                base: a,
                                argc: b,
                                resume: (pc + 2) as u64,
                                direct: true,
                            };
                        }
                        Opcode::TailCallDirect => {
                            let function = *code.add(pc + 1) as usize;
                            let mut closure = *statics.add(function);
                            if closure == Value::FALSE {
                                closure = attempt!(self.make_static(current_module, function));
                            }
                            if S::PRECISE {
                                if a != 0 {
                                    for i in 1..=b {
                                        self.shift(fp, a + i, i);
                                    }
                                }
                                let old = slot(fp, 0);
                                S::retain(self.heap_mut(), closure);
                                set(fp, 0, closure);
                                S::release(self.heap_mut(), old);
                            } else {
                                for i in 1..=b {
                                    set(fp, i, slot(fp, a + i));
                                }
                                set(fp, 0, closure);
                            }
                            break Next::TailCall { argc: b, direct: true };
                        }
                        Opcode::Return => {
                            let value = slot(fp, a);
                            if S::PRECISE {
                                // The value leaves with a reference of its
                                // own: moved out of its slot, or counted in
                                // again from slot 0, which the closure keeps
                                // until the frame goes.
                                if a == 0 {
                                    S::retain(self.heap_mut(), value);
                                } else {
                                    set(fp, a, Value::UNSPECIFIED);
                                }
                            }
                            break Next::Return(value);
                        }
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
                            if Self::pair_is_frozen(pair) {
                                fail!(self.immutable_pair("set-car!", pair));
                            }
                            self.set_car(pair, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::SetCdr => {
                            let pair = slot(fp, a);
                            if !pair.is_pair() {
                                fail!(type_error("set-cdr!", "a pair", pair));
                            }
                            if Self::pair_is_frozen(pair) {
                                fail!(self.immutable_pair("set-cdr!", pair));
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
                                if self.header_flags(vector, TypeCode::Vector) & data::IMMUTABLE
                                    != 0
                                {
                                    fail!(self.immutable_vector(name, vector));
                                }
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
                                    .map_or(0, |h| (h.data() & data::closure::FREE_MASK) as usize)
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
                                self.procedure_code(closure).map(|c| c.kind()),
                                Ok(Kind::Bytecode { module, .. } | Kind::Lazy { module, .. })
                                    if *module as usize == current_module
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
                        }};
                    }
                    // The `wide` arm: the prefix's fields are the high bytes
                    // of the next instruction's.
                    macro_rules! wide_arm {
                        (normal, $a:expr, $b:expr, $c:expr) => {{
                            pc += 1;
                            let inner = *code.add(pc);
                            let (x, y, z) = fields(inner);
                            let Some(op) = Opcode::from_u8(inner as u8) else {
                                unreachable!("validated code has only known opcodes");
                            };
                            execute!(
                                wide,
                                op,
                                $a << 8 | usize::from(x),
                                $b << 8 | usize::from(y),
                                $c << 8 | usize::from(z),
                                inner,
                                true
                            )
                        }};
                        (wide, $a:expr, $b:expr, $c:expr) => {{ unreachable!("validated code never has two prefixes in a row") }};
                    }
                    execute!(normal, op, a, b, c, word, false)
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
