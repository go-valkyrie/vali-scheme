//! Heaps and tasks, and the raw object API.
//!
//! A [`Heap`] is shared by an isolate's tasks; each [`Task`] allocates from
//! buffers of its own (ADR 0003). The object constructors and accessors here
//! work on raw [`Value`]s, so the accessors are `unsafe`: a raw value is
//! valid only until the next allocation, and must have the type the accessor
//! expects.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vali_abi::{Header, Layout, TypeCode, data};

use crate::handle::{Error, Handle, RootTable};
use crate::page::{HeapConfig, OutOfMemory};
use crate::strategy::{Space, Strategy};
use crate::value::Value;

/// The memory shared by an isolate's tasks.
pub struct Heap<S: Strategy> {
    state: S::Heap,
}

impl<S: Strategy> fmt::Debug for Heap<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Heap")
            .field("strategy", &S::NAME)
            .field("state", &self.state)
            .finish()
    }
}

impl<S: Strategy> Heap<S> {
    /// A heap set up by `config`.
    pub fn new(config: &HeapConfig) -> Arc<Heap<S>> {
        Arc::new(Heap {
            state: S::new_heap(config),
        })
    }

    /// The strategy's shared state.
    pub fn state(&self) -> &S::Heap {
        &self.state
    }
}

/// One task's view of a heap: its allocation buffers and pending work.
pub struct Task<S: Strategy> {
    heap: Arc<Heap<S>>,
    mutator: S::Mutator,
    roots: Arc<RootTable>,
}

impl<S: Strategy> fmt::Debug for Task<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("strategy", &S::NAME)
            .field("mutator", &self.mutator)
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
unsafe fn get(v: Value, offset: i32) -> u64 {
    // SAFETY: passed on to the caller.
    unsafe { word(v, offset) }.load(Ordering::Relaxed)
}

#[inline(always)]
unsafe fn put(v: Value, offset: i32, x: u64) {
    // SAFETY: passed on to the caller.
    unsafe { word(v, offset) }.store(x, Ordering::Relaxed)
}

impl<S: Strategy> Task<S> {
    /// Where fields sit under this strategy.
    pub const LAYOUT: Layout = Layout::new(S::LAYOUT);

    /// A task allocating from `heap`.
    pub fn new(heap: Arc<Heap<S>>) -> Task<S> {
        let mutator = S::new_mutator(&heap.state);
        Task {
            heap,
            mutator,
            roots: RootTable::new(),
        }
    }

    /// The heap this task allocates from.
    pub fn heap(&self) -> &Arc<Heap<S>> {
        &self.heap
    }

    /// A new object of type `t` with `fields` words after its header, the
    /// header written and the fields uninitialised.
    fn new_object(&mut self, t: TypeCode, fields: usize, data: u32) -> Result<Value, OutOfMemory> {
        let space = match t {
            TypeCode::Pair => Space::Pair,
            TypeCode::Flonum => Space::Flonum,
            _ => Space::General,
        };
        let words = Self::LAYOUT.words(t, fields);
        let memory = S::allocate(&self.heap.state, &mut self.mutator, space, words)?;
        let v = Value::from_address(t.tag(), memory.as_ptr() as u64);
        if let Some(offset) = Self::LAYOUT.header(t) {
            let header = Header::new(t, data).expect("runtime data fits in 24 bits");
            // SAFETY: the object was just allocated with room for its header.
            unsafe { put(v, offset, header.0) };
        }
        Ok(v)
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
            put(pair, Self::LAYOUT.car(), car.raw());
            put(pair, Self::LAYOUT.cdr(), cdr.raw());
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
                put(v, Self::LAYOUT.vector_element(i), fill.raw());
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
            put(v, Self::LAYOUT.string_bytes(), bytes.raw());
            put(v, Self::LAYOUT.string_hints(), Value::FALSE.raw());
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
        Value::from_raw(unsafe { get(v, Self::LAYOUT.car()) })
    }

    /// A pair's `cdr`.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair.
    pub unsafe fn cdr(&self, v: Value) -> Value {
        // SAFETY: passed on to the caller.
        Value::from_raw(unsafe { get(v, Self::LAYOUT.cdr()) })
    }

    /// Sets a pair's `car`.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair and `x` a valid value.
    pub unsafe fn set_car(&mut self, v: Value, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { put(v, Self::LAYOUT.car(), x.raw()) }
    }

    /// Sets a pair's `cdr`.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair and `x` a valid value.
    pub unsafe fn set_cdr(&mut self, v: Value, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { put(v, Self::LAYOUT.cdr(), x.raw()) }
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
        Value::from_raw(unsafe { get(v, Self::LAYOUT.vector_element(i)) })
    }

    /// Sets a vector's `i`th element.
    ///
    /// # Safety
    ///
    /// `v` must be a live vector with more than `i` elements, and `x` a
    /// valid value.
    pub unsafe fn vector_set(&mut self, v: Value, i: usize, x: Value) {
        // SAFETY: passed on to the caller.
        unsafe { put(v, Self::LAYOUT.vector_element(i), x.raw()) }
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
impl<S: Strategy> Task<S> {
    /// Holds `v` for Rust code, as a handle owned by this task.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub unsafe fn root(&mut self, v: Value) -> Handle {
        if !v.is_pointer() {
            return Handle::immediate(v);
        }
        self.safepoint();
        S::retain(&mut self.mutator, v);
        // SAFETY: this task owns its root table, and `&mut self` makes this
        // the only use of it.
        let slot = unsafe { self.roots.insert(v) };
        Handle::rooted(Arc::clone(&self.roots), slot, v.tag())
    }

    /// The raw value `h` holds, if this task owns it. The value is valid
    /// until the next allocation.
    pub fn get(&self, h: &Handle) -> Result<Value, Error> {
        h.value_in(&self.roots)
    }

    /// Releases the objects of handles dropped since the last safepoint.
    pub fn safepoint(&mut self) {
        let mutator = &mut self.mutator;
        // SAFETY: this task owns its root table, and `&mut self` makes this
        // the only use of it.
        unsafe { self.roots.reclaim(|v| S::release(mutator, v)) };
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
        // SAFETY: both values come from live handles of this task.
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
        // SAFETY: `fill` comes from a live handle of this task.
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

/// Using handles. Each method takes the handle's owning task, which proves
/// nothing else is touching that task's objects (ADR 0003).
impl Handle {
    /// A pair's `car`.
    pub fn car<S: Strategy>(&self, task: &mut Task<S>) -> Result<Handle, Error> {
        let pair = task.pair(self)?;
        // SAFETY: `pair` is a live pair, and its `car` a valid value.
        unsafe {
            let car = task.car(pair);
            Ok(task.root(car))
        }
    }

    /// A pair's `cdr`.
    pub fn cdr<S: Strategy>(&self, task: &mut Task<S>) -> Result<Handle, Error> {
        let pair = task.pair(self)?;
        // SAFETY: `pair` is a live pair, and its `cdr` a valid value.
        unsafe {
            let cdr = task.cdr(pair);
            Ok(task.root(cdr))
        }
    }

    /// Sets a pair's `car`.
    pub fn set_car<S: Strategy>(&self, task: &mut Task<S>, x: &Handle) -> Result<(), Error> {
        let (pair, x) = (task.pair(self)?, task.get(x)?);
        // SAFETY: `pair` is a live pair and `x` a valid value.
        unsafe { task.set_car(pair, x) };
        Ok(())
    }

    /// Sets a pair's `cdr`.
    pub fn set_cdr<S: Strategy>(&self, task: &mut Task<S>, x: &Handle) -> Result<(), Error> {
        let (pair, x) = (task.pair(self)?, task.get(x)?);
        // SAFETY: `pair` is a live pair and `x` a valid value.
        unsafe { task.set_cdr(pair, x) };
        Ok(())
    }

    /// A flonum's value.
    pub fn to_f64<S: Strategy>(&self, task: &mut Task<S>) -> Result<f64, Error> {
        let v = task.get(self)?;
        if v.tag() != vali_abi::Tag::Flonum {
            return Err(Error::WrongType {
                expected: "a flonum",
            });
        }
        // SAFETY: `v` is a live flonum.
        Ok(unsafe { task.flonum_value(v) })
    }

    /// A string's text, copied out.
    pub fn string_value<S: Strategy>(&self, task: &mut Task<S>) -> Result<String, Error> {
        let v = task.get(self)?;
        if v.tag() != vali_abi::Tag::String {
            return Err(Error::WrongType {
                expected: "a string",
            });
        }
        // SAFETY: `v` is a live string, copied before anything can change it.
        Ok(unsafe { task.string_str(v) }.to_owned())
    }

    /// A vector's length.
    pub fn vector_length<S: Strategy>(&self, task: &mut Task<S>) -> Result<usize, Error> {
        let v = task.vector_value(self)?;
        // SAFETY: `v` is a live vector.
        Ok(unsafe { task.vector_length(v) })
    }

    /// A vector's `i`th element.
    pub fn vector_ref<S: Strategy>(&self, task: &mut Task<S>, i: usize) -> Result<Handle, Error> {
        let v = task.vector_value(self)?;
        // SAFETY: `v` is a live vector, checked to have more than `i`
        // elements.
        unsafe {
            if i >= task.vector_length(v) {
                return Err(Error::OutOfRange);
            }
            let element = task.vector_ref(v, i);
            Ok(task.root(element))
        }
    }

    /// Sets a vector's `i`th element.
    pub fn vector_set<S: Strategy>(
        &self,
        task: &mut Task<S>,
        i: usize,
        x: &Handle,
    ) -> Result<(), Error> {
        let (v, x) = (task.vector_value(self)?, task.get(x)?);
        // SAFETY: `v` is a live vector, checked to have more than `i`
        // elements, and `x` a valid value.
        unsafe {
            if i >= task.vector_length(v) {
                return Err(Error::OutOfRange);
            }
            task.vector_set(v, i, x);
        }
        Ok(())
    }
}
