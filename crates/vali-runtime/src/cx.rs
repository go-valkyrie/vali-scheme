//! An isolate's context, and the raw object API.
//!
//! A [`Cx`] holds an isolate's heap, its root table, what is loaded into it
//! and its current task; an [`Isolate`](crate::Isolate) lends it to a host
//! through `with`. One thread at a time uses it (ADR 0008), so the strategy's state needs no locks and its counts
//! no atomics. The object constructors and accessors here
//! work on raw [`Value`]s, so the accessors are `unsafe`: a raw value is
//! valid only until the next allocation, and must have the type the accessor
//! expects.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vali_abi::{Header, Layout, TypeCode, data};

use crate::code::Code;
use crate::handle::{Error, Handle, RootTable};
use crate::page::{HeapConfig, OutOfMemory};
use crate::strategy::{Space, Strategy};
use crate::task::Task;
use crate::value::Value;

/// The most fields an object can have, so that every field's offset from
/// its value, in bytes, fits the 32 bits `Layout::field` computes in.
const MAX_FIELDS: usize = (i32::MAX as usize / 8) - 2;

/// A variable's cell that the isolate owns, boxed so it stays put.
pub(crate) type OwnCell = Box<std::cell::UnsafeCell<Value>>;

/// An isolate's context: its heap, its root table, what is loaded into it
/// and the task it is running. Holding `&mut Cx`
/// proves the isolate is not running Scheme code anywhere else, which is
/// what using a handle requires (ADR 0003).
pub struct Cx<S: Strategy> {
    heap: S::Heap,
    roots: Arc<RootTable>,
    /// Modules loaded into this isolate.
    pub(crate) instances: crate::code::Instances<S>,
    /// The cells of the host's globals, by name, which variables linked
    /// as globals share. Boxed so the cells stay put.
    pub(crate) globals: HashMap<Box<str>, OwnCell>,
    /// The cells of the procedures implemented in Rust that `(vali
    /// primitive)` exports, by name, apart from the globals, so only code
    /// that imports that library reaches them.
    pub(crate) primitives: HashMap<Box<str>, OwnCell>,
    /// The cells of native functions modules link to, by library identity
    /// and name, each holding its procedure.
    pub(crate) native_cells: HashMap<(Box<str>, Box<str>), OwnCell>,
    /// Whether the primitives are installed (`Cx::install_primitives`).
    pub(crate) primitives_installed: bool,
    /// Feature identifiers beyond [`crate::features`]: the `srfi-N` of the
    /// libraries the compiler ships, which it sets.
    pub(crate) extra_features: Vec<Box<str>>,
    /// Loaded libraries, by identity, which imports link to.
    pub(crate) libraries: HashMap<Box<str>, crate::code::ModuleId>,
    /// Where the next constant of a module goes (ADR 0020).
    pub(crate) pool: crate::pool::Pool,
    /// Interned symbols.
    pub(crate) symbols: HashMap<Box<str>, Value>,
    /// The interned SRFI 88 keywords, by name without the colon.
    pub(crate) keywords: HashMap<Box<str>, Value>,
    /// Descriptors of Rust procedures, which closures point to. Each is
    /// boxed so its address survives the vector growing.
    #[allow(clippy::vec_box)]
    pub(crate) natives: Vec<Box<Code<S>>>,
    /// The task running, or to run next: its stack and dynamic state.
    pub(crate) task: Task,
    /// The most words a task's stack may take.
    pub(crate) max_stack_words: usize,
    /// Whether Scheme code is running, which forbids re-entering it.
    pub(crate) running: bool,
    /// Why the isolate refuses to run: a core primitive panicked, and may
    /// have left the heap half-updated.
    pub(crate) poisoned: Option<String>,
    /// Calls made, the fuel used so far (ADR 0009).
    pub(crate) fuel_used: u64,
    /// What it has done, counted (the `counters` feature).
    #[cfg(feature = "counters")]
    pub(crate) counters: crate::Counters,
    /// The most fuel the isolate may use, if limited.
    pub(crate) fuel_limit: Option<u64>,
    /// The syntax objects that heap objects of type `Syntax` index.
    pub(crate) syntaxes: Vec<vali_syntax::Syntax>,
    /// The limits the host set (ADR 0023), and the Rust-side bytes of
    /// symbols, keywords and syntax objects charged against `max_bytes`.
    pub(crate) limits: Limits,
    /// Data the host keeps here for its Rust procedures to reach.
    pub(crate) host_data: Option<Box<dyn std::any::Any + Send>>,
    /// The expander's data, for the syntax procedures: its scopes and
    /// bindings while a transformer runs, or a program's, for syntax
    /// objects at run time.
    pub(crate) expander_data: Option<Box<dyn std::any::Any + Send>>,
    /// What `eval` needs, if the program can evaluate code: the compiler's
    /// state, kept apart from the host's.
    pub(crate) evaluator: Option<Box<dyn std::any::Any + Send>>,
    /// The procedure an error calls where it happens, while a handler is
    /// installed: (scheme base)'s `raise`.
    pub(crate) raise_hook: Option<Handle>,
    /// The record type of error objects, made when first needed.
    pub(crate) error_object_type: Option<Value>,
    /// The record type of multiple values, made when first needed.
    pub(crate) values_type: Option<Value>,
    /// The console ports: the current ports' defaults (`ports`).
    pub(crate) console_input: Option<Handle>,
    pub(crate) console_output: Option<Handle>,
    pub(crate) console_error: Option<Handle>,
    /// The Rust objects Scheme holds (ADR 0016).
    pub(crate) objects: crate::rust_objects::Objects,
    /// The native libraries loaded programs brought, by identity, which
    /// modules linking to native functions find them in.
    pub(crate) native_libraries: HashMap<Box<str>, Arc<crate::bridge::NativeLibrary>>,
    /// Whether this is an expander's isolate, where only expansion-safe
    /// native functions run (ADR 0009 #17).
    pub(crate) expanding: bool,
}

/// An isolate's limits besides the heap's own, and what it has charged
/// against them (ADR 0023).
#[derive(Debug)]
pub(crate) struct Limits {
    max_bytes: Option<usize>,
    pub(crate) max_symbols: Option<usize>,
    pub(crate) max_syntaxes: Option<usize>,
    /// Rust-side bytes the heap does not see: table entries of symbols,
    /// keywords and syntax objects, estimated per entry.
    rust_bytes: usize,
}

impl Limits {
    fn new(config: &HeapConfig) -> Limits {
        Limits {
            max_bytes: config.max_bytes,
            max_symbols: config.max_symbols,
            max_syntaxes: config.max_syntaxes,
            rust_bytes: 0,
        }
    }
}

/// What one interned name costs on the Rust side: its bytes, the key's box,
/// the value and a control byte of the table.
pub(crate) fn name_entry_bytes(name: &str) -> usize {
    name.len() + std::mem::size_of::<(Box<str>, Value)>() + 1
}

/// What one runtime syntax object costs on the Rust side besides the
/// syntax's own allocations: its slot in the table.
pub(crate) const SYNTAX_SLOT_BYTES: usize = std::mem::size_of::<vali_syntax::Syntax>();

impl<S: Strategy> fmt::Debug for Cx<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cx")
            .field("strategy", &S::NAME)
            .field("heap", &self.heap)
            .finish_non_exhaustive()
    }
}

/// The word at `offset` bytes from the tagged value `v`, accessed as a
/// relaxed atomic (ADR 0003).
///
/// # Safety
///
/// `v` must point to a live object with a word at that offset.
#[inline(always)]
unsafe fn word<'a>(v: Value, offset: i32) -> &'a AtomicU64 {
    let address = v.raw().wrapping_add_signed(i64::from(offset));
    // SAFETY: the caller guarantees the word is part of a live object, and
    // heap words are only ever accessed atomically.
    unsafe { AtomicU64::from_ptr(address as *mut u64) }
}

#[inline(always)]
pub(crate) unsafe fn get(v: Value, offset: i32) -> u64 {
    // SAFETY: passed on to the caller.
    unsafe { word(v, offset) }.load(Ordering::Relaxed)
}

#[inline(always)]
pub(crate) unsafe fn put(v: Value, offset: i32, x: u64) {
    // SAFETY: passed on to the caller.
    unsafe { word(v, offset) }.store(x, Ordering::Relaxed)
}

impl<S: Strategy> Cx<S> {
    /// Where fields sit under this strategy.
    pub const LAYOUT: Layout = Layout::new(S::LAYOUT);

    /// A context with a fresh heap set up by `config`.
    pub fn new(config: &HeapConfig) -> Cx<S> {
        Cx {
            heap: S::new_heap(config),
            roots: RootTable::new(),
            instances: crate::code::Instances::new(),
            globals: HashMap::new(),
            primitives: HashMap::new(),
            native_cells: HashMap::new(),
            primitives_installed: false,
            extra_features: Vec::new(),
            libraries: HashMap::new(),
            pool: Default::default(),
            symbols: HashMap::new(),
            keywords: HashMap::new(),
            natives: Vec::new(),
            task: Task::new(),
            max_stack_words: crate::stack::MAX_STACK_WORDS,
            running: false,
            poisoned: None,
            fuel_used: 0,
            #[cfg(feature = "counters")]
            counters: crate::Counters::default(),
            fuel_limit: None,
            syntaxes: Vec::new(),
            limits: Limits::new(config),
            host_data: None,
            expander_data: None,
            evaluator: None,
            raise_hook: None,
            error_object_type: None,
            values_type: None,
            console_input: None,
            console_output: None,
            console_error: None,
            objects: Default::default(),
            native_libraries: HashMap::new(),
            expanding: false,
        }
    }

    /// Makes this an expander's isolate: a native function not marked
    /// expansion-safe is a stub that fails when called (ADR 0009 #17).
    pub fn set_expanding(&mut self, expanding: bool) {
        self.expanding = expanding;
    }

    /// The strategy's state.
    pub fn heap(&self) -> &S::Heap {
        &self.heap
    }

    /// The strategy's state, to change.
    pub(crate) fn heap_mut(&mut self) -> &mut S::Heap {
        &mut self.heap
    }

    /// The strategy's state, the Rust objects and the loaded modules, all
    /// to change.
    pub(crate) fn heap_objects_instances(
        &mut self,
    ) -> (
        &mut S::Heap,
        &mut crate::rust_objects::Objects,
        &mut crate::code::Instances<S>,
    ) {
        (&mut self.heap, &mut self.objects, &mut self.instances)
    }

    /// How many modules are loaded: the libraries, programs and evaluated
    /// code whose modules are not freed yet.
    #[doc(hidden)]
    pub fn loaded_modules(&self) -> usize {
        self.instances.live()
    }

    /// How many slots the loaded modules take, live or free: bounded where
    /// modules are freed once nothing refers to them.
    #[doc(hidden)]
    pub fn module_slots(&self) -> usize {
        self.instances.slots()
    }

    /// How many symbols and keywords are interned.
    #[doc(hidden)]
    pub fn interned_symbols(&self) -> usize {
        self.symbols.len() + self.keywords.len()
    }

    /// How many runtime syntax objects the isolate holds.
    #[doc(hidden)]
    pub fn live_syntaxes(&self) -> usize {
        self.syntaxes.len()
    }

    /// How many bytes the heap has taken from the system.
    pub fn bytes_used(&self) -> usize {
        S::bytes_used(&self.heap)
    }

    /// Whether `bytes` more of Rust-side memory fit the memory limit
    /// together with the heap and what is already charged (ADR 0023).
    pub(crate) fn check_rust_bytes(&self, bytes: usize) -> Result<(), crate::Error> {
        match self.limits.max_bytes {
            Some(max)
                if self
                    .bytes_used()
                    .saturating_add(self.limits.rust_bytes)
                    .saturating_add(bytes)
                    > max =>
            {
                Err(crate::Error::OutOfMemory)
            }
            _ => Ok(()),
        }
    }

    /// Whether a symbol or keyword named `name`, not interned yet, fits
    /// the limits (ADR 0023): its count and its Rust-side bytes.
    pub(crate) fn check_new_name(&self, name: &str) -> Result<(), crate::Error> {
        if let Some(max) = self.limits.max_symbols
            && self.symbols.len() + self.keywords.len() >= max
        {
            return Err(crate::Error::LimitExceeded(crate::Limit::Symbols));
        }
        self.check_rust_bytes(name_entry_bytes(name))
    }

    /// Charges `bytes` of Rust-side memory, which `check_rust_bytes`
    /// allowed, against the memory limit.
    pub(crate) fn charge_rust_bytes(&mut self, bytes: usize) {
        self.limits.rust_bytes += bytes;
    }

    /// A new object of type `t` with `fields` words after its header, the
    /// header written and the fields uninitialised.
    pub(crate) fn new_object(
        &mut self,
        t: TypeCode,
        fields: usize,
        data: u32,
    ) -> Result<Value, OutOfMemory> {
        let space = match t {
            TypeCode::Pair => Space::Pair,
            TypeCode::Flonum => Space::Flonum,
            _ => Space::General,
        };
        self.new_object_in(t, fields, data, space)
    }

    /// As [`Cx::new_object`], in `space`.
    pub(crate) fn new_object_in(
        &mut self,
        t: TypeCode,
        fields: usize,
        data: u32,
        space: Space,
    ) -> Result<Value, OutOfMemory> {
        // Field offsets are 32-bit (`Layout::field`): a bigger object's
        // last fields would wrap around to addresses before it.
        if fields > MAX_FIELDS {
            return Err(OutOfMemory);
        }
        let words = Self::LAYOUT.words(t, fields);
        let memory = S::allocate(&mut self.heap, space, words)?;
        #[cfg(feature = "counters")]
        {
            self.counters.allocations += 1;
            self.counters.words += words as u64;
        }
        // SAFETY: the memory was just allocated for an object of type `t`.
        let v = unsafe { Value::from_address(t.tag(), memory.as_ptr() as u64) };
        if let Some(offset) = Self::LAYOUT.header(t) {
            let header = Header::new(t, data).expect("runtime data fits in 24 bits");
            // SAFETY: the object was just allocated with room for its header.
            unsafe { put(v, offset, header.0) };
        }
        S::allocated(&mut self.heap, v);
        Ok(v)
    }

    /// Tells the strategy the runtime keeps a reference to `v` outside the
    /// heap, for as long as the isolate lives: a module's constant, an
    /// interned symbol, a type the runtime made.
    pub(crate) fn hold(&mut self, v: Value) {
        if v.is_pointer() {
            S::retain(&mut self.heap, v);
        }
    }

    /// Stores `x` in a field of `v`, an object just allocated, telling the
    /// strategy.
    ///
    /// # Safety
    ///
    /// `v` must be a live object with a field at `offset` that holds
    /// values, and `x` a valid value.
    #[inline(always)]
    pub(crate) unsafe fn init_field(&mut self, v: Value, offset: i32, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { put(v, offset, x.raw()) };
        S::init(&mut self.heap, x);
    }

    /// Stores `x` over the value in a field of `v`, telling the strategy.
    ///
    /// # Safety
    ///
    /// `v` must be a live object with a field at `offset` that holds a
    /// valid value, and `x` a valid value.
    #[inline(always)]
    pub(crate) unsafe fn write_field(&mut self, v: Value, offset: i32, x: Value) {
        // SAFETY: passed on to the caller.
        let old = unsafe { Value::from_raw(get(v, offset)) };
        // SAFETY: as above.
        unsafe { put(v, offset, x.raw()) };
        S::write(&mut self.heap, old, x);
    }

    /// Stores `x` over the value in a variable's cell, telling the strategy.
    ///
    /// # Safety
    ///
    /// `cell` must be a cell of this isolate, and `x` a valid value.
    #[inline(always)]
    pub(crate) unsafe fn write_cell(&mut self, cell: *mut Value, x: Value) {
        // SAFETY: passed on to the caller.
        let old = unsafe { std::mem::replace(&mut *cell, x) };
        S::write(&mut self.heap, old, x);
    }

    /// A new pair.
    ///
    /// # Safety
    ///
    /// `car` and `cdr` must be valid values.
    pub unsafe fn cons_raw(&mut self, car: Value, cdr: Value) -> Result<Value, OutOfMemory> {
        let pair = self.new_object(TypeCode::Pair, 2, 0)?;
        // SAFETY: the pair was just allocated with both fields.
        unsafe {
            self.init_field(pair, Self::LAYOUT.car(), car);
            self.init_field(pair, Self::LAYOUT.cdr(), cdr);
        }
        Ok(pair)
    }

    /// A new flonum.
    pub fn flonum_raw(&mut self, x: f64) -> Result<Value, OutOfMemory> {
        let v = self.new_object(TypeCode::Flonum, 1, 0)?;
        // SAFETY: the flonum was just allocated.
        unsafe { put(v, Self::LAYOUT.flonum(), x.to_bits()) };
        Ok(v)
    }

    /// A new vector of `len` elements, each `fill`.
    ///
    /// # Safety
    ///
    /// `fill` must be a valid value.
    pub unsafe fn make_vector_raw(
        &mut self,
        len: usize,
        fill: Value,
    ) -> Result<Value, OutOfMemory> {
        let fields = len.checked_add(1).ok_or(OutOfMemory)?;
        let v = self.new_object(TypeCode::Vector, fields, 0)?;
        // SAFETY: the vector was just allocated with a length word and `len`
        // elements.
        unsafe {
            put(v, Self::LAYOUT.length(TypeCode::Vector), len as u64);
            for i in 0..len {
                self.init_field(v, Self::LAYOUT.vector_element(i), fill);
            }
        }
        Ok(v)
    }

    /// A new bytevector holding `bytes`.
    pub fn bytevector_raw(&mut self, bytes: &[u8]) -> Result<Value, OutOfMemory> {
        let fields = 1 + bytes.len().div_ceil(8);
        let v = self.new_object(TypeCode::Bytevector, fields, 0)?;
        let length = Self::LAYOUT.length(TypeCode::Bytevector);
        // SAFETY: the bytevector was just allocated with a length word and
        // room for every byte, rounded up to whole words; nothing else can
        // see it yet, so plain writes are fine.
        unsafe {
            put(v, length, bytes.len() as u64);
            let start = v.raw().wrapping_add_signed(i64::from(length) + 8) as *mut u8;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), start, bytes.len());
        }
        Ok(v)
    }

    /// A new string holding `s`, which is immutable if `immutable` is set,
    /// as literals are.
    pub fn string_raw(&mut self, s: &str, immutable: bool) -> Result<Value, OutOfMemory> {
        // Allocating the bytes first and the string second is safe while no
        // strategy moves objects; a moving strategy will need the bytes
        // rooted across the second allocation.
        let bytes = self.bytevector_raw(s.as_bytes())?;
        let mut flags = 0;
        if immutable {
            flags |= data::IMMUTABLE;
        }
        if s.is_ascii() {
            flags |= data::ASCII;
        }
        let v = self.new_object(TypeCode::String, 4, flags)?;
        // SAFETY: the string was just allocated with its four fields.
        unsafe {
            put(v, Self::LAYOUT.string_byte_length(), s.len() as u64);
            put(
                v,
                Self::LAYOUT.string_char_count(),
                s.chars().count() as u64,
            );
            self.init_field(v, Self::LAYOUT.string_bytes(), bytes);
            self.init_field(v, Self::LAYOUT.string_hints(), Value::FALSE);
        }
        Ok(v)
    }

    /// The header of a heap object, if its type has one under this strategy.
    ///
    /// # Safety
    ///
    /// `v` must point to a live heap object.
    pub unsafe fn header(&self, v: Value, t: TypeCode) -> Option<Header> {
        let offset = Self::LAYOUT.header(t)?;
        // SAFETY: passed on to the caller.
        Some(Header(unsafe { get(v, offset) }))
    }

    /// A pair's `car`.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair.
    pub unsafe fn car(&self, v: Value) -> Value {
        // SAFETY: passed on to the caller.
        unsafe { Value::from_raw(get(v, Self::LAYOUT.car())) }
    }

    /// A pair's `cdr`.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair.
    pub unsafe fn cdr(&self, v: Value) -> Value {
        // SAFETY: passed on to the caller.
        unsafe { Value::from_raw(get(v, Self::LAYOUT.cdr())) }
    }

    /// Sets a pair's `car`.
    ///
    /// Unchecked: for a pair just built, as the reader's cycle patching and
    /// builders do. A pair that might be a constant, which is immutable,
    /// must be checked first (`pair_is_frozen`); the VM, the primitives and
    /// `Handle::set_car` do.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair that is not a constant, and `x` a valid
    /// value.
    pub unsafe fn set_car(&mut self, v: Value, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { self.write_field(v, Self::LAYOUT.car(), x) }
    }

    /// Sets a pair's `cdr`; unchecked, as `set_car` is.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair that is not a constant, and `x` a valid
    /// value.
    pub unsafe fn set_cdr(&mut self, v: Value, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { self.write_field(v, Self::LAYOUT.cdr(), x) }
    }

    /// A flonum's value.
    ///
    /// # Safety
    ///
    /// `v` must be a live flonum.
    pub unsafe fn flonum_value(&self, v: Value) -> f64 {
        // SAFETY: passed on to the caller.
        f64::from_bits(unsafe { get(v, Self::LAYOUT.flonum()) })
    }

    /// A vector's length.
    ///
    /// # Safety
    ///
    /// `v` must be a live vector.
    pub unsafe fn vector_length(&self, v: Value) -> usize {
        // SAFETY: passed on to the caller.
        unsafe { get(v, Self::LAYOUT.length(TypeCode::Vector)) as usize }
    }

    /// A vector's `i`th element.
    ///
    /// # Safety
    ///
    /// `v` must be a live vector with more than `i` elements.
    pub unsafe fn vector_ref(&self, v: Value, i: usize) -> Value {
        // SAFETY: passed on to the caller.
        unsafe { Value::from_raw(get(v, Self::LAYOUT.vector_element(i))) }
    }

    /// Sets a vector's `i`th element.
    ///
    /// # Safety
    ///
    /// `v` must be a live vector with more than `i` elements, and `x` a
    /// valid value.
    pub unsafe fn vector_set(&mut self, v: Value, i: usize, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { self.write_field(v, Self::LAYOUT.vector_element(i), x) }
    }

    /// A bytevector's bytes.
    ///
    /// # Safety
    ///
    /// `v` must be a live bytevector, not modified while the slice is held.
    pub unsafe fn bytevector_bytes(&self, v: Value) -> &[u8] {
        let length = Self::LAYOUT.length(TypeCode::Bytevector);
        // SAFETY: passed on to the caller; the bytes follow the length word.
        unsafe {
            let len = get(v, length) as usize;
            let start = v.raw().wrapping_add_signed(i64::from(length) + 8) as *const u8;
            std::slice::from_raw_parts(start, len)
        }
    }

    /// A string's text.
    ///
    /// # Safety
    ///
    /// `v` must be a live string, not modified while the text is held.
    pub unsafe fn string_str(&self, v: Value) -> &str {
        // SAFETY: passed on to the caller; a string's bytes are a bytevector
        // holding UTF-8.
        unsafe {
            let bytes = Value::from_raw(get(v, Self::LAYOUT.string_bytes()));
            std::str::from_utf8_unchecked(self.bytevector_bytes(bytes))
        }
    }

    /// A string's length in characters.
    ///
    /// # Safety
    ///
    /// `v` must be a live string.
    pub unsafe fn string_length(&self, v: Value) -> usize {
        // SAFETY: passed on to the caller.
        unsafe { get(v, Self::LAYOUT.string_char_count()) as usize }
    }
}

/// Handles: rooting values for Rust, and the safe constructors.
impl<S: Strategy> Cx<S> {
    /// Holds `v` for Rust code, as a handle owned by this isolate.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub unsafe fn root(&mut self, v: Value) -> Handle {
        if !v.is_pointer() {
            return Handle::immediate(v);
        }
        self.safepoint();
        S::retain(&mut self.heap, v);
        // SAFETY: this context owns its root table, and `&mut self` makes this
        // the only use of it.
        let slot = unsafe { self.roots.insert(v) };
        Handle::rooted(Arc::clone(&self.roots), slot, v.tag())
    }

    /// The raw value `h` holds, if this isolate owns it. The value is valid
    /// until the next allocation.
    pub fn get(&self, h: &Handle) -> Result<Value, Error> {
        h.value_in(&self.roots)
    }

    /// Releases the objects of handles dropped since the last safepoint.
    pub fn safepoint(&mut self) {
        let heap = &mut self.heap;
        // SAFETY: this context owns its root table, and `&mut self` makes this
        // the only use of it.
        unsafe { self.roots.reclaim(|v| S::release(heap, v)) };
    }

    /// How many root slots hold values, after releasing dropped handles.
    pub fn live_handles(&mut self) -> usize {
        self.safepoint();
        // SAFETY: as in `safepoint`.
        unsafe { self.roots.live() }
    }

    /// A new pair.
    pub fn cons(&mut self, car: &Handle, cdr: &Handle) -> Result<Handle, Error> {
        let (car, cdr) = (self.get(car)?, self.get(cdr)?);
        // SAFETY: both values come from live handles of this isolate.
        unsafe {
            let pair = self.cons_raw(car, cdr)?;
            Ok(self.root(pair))
        }
    }

    /// A new flonum.
    pub fn flonum(&mut self, x: f64) -> Result<Handle, Error> {
        let v = self.flonum_raw(x)?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(v) })
    }

    /// A new mutable string holding `s`.
    pub fn string(&mut self, s: &str) -> Result<Handle, Error> {
        let v = self.string_raw(s, false)?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(v) })
    }

    /// A new vector of `len` elements, each `fill`.
    pub fn vector(&mut self, len: usize, fill: &Handle) -> Result<Handle, Error> {
        let fill = self.get(fill)?;
        // SAFETY: `fill` comes from a live handle of this isolate.
        unsafe {
            let v = self.make_vector_raw(len, fill)?;
            Ok(self.root(v))
        }
    }

    fn pair(&self, h: &Handle) -> Result<Value, Error> {
        let v = self.get(h)?;
        if v.is_pair() {
            Ok(v)
        } else {
            Err(Error::WrongType { expected: "a pair" })
        }
    }

    fn vector_value(&self, h: &Handle) -> Result<Value, Error> {
        let v = self.get(h)?;
        if v.tag() == vali_abi::Tag::Other {
            // SAFETY: `v` comes from a live handle and points to a headered
            // object.
            let header = unsafe { self.header(v, TypeCode::Vector) };
            if header.and_then(Header::type_code) == Some(TypeCode::Vector) {
                return Ok(v);
            }
        }
        Err(Error::WrongType {
            expected: "a vector",
        })
    }
}

/// Using handles. Each method takes the context of the handle's isolate,
/// which proves the isolate is not running anywhere else (ADR 0003).
impl Handle {
    /// A pair's `car`.
    pub fn car<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<Handle, Error> {
        let pair = cx.pair(self)?;
        // SAFETY: `pair` is a live pair, and its `car` a valid value.
        unsafe {
            let car = cx.car(pair);
            Ok(cx.root(car))
        }
    }

    /// A pair's `cdr`.
    pub fn cdr<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<Handle, Error> {
        let pair = cx.pair(self)?;
        // SAFETY: `pair` is a live pair, and its `cdr` a valid value.
        unsafe {
            let cdr = cx.cdr(pair);
            Ok(cx.root(cdr))
        }
    }

    /// Sets a pair's `car`; an error if the pair is a constant, which is
    /// immutable.
    pub fn set_car<S: Strategy>(&self, cx: &mut Cx<S>, x: &Handle) -> Result<(), Error> {
        let (pair, x) = (cx.pair(self)?, cx.get(x)?);
        // SAFETY: `pair` is a live pair and `x` a valid value.
        unsafe {
            if Cx::<S>::pair_is_frozen(pair) {
                return Err(Error::scheme(format!(
                    "the pair {} is immutable",
                    cx.written(pair)
                )));
            }
            cx.set_car(pair, x);
        }
        Ok(())
    }

    /// Sets a pair's `cdr`; an error if the pair is a constant, which is
    /// immutable.
    pub fn set_cdr<S: Strategy>(&self, cx: &mut Cx<S>, x: &Handle) -> Result<(), Error> {
        let (pair, x) = (cx.pair(self)?, cx.get(x)?);
        // SAFETY: `pair` is a live pair and `x` a valid value.
        unsafe {
            if Cx::<S>::pair_is_frozen(pair) {
                return Err(Error::scheme(format!(
                    "the pair {} is immutable",
                    cx.written(pair)
                )));
            }
            cx.set_cdr(pair, x);
        }
        Ok(())
    }

    /// A flonum's value.
    pub fn to_f64<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<f64, Error> {
        let v = cx.get(self)?;
        if v.tag() != vali_abi::Tag::Flonum {
            return Err(Error::WrongType {
                expected: "a flonum",
            });
        }
        // SAFETY: `v` is a live flonum.
        Ok(unsafe { cx.flonum_value(v) })
    }

    /// A string's text, copied out.
    pub fn string_value<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<String, Error> {
        let v = cx.get(self)?;
        if v.tag() != vali_abi::Tag::String {
            return Err(Error::WrongType {
                expected: "a string",
            });
        }
        // SAFETY: `v` is a live string, copied before anything can change it.
        Ok(unsafe { cx.string_str(v) }.to_owned())
    }

    /// A vector's length.
    pub fn vector_length<S: Strategy>(&self, cx: &mut Cx<S>) -> Result<usize, Error> {
        let v = cx.vector_value(self)?;
        // SAFETY: `v` is a live vector.
        Ok(unsafe { cx.vector_length(v) })
    }

    /// A vector's `i`th element.
    pub fn vector_ref<S: Strategy>(&self, cx: &mut Cx<S>, i: usize) -> Result<Handle, Error> {
        let v = cx.vector_value(self)?;
        // SAFETY: `v` is a live vector, checked to have more than `i`
        // elements.
        unsafe {
            if i >= cx.vector_length(v) {
                return Err(Error::OutOfRange);
            }
            let element = cx.vector_ref(v, i);
            Ok(cx.root(element))
        }
    }

    /// Sets a vector's `i`th element; an error if the vector is a
    /// literal, which is immutable.
    pub fn vector_set<S: Strategy>(
        &self,
        cx: &mut Cx<S>,
        i: usize,
        x: &Handle,
    ) -> Result<(), Error> {
        let (v, x) = (cx.vector_value(self)?, cx.get(x)?);
        // SAFETY: `v` is a live vector, checked to have more than `i`
        // elements, and `x` a valid value.
        unsafe {
            if i >= cx.vector_length(v) {
                return Err(Error::OutOfRange);
            }
            if cx.header_flags(v, vali_abi::TypeCode::Vector) & vali_abi::data::IMMUTABLE != 0 {
                return Err(Error::scheme(format!(
                    "the vector {} is immutable",
                    cx.written(v)
                )));
            }
            cx.vector_set(v, i, x);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoCollection;

    /// The Rust-side bytes of symbols and syntax objects count against the
    /// memory limit together with the heap's pages (ADR 0023).
    #[test]
    fn rust_bytes_share_the_memory_limit() {
        let mut cx = Cx::<NoCollection>::new(&HeapConfig {
            max_bytes: Some(1000),
            ..Default::default()
        });
        assert_eq!(cx.check_rust_bytes(1000), Ok(()));
        assert_eq!(cx.check_rust_bytes(1001), Err(crate::Error::OutOfMemory));
        cx.charge_rust_bytes(990);
        assert_eq!(cx.check_rust_bytes(10), Ok(()));
        assert_eq!(cx.check_rust_bytes(11), Err(crate::Error::OutOfMemory));
        // Without a memory limit nothing is refused.
        let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
        cx.charge_rust_bytes(usize::MAX / 2);
        assert_eq!(cx.check_rust_bytes(usize::MAX / 2), Ok(()));
    }
}
