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

use vali_abi::{Header, Tag, TypeCode, data};
use vali_bytecode::{Arity, Constant, Module, Opcode, field_d, field_e_signed, fields, validate};

use crate::code::{Code, Instance, Kind, Loaded, ModuleId, NativeFn};
use crate::cx::{Cx, get, put};
use crate::handle::{Error, Handle, SchemeError};
use crate::page::OutOfMemory;
use crate::stack::{SEGMENT_WORDS, Segment};
use crate::strategy::Strategy;
use crate::trampoline::{self, Link};
use crate::value::Value;
use std::ffi::c_void;
use vali_num::Integer;

/// The bytecode position stored in the entry frame's header: returning to
/// it leaves the VM.
const ENTRY: u64 = u64::MAX;

/// Set in a frame's position word when the frame is the first in its
/// segment, so returning from it drops back to the previous segment.
const BOUNDARY: u64 = 1 << 63;

/// A frame's position word: where the caller resumes, and the caller's
/// `base`, so the result can be stored even when the frame was moved to a
/// new segment.
fn position(resume: u64, base: usize) -> u64 {
    resume | (base as u64) << 32
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

#[inline(always)]
unsafe fn slot(fp: *mut u64, i: usize) -> Value {
    // SAFETY: the caller guarantees slot `i` is inside the frame.
    Value::from_raw(unsafe { *fp.sub(i + 1) })
}

#[inline(always)]
unsafe fn set(fp: *mut u64, i: usize, v: Value) {
    // SAFETY: the caller guarantees slot `i` is inside the frame.
    unsafe { *fp.sub(i + 1) = v.raw() }
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
}

/// Numbers the generic arithmetic understands. Rationals and complex
/// numbers come next.
pub(crate) enum Num {
    Fixnum(i64),
    Integer(Integer),
    Flonum(f64),
}

impl Num {
    /// The exact integer, if this is one.
    pub(crate) fn exact(&self) -> Option<Integer> {
        match self {
            Num::Fixnum(n) => Some(Integer::from(*n)),
            Num::Integer(n) => Some(n.clone()),
            Num::Flonum(_) => None,
        }
    }

    pub(crate) fn to_f64(&self) -> f64 {
        match self {
            Num::Fixnum(n) => *n as f64,
            Num::Integer(n) => n.to_f64(),
            Num::Flonum(x) => *x,
        }
    }
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

/// Loading modules, and the procedures Rust code calls.
impl<S: Strategy> Cx<S> {
    /// Validates `module` and loads it into this isolate: its constants are
    /// created in the heap, and each of its variables is bound to the value
    /// of the global of the same name, if there is one.
    pub fn load(&mut self, module: Module) -> Result<ModuleId, Error> {
        validate(&module)
            .map_err(|e| Error::scheme(format!("invalid bytecode in {}: {e}", module.name)))?;
        let loaded = Loaded::<S>::new(module);
        let mut constants = Vec::with_capacity(loaded.module.constants.len());
        for constant in &loaded.module.constants {
            constants.push(self.constant(constant)?);
        }
        let variables = loaded
            .module
            .variables
            .iter()
            .map(|name| self.globals.get(name).copied().unwrap_or(Value::UNBOUND))
            .collect();
        let id = self.instances.len();
        self.instance_of.insert(Arc::as_ptr(&loaded) as usize, id);
        self.instances.push(Instance {
            loaded,
            constants,
            variables,
        });
        Ok(ModuleId(id))
    }

    /// Runs a loaded module's body, returning its result.
    pub fn run(&mut self, module: ModuleId) -> Result<Handle, Error> {
        let instance = &self.instances[module.0];
        let body =
            &instance.loaded.functions[instance.loaded.module.body as usize] as *const Code<S>;
        let closure = self.closure_raw(body, &[])?;
        let result = self.execute(closure, &[])?;
        // SAFETY: the VM returns valid values.
        Ok(unsafe { self.root(result) })
    }

    /// The value of a loaded module's variable, if it has one.
    pub fn variable(&mut self, module: ModuleId, name: &str) -> Option<Handle> {
        let instance = &self.instances[module.0];
        let index = instance
            .loaded
            .module
            .variables
            .iter()
            .position(|v| &**v == name)?;
        let value = instance.variables[index];
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
        self.globals.insert(name.into(), value);
        Ok(())
    }

    /// The value bound to the global `name`, if there is one.
    pub fn global(&mut self, name: &str) -> Option<Handle> {
        let value = *self.globals.get(name)?;
        // SAFETY: globals hold valid values.
        Some(unsafe { self.root(value) })
    }

    /// A procedure implemented in Rust, bound to the global `name`.
    pub fn define_native(
        &mut self,
        name: &str,
        arity: Arity,
        f: NativeFn<S>,
    ) -> Result<Handle, Error> {
        let code = Box::new(Code::native(name, arity, f));
        let pointer = &*code as *const Code<S>;
        self.natives.push(code);
        let closure = self.closure_raw(pointer, &[])?;
        self.globals.insert(name.into(), closure);
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
        Ok(unsafe { self.root(result) })
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
            put(v, Self::LAYOUT.symbol_name(), string.raw());
            put(v, Self::LAYOUT.symbol_hash(), symbol_hash(name));
        }
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
            Constant::Pair(car, cdr) => {
                let car = self.constant(car)?;
                let cdr = self.constant(cdr)?;
                // SAFETY: both are valid values.
                unsafe { self.cons_raw(car, cdr)? }
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
                put(v, Self::LAYOUT.closure_free(i), x.raw());
            }
        }
        Ok(v)
    }

    /// The code of `v`, if it is a procedure.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn procedure_code<'a>(&self, v: Value) -> Result<&'a Code<S>, Error> {
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
    fn is_other(&self, v: Value, t: TypeCode) -> bool {
        // SAFETY: values in slots are valid, so an "other" pointer points to
        // a live object with a header.
        v.tag() == Tag::Other && unsafe { self.header(v, t) }.and_then(Header::type_code) == Some(t)
    }

    pub(crate) fn number(&self, v: Value) -> Option<Num> {
        if let Some(n) = v.as_fixnum() {
            return Some(Num::Fixnum(n));
        }
        if v.tag() == Tag::Flonum {
            // SAFETY: a valid value tagged as a flonum is a live flonum.
            return Some(Num::Flonum(unsafe { self.flonum_value(v) }));
        }
        if self.is_other(v, TypeCode::Bignum) {
            // SAFETY: `v` is a live bignum.
            return Some(Num::Integer(unsafe { self.bignum_value(v) }));
        }
        None
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

    /// Generic arithmetic: the slow path behind the fixnum fast paths. Exact
    /// integers of any size stay exact; a flonum operand makes the result a
    /// flonum.
    pub(crate) fn arith(&mut self, op: Arith, a: Value, b: Value) -> Result<Value, Error> {
        let name = match op {
            Arith::Add => "+",
            Arith::Sub => "-",
            Arith::Mul => "*",
        };
        let (x, y) = match (self.number(a), self.number(b)) {
            (Some(x), Some(y)) => (x, y),
            (None, _) => return Err(type_error(name, "a number", a)),
            (_, None) => return Err(type_error(name, "a number", b)),
        };
        if let (Num::Fixnum(x), Num::Fixnum(y)) = (&x, &y) {
            let result = match op {
                Arith::Add => x.checked_add(*y),
                Arith::Sub => x.checked_sub(*y),
                Arith::Mul => x.checked_mul(*y),
            };
            if let Some(v) = result.and_then(Value::fixnum) {
                return Ok(v);
            }
        }
        match (x.exact(), y.exact()) {
            (Some(x), Some(y)) => {
                let result = match op {
                    Arith::Add => &x + &y,
                    Arith::Sub => &x - &y,
                    Arith::Mul => &x * &y,
                };
                Ok(self.integer_raw(&result)?)
            }
            _ => {
                let (x, y) = (x.to_f64(), y.to_f64());
                let result = match op {
                    Arith::Add => x + y,
                    Arith::Sub => x - y,
                    Arith::Mul => x * y,
                };
                Ok(self.flonum_raw(result)?)
            }
        }
    }

    /// Generic comparison.
    pub(crate) fn compare(&self, op: Compare, a: Value, b: Value) -> Result<bool, Error> {
        let name = match op {
            Compare::Lt => "<",
            Compare::Le => "<=",
            Compare::Eq => "=",
        };
        let (x, y) = match (self.number(a), self.number(b)) {
            (Some(x), Some(y)) => (x, y),
            (None, _) => return Err(type_error(name, "a number", a)),
            (_, None) => return Err(type_error(name, "a number", b)),
        };
        let ordering = match (x.exact(), y.exact()) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => x.to_f64().partial_cmp(&y.to_f64()),
        };
        Ok(match (op, ordering) {
            (_, None) => false,
            (Compare::Lt, Some(o)) => o.is_lt(),
            (Compare::Le, Some(o)) => o.is_le(),
            (Compare::Eq, Some(o)) => o.is_eq(),
        })
    }

    /// Adds a backtrace, read from the frames starting at `fp`, to a Scheme
    /// error.
    ///
    /// # Safety
    ///
    /// `fp` must be the frame pointer of a live frame.
    unsafe fn with_backtrace(&self, mut fp: *mut u64, error: Error) -> Error {
        let Error::Scheme(mut e) = error else {
            return error;
        };
        loop {
            // SAFETY: every frame's slot 0 holds its closure, and its header
            // links to its caller, up to the entry frame.
            unsafe {
                if let Ok(code) = self.procedure_code(slot(fp, 0)) {
                    e.backtrace.push(code.name().to_owned());
                }
                if *fp.add(2) == ENTRY {
                    break;
                }
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
        let segment = &self.stack[0];
        // The entry frame is a pseudo-frame at the top of the segment. The
        // word at its frame pointer will hold the host's stack pointer, for
        // unwinders; its slots 0 to 2 hold the callee's header, slot 3 the
        // procedure, and the arguments follow.
        // SAFETY: the segment has room for 5 + 255 words.
        let (entry, limit) = unsafe { (segment.top().sub(1), segment.limit()) };
        // SAFETY: as above.
        unsafe {
            set(entry, 3, procedure);
            for (i, &arg) in args.iter().enumerate() {
                set(entry, 4 + i, arg);
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
        if let Some(panic) = state.panic {
            std::panic::resume_unwind(panic);
        }
        let result = state.result.expect("the VM ran");
        // An error can leave segments chained; only the first is kept.
        while self.stack.len() > 1 {
            self.pop_segment();
        }
        result
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
        self.stack.push(segment);
        Ok(ends)
    }

    /// Drops the current segment, keeping it as the spare, and returns the
    /// previous segment's limit.
    fn pop_segment(&mut self) -> *mut u64 {
        let segment = self.stack.pop().expect("a segment to pop");
        if let Some(old) = self.spare.replace(segment) {
            self.stack_words -= old.words();
        }
        self.stack.last().expect("the first segment stays").limit()
    }

    /// How many Scheme procedures have been entered so far: the fuel used
    /// (ADR 0009).
    pub fn fuel_used(&self) -> u64 {
        self.fuel_used
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
                set(fp, i, Value::DEFAULT);
            }
            if arity.rest {
                let mut list = Value::EMPTY_LIST;
                for i in (fixed + 1..=argc).rev() {
                    list = self.cons_raw(slot(fp, i), list)?;
                }
                set(fp, fixed + 1, list);
            }
            for i in arity.parameter_slots() + 1..slots {
                set(fp, i, Value::UNSPECIFIED);
            }
        }
        self.fuel_used += 1;
        code.calls.fetch_add(1, Ordering::Relaxed);
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
        f: NativeFn<S>,
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
        f(self, args)
    }

    /// The constants and variables of the loaded module at `module`.
    fn instance_registers(&mut self, module: usize) -> (*const Value, *mut Value) {
        let index = self.instance_of[&module];
        let instance = &mut self.instances[index];
        (instance.constants.as_ptr(), instance.variables.as_mut_ptr())
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
        let mut pc: usize;
        let mut code: *const u32 = std::ptr::null();
        let mut constants: *const Value = std::ptr::null();
        let mut variables: *mut Value = std::ptr::null_mut();
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

        // SAFETY (for the whole loop): the validator checked every slot,
        // constant, variable, function and branch operand; frames are set up
        // by `enter`, which checks the stack limit; heap accesses check
        // types first.
        unsafe {
            macro_rules! fail {
                ($e:expr) => {{
                    let error = $e;
                    return Err(if fp == top {
                        error
                    } else {
                        self.with_backtrace(fp, error)
                    });
                }};
            }
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
                    } else if position & BOUNDARY != 0 {
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
                    *link.record = $fp as u64;
                    *link.record.add(1) = current_stub;
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

            loop {
                // The call machinery: set up the frame `next` asks for.
                match next {
                    Next::Call { base, argc, resume } => {
                        let procedure = slot(fp, base + 3);
                        let callee = attempt!(self.procedure_code(procedure));
                        match callee.kind {
                            Kind::Native(f) => {
                                let result =
                                    attempt!(self.call_native(callee, f, fp, base + 4, argc));
                                if resume == ENTRY {
                                    return Ok(result);
                                }
                                set(fp, base + 3, result);
                                pc = resume as usize;
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
                                        set(new_fp, i, slot(callee_fp, i));
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
                                attempt!(self.enter(callee, callee_fp, argc));
                                fp = callee_fp;
                                link_to!(fp, stub_of!(position, callee));
                                pc = 0;
                                switch_to!(callee);
                            }
                        }
                    }
                    Next::TailCall { argc } => {
                        let callee = attempt!(self.procedure_code(slot(fp, 0)));
                        match callee.kind {
                            Kind::Native(f) => {
                                let result = attempt!(self.call_native(callee, f, fp, 1, argc));
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
                                    let moved: Vec<Value> =
                                        (0..=argc).map(|i| slot(fp, i)).collect();
                                    let position = if old == ENTRY {
                                        ENTRY
                                    } else if old & BOUNDARY != 0 {
                                        self.pop_segment();
                                        old
                                    } else {
                                        let base = (saved as usize - fp as usize) / 8 - 3;
                                        position(old & 0xFFFF_FFFF, base) | BOUNDARY
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
                                    *link.record = fp as u64;
                                }
                                attempt!(self.enter(callee, fp, argc));
                                // A different function now runs in this frame.
                                link_to!(fp, stub_of!(*fp.add(2), callee));
                                pc = 0;
                                switch_to!(callee);
                            }
                        }
                    }
                    Next::Return(value) => {
                        let position = *fp.add(2);
                        if position == ENTRY {
                            return Ok(value);
                        }
                        let caller = *fp as *mut u64;
                        let base = ((position >> 32) & 0xFFFF) as usize;
                        set(caller, base + 3, value);
                        if position & BOUNDARY != 0 {
                            limit = self.pop_segment();
                        }
                        fp = caller;
                        pc = (position & 0xFFFF_FFFF) as usize;
                        let caller = attempt!(self.procedure_code(slot(fp, 0)));
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
                            set(fp, a, slot(fp, b));
                            pc += 1;
                        }
                        Opcode::LoadConstant => {
                            set(fp, a, *constants.add(usize::from(field_d(word))));
                            pc += 1;
                        }
                        Opcode::LoadInt => {
                            let n = field_d(word) as i16;
                            set(fp, a, Value::fixnum(i64::from(n)).expect("16 bits fit"));
                            pc += 1;
                        }
                        Opcode::GetVariable => {
                            let index = usize::from(field_d(word));
                            let value = *variables.add(index);
                            if value == Value::UNBOUND {
                                let closure = slot(fp, 0);
                                let Kind::Bytecode { module, .. } =
                                    attempt!(self.procedure_code(closure)).kind
                                else {
                                    unreachable!()
                                };
                                let loaded: &Loaded<S> = &*module;
                                let name = &loaded.module.variables[index];
                                fail!(Error::scheme(format!("unbound variable: {name}")));
                            }
                            set(fp, a, value);
                            pc += 1;
                        }
                        Opcode::SetVariable => {
                            *variables.add(usize::from(field_d(word))) = slot(fp, a);
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
                            set(fp, a, closure);
                            pc += 2 + count.div_ceil(4);
                        }
                        Opcode::Free => {
                            let closure = slot(fp, 0);
                            set(
                                fp,
                                a,
                                Value::from_raw(get(closure, Self::LAYOUT.closure_free(b))),
                            );
                            pc += 1;
                        }
                        Opcode::MakeBox => {
                            let cell = attempt!(self.new_object(TypeCode::Box, 1, 0));
                            put(
                                cell,
                                Self::LAYOUT.field(TypeCode::Box, 0),
                                slot(fp, b).raw(),
                            );
                            set(fp, a, cell);
                            pc += 1;
                        }
                        Opcode::Unbox => {
                            let cell = slot(fp, b);
                            if !self.is_other(cell, TypeCode::Box) {
                                fail!(type_error("unbox", "a box", cell));
                            }
                            set(
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
                            put(
                                cell,
                                Self::LAYOUT.field(TypeCode::Box, 0),
                                slot(fp, b).raw(),
                            );
                            pc += 1;
                        }
                        Opcode::Jump => {
                            pc = (pc + 1).wrapping_add_signed(field_e_signed(word) as isize);
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
                                set(fp, i, slot(fp, a + i));
                            }
                            break Next::TailCall { argc: b };
                        }
                        Opcode::Return => break Next::Return(slot(fp, a)),
                        Opcode::Add => {
                            let v = arith!(Arith::Add, slot(fp, b), slot(fp, c), i64::checked_add);
                            set(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Sub => {
                            let v = arith!(Arith::Sub, slot(fp, b), slot(fp, c), i64::checked_sub);
                            set(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Mul => {
                            let v =
                                arith!(Arith::Mul, slot(fp, b), slot(fp, c), |x: i64, y: i64| {
                                    (x >> 3).checked_mul(y)
                                });
                            set(fp, a, v);
                            pc += 1;
                        }
                        Opcode::AddImm => {
                            let imm = Value::fixnum(i64::from(c as u8 as i8)).expect("fits");
                            let v = arith!(Arith::Add, slot(fp, b), imm, i64::checked_add);
                            set(fp, a, v);
                            pc += 1;
                        }
                        Opcode::SubImm => {
                            let imm = Value::fixnum(i64::from(c as u8 as i8)).expect("fits");
                            let v = arith!(Arith::Sub, slot(fp, b), imm, i64::checked_sub);
                            set(fp, a, v);
                            pc += 1;
                        }
                        Opcode::Lt => {
                            let r = compare!(Compare::Lt, slot(fp, b), slot(fp, c), <);
                            set(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::Le => {
                            let r = compare!(Compare::Le, slot(fp, b), slot(fp, c), <=);
                            set(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::NumEq => {
                            let r = compare!(Compare::Eq, slot(fp, b), slot(fp, c), ==);
                            set(fp, a, Value::boolean(r));
                            pc += 1;
                        }
                        Opcode::Eq => {
                            set(fp, a, Value::boolean(slot(fp, b) == slot(fp, c)));
                            pc += 1;
                        }
                        Opcode::Not => {
                            set(fp, a, Value::boolean(!slot(fp, b).is_true()));
                            pc += 1;
                        }
                        Opcode::Cons => {
                            let pair = attempt!(self.cons_raw(slot(fp, b), slot(fp, c)));
                            set(fp, a, pair);
                            pc += 1;
                        }
                        Opcode::Car => {
                            let pair = slot(fp, b);
                            if !pair.is_pair() {
                                fail!(type_error("car", "a pair", pair));
                            }
                            set(fp, a, self.car(pair));
                            pc += 1;
                        }
                        Opcode::Cdr => {
                            let pair = slot(fp, b);
                            if !pair.is_pair() {
                                fail!(type_error("cdr", "a pair", pair));
                            }
                            set(fp, a, self.cdr(pair));
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
                            set(fp, a, Value::boolean(slot(fp, b) == Value::EMPTY_LIST));
                            pc += 1;
                        }
                        Opcode::IsPair => {
                            set(fp, a, Value::boolean(slot(fp, b).is_pair()));
                            pc += 1;
                        }
                        Opcode::MakeVector => {
                            let len = slot(fp, b);
                            let Some(n) = len.as_fixnum().filter(|n| *n >= 0) else {
                                fail!(type_error("make-vector", "a non-negative fixnum", len));
                            };
                            let v = attempt!(self.make_vector_raw(n as usize, slot(fp, c)));
                            set(fp, a, v);
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
                                set(fp, a, self.vector_ref(vector, i as usize));
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
                            set(fp, a, Value::fixnum(len).expect("lengths fit"));
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
