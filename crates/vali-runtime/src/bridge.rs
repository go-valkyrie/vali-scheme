//! Rust bridges (ADR 0016): Rust functions and methods exposed to Scheme.
//!
//! A [`NativeLibrary`] is built from ordinary Rust functions. Each
//! parameter converts from a Scheme value through [`Param`], usually by
//! way of [`FromScheme`], and the result back through [`IntoScheme`]; a
//! function's arity follows from its signature. A method's receiver,
//! `&T` or `&mut T` for a [`RustObject`] type `T`, is borrowed from the
//! isolate's table for the call only, checked against other borrows.
//!
//! Bridges see the isolate through [`Isolate`], an object-safe view, so a
//! native library works with any memory strategy.
//!
//! [`RustObject`]: crate::RustObject

use std::any::TypeId;
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use vali_bytecode::Arity;
use vali_num::Number;

use crate::RustObject;
use crate::cx::Cx;
use crate::handle::{Error, Handle};
use crate::rust_objects::{Borrow, ErasedObject};
use crate::strategy::Strategy;
use crate::value::Value;

/// An isolate as a bridge's conversions see it. Object-safe, so one native
/// library serves isolates of every memory strategy.
///
/// The methods taking a [`Value`] are `unsafe`: the value must be one the
/// bridge was called with, or one these methods made during the call.
pub trait Isolate {
    /// The number `v` is, if it is one.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn number(&self, v: Value) -> Option<Number>;
    /// `n` as a value.
    fn number_value(&mut self, n: &Number) -> Result<Value, Error>;
    /// The text of `v`, if it is a string.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn string(&self, v: Value) -> Option<String>;
    /// A new string.
    fn string_value(&mut self, s: &str) -> Result<Value, Error>;
    /// The bytes of `v`, if it is a bytevector.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn bytes(&self, v: Value) -> Option<Vec<u8>>;
    /// A new bytevector.
    fn bytes_value(&mut self, bytes: &[u8]) -> Result<Value, Error>;
    /// The elements of `v`, if it is a proper list.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn list(&self, v: Value) -> Option<Vec<Value>>;
    /// A new list of `items`.
    ///
    /// # Safety
    ///
    /// `items` must be valid values.
    unsafe fn list_value(&mut self, items: &[Value]) -> Result<Value, Error>;
    /// The elements of `v`, if it is a vector.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn vector(&self, v: Value) -> Option<Vec<Value>>;
    /// A new vector of `items`.
    ///
    /// # Safety
    ///
    /// As `list_value`.
    unsafe fn vector_value(&mut self, items: &[Value]) -> Result<Value, Error>;
    /// Holds `v` as a handle.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn root(&mut self, v: Value) -> Handle;
    /// The value `h` holds.
    fn handle_value(&self, h: &Handle) -> Result<Value, Error>;
    /// Whether `v` is a Rust object of the type `id`.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn is_object(&self, v: Value, id: TypeId) -> bool;
    /// Moves `value` into the isolate's table, as a new Rust object.
    fn object_value(&mut self, value: Box<dyn ErasedObject>) -> Result<Value, Error>;
    /// Borrows the Rust object `v`, which must be of the type `id`, until
    /// [`Isolate::release_object`]: an error if it is not one, or if a
    /// conflicting borrow is held.
    ///
    /// # Safety
    ///
    /// As `number`. The pointer is valid until the borrow is released, and
    /// only as `&mut` if `exclusive`.
    unsafe fn borrow_object(
        &mut self,
        v: Value,
        id: TypeId,
        exclusive: bool,
    ) -> Result<NonNull<dyn ErasedObject>, BorrowError>;
    /// Releases a borrow of the Rust object `v`.
    ///
    /// # Safety
    ///
    /// As `number`, with a borrow held.
    unsafe fn release_object(&mut self, v: Value);
    /// `v` as `write` prints it, cut short, for error messages.
    ///
    /// # Safety
    ///
    /// As `number`.
    unsafe fn written(&self, v: Value) -> String;
}

/// Why a Rust object could not be borrowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorrowError {
    /// The value is not a Rust object of the type asked for.
    WrongType,
    /// A conflicting borrow is held.
    Borrowed,
}

impl<S: Strategy> Isolate for Cx<S> {
    unsafe fn number(&self, v: Value) -> Option<Number> {
        Cx::number(self, v)
    }

    fn number_value(&mut self, n: &Number) -> Result<Value, Error> {
        Ok(self.number_raw(n)?)
    }

    unsafe fn string(&self, v: Value) -> Option<String> {
        // SAFETY: passed on to the caller.
        (v.tag() == vali_abi::Tag::String).then(|| unsafe { self.string_str(v) }.to_owned())
    }

    fn string_value(&mut self, s: &str) -> Result<Value, Error> {
        Ok(self.string_raw(s, false)?)
    }

    unsafe fn bytes(&self, v: Value) -> Option<Vec<u8>> {
        // SAFETY: passed on to the caller.
        unsafe {
            (self.header_type(v) == Some(vali_abi::TypeCode::Bytevector))
                .then(|| self.bytevector_bytes(v).to_vec())
        }
    }

    fn bytes_value(&mut self, bytes: &[u8]) -> Result<Value, Error> {
        Ok(self.bytevector_raw(bytes)?)
    }

    unsafe fn list(&self, v: Value) -> Option<Vec<Value>> {
        crate::primitives::elements(self, "list", v).ok()
    }

    unsafe fn list_value(&mut self, items: &[Value]) -> Result<Value, Error> {
        let mut list = Value::EMPTY_LIST;
        for &item in items.iter().rev() {
            // SAFETY: passed on to the caller; allocating frees nothing.
            list = unsafe { self.cons_raw(item, list) }?;
        }
        Ok(list)
    }

    unsafe fn vector(&self, v: Value) -> Option<Vec<Value>> {
        // SAFETY: passed on to the caller.
        unsafe {
            (self.header_type(v) == Some(vali_abi::TypeCode::Vector)).then(|| {
                (0..self.vector_length(v))
                    .map(|i| self.vector_ref(v, i))
                    .collect()
            })
        }
    }

    unsafe fn vector_value(&mut self, items: &[Value]) -> Result<Value, Error> {
        // SAFETY: passed on to the caller.
        unsafe {
            let v = self.make_vector_raw(items.len(), Value::UNSPECIFIED)?;
            for (i, &item) in items.iter().enumerate() {
                self.vector_set(v, i, item);
            }
            Ok(v)
        }
    }

    unsafe fn root(&mut self, v: Value) -> Handle {
        // SAFETY: passed on to the caller.
        unsafe { Cx::root(self, v) }
    }

    fn handle_value(&self, h: &Handle) -> Result<Value, Error> {
        self.get(h)
    }

    unsafe fn is_object(&self, v: Value, id: TypeId) -> bool {
        Self::rust_object_index(v)
            .and_then(|i| self.objects.entry(i))
            .is_some_and(|entry| entry.type_id() == id)
    }

    fn object_value(&mut self, value: Box<dyn ErasedObject>) -> Result<Value, Error> {
        Ok(self.object_raw(value)?)
    }

    unsafe fn borrow_object(
        &mut self,
        v: Value,
        id: TypeId,
        exclusive: bool,
    ) -> Result<NonNull<dyn ErasedObject>, BorrowError> {
        let entry = Self::rust_object_index(v)
            .and_then(|i| self.objects.entry_mut(i))
            .filter(|entry| entry.type_id() == id)
            .ok_or(BorrowError::WrongType)?;
        entry.borrow = match (entry.borrow, exclusive) {
            (Borrow::Free, true) => Borrow::Exclusive,
            (Borrow::Free, false) => Borrow::Shared(1),
            (Borrow::Shared(n), false) => Borrow::Shared(n + 1),
            _ => return Err(BorrowError::Borrowed),
        };
        Ok(entry.pointer())
    }

    unsafe fn release_object(&mut self, v: Value) {
        if let Some(entry) = Self::rust_object_index(v).and_then(|i| self.objects.entry_mut(i)) {
            entry.borrow = match entry.borrow {
                Borrow::Shared(n) if n > 1 => Borrow::Shared(n - 1),
                _ => Borrow::Free,
            };
        }
    }

    unsafe fn written(&self, v: Value) -> String {
        Cx::written(self, v)
    }
}

/// A Rust type a Scheme value converts to.
pub trait FromScheme: Sized {
    /// What the value must be, for errors: "an exact integer".
    const EXPECTED: &'static str;

    /// `v` as this type, or `None` if it is not one.
    ///
    /// # Safety
    ///
    /// `v` must be a value the bridge was called with.
    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self>;
}

/// A Rust type that converts to a Scheme value.
pub trait IntoScheme {
    /// This as a Scheme value. An error is raised as a condition.
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error>;
}

/// How a parameter takes its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// One argument, which must be given.
    Required,
    /// One argument, which may be left out.
    Optional,
    /// All the remaining arguments.
    Rest,
}

/// A parameter of a bridge: one argument converted through [`FromScheme`],
/// an optional one ([`Option`]), or the rest ([`Rest`]).
pub trait Param: Sized {
    /// How it takes its arguments.
    const KIND: ParamKind;

    /// Takes its arguments from `args` at `*next`, moving `next` past them.
    ///
    /// # Safety
    ///
    /// `args` must be the values the bridge was called with.
    unsafe fn take(cx: &mut dyn Isolate, args: &[Value], next: &mut usize) -> Result<Self, BadArg>;
}

/// An argument that did not convert: its position, from 1, what it had to
/// be, and the value.
#[derive(Clone, Copy, Debug)]
pub struct BadArg {
    position: usize,
    expected: &'static str,
    got: Value,
}

impl<T: FromScheme> Param for T {
    const KIND: ParamKind = ParamKind::Required;

    unsafe fn take(cx: &mut dyn Isolate, args: &[Value], next: &mut usize) -> Result<Self, BadArg> {
        let v = args[*next];
        *next += 1;
        // SAFETY: passed on to the caller.
        unsafe { T::from_scheme(cx, v) }.ok_or(BadArg {
            position: *next,
            expected: T::EXPECTED,
            got: v,
        })
    }
}

impl<T: FromScheme> Param for Option<T> {
    const KIND: ParamKind = ParamKind::Optional;

    unsafe fn take(cx: &mut dyn Isolate, args: &[Value], next: &mut usize) -> Result<Self, BadArg> {
        match args.get(*next).copied().filter(|&v| v != Value::DEFAULT) {
            None => {
                *next += 1;
                Ok(None)
            }
            // SAFETY: passed on to the caller.
            Some(_) => unsafe { T::take(cx, args, next) }.map(Some),
        }
    }
}

/// The remaining arguments, each converted to `T`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rest<T>(pub Vec<T>);

impl<T: FromScheme> Param for Rest<T> {
    const KIND: ParamKind = ParamKind::Rest;

    unsafe fn take(cx: &mut dyn Isolate, args: &[Value], next: &mut usize) -> Result<Self, BadArg> {
        let mut items = Vec::new();
        while *next < args.len() {
            // SAFETY: passed on to the caller.
            items.push(unsafe { T::take(cx, args, next) }?);
        }
        Ok(Rest(items))
    }
}

/// A function's arity, from its parameters' kinds.
fn arity_of(kinds: &[ParamKind]) -> Arity {
    let count = |k| kinds.iter().filter(|&&kind| kind == k).count() as u8;
    Arity {
        required: count(ParamKind::Required),
        optional: count(ParamKind::Optional),
        rest: kinds.contains(&ParamKind::Rest),
    }
}

// Conversions.

macro_rules! integers {
    ($($t:ty),*) => {$(
        impl FromScheme for $t {
            const EXPECTED: &'static str = concat!("an exact integer that fits in ", stringify!($t));

            unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
                if let Some(n) = v.as_fixnum() {
                    return <$t>::try_from(n).ok();
                }
                // SAFETY: passed on to the caller.
                match unsafe { cx.number(v) }? {
                    Number::Integer(n) => n.to_i128().and_then(|n| <$t>::try_from(n).ok()),
                    _ => None,
                }
            }
        }

        impl IntoScheme for $t {
            fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
                match i64::try_from(self).ok().and_then(Value::fixnum) {
                    Some(v) => Ok(v),
                    None => cx.number_value(&Number::from(self)),
                }
            }
        }
    )*};
}

integers!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl FromScheme for f64 {
    const EXPECTED: &'static str = "a real number";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        match vali_num::arith::to_inexact(&unsafe { cx.number(v) }?) {
            Number::Real(x) => Some(x),
            _ => None,
        }
    }
}

impl IntoScheme for f64 {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.number_value(&Number::Real(self))
    }
}

impl FromScheme for Number {
    const EXPECTED: &'static str = "a number";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        unsafe { cx.number(v) }
    }
}

impl IntoScheme for Number {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.number_value(&self)
    }
}

impl FromScheme for bool {
    const EXPECTED: &'static str = "a boolean";

    unsafe fn from_scheme(_cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        v.as_boolean()
    }
}

impl IntoScheme for bool {
    fn into_scheme(self, _cx: &mut dyn Isolate) -> Result<Value, Error> {
        Ok(Value::boolean(self))
    }
}

impl FromScheme for char {
    const EXPECTED: &'static str = "a character";

    unsafe fn from_scheme(_cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        v.as_character()
    }
}

impl IntoScheme for char {
    fn into_scheme(self, _cx: &mut dyn Isolate) -> Result<Value, Error> {
        Ok(Value::character(self))
    }
}

impl FromScheme for String {
    const EXPECTED: &'static str = "a string";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        unsafe { cx.string(v) }
    }
}

impl IntoScheme for String {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.string_value(&self)
    }
}

impl IntoScheme for &str {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.string_value(self)
    }
}

/// A bytevector's bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl FromScheme for Bytes {
    const EXPECTED: &'static str = "a bytevector";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        unsafe { cx.bytes(v) }.map(Bytes)
    }
}

impl IntoScheme for Bytes {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.bytes_value(&self.0)
    }
}

impl FromScheme for Handle {
    const EXPECTED: &'static str = "a value";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        Some(unsafe { cx.root(v) })
    }
}

impl IntoScheme for Handle {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.handle_value(&self)
    }
}

impl IntoScheme for () {
    fn into_scheme(self, _cx: &mut dyn Isolate) -> Result<Value, Error> {
        Ok(Value::UNSPECIFIED)
    }
}

/// The end-of-file object, as `read-char` returns at the end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Eof;

impl IntoScheme for Eof {
    fn into_scheme(self, _cx: &mut dyn Isolate) -> Result<Value, Error> {
        Ok(Value::EOF)
    }
}

/// A proper list, each element converted to `T`.
impl<T: FromScheme> FromScheme for Vec<T> {
    const EXPECTED: &'static str = "a proper list";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller; the elements are its values.
        unsafe {
            cx.list(v)?
                .into_iter()
                .map(|item| T::from_scheme(cx, item))
                .collect()
        }
    }
}

/// A list of the elements.
impl<T: IntoScheme> IntoScheme for Vec<T> {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        let items = self
            .into_iter()
            .map(|item| item.into_scheme(cx))
            .collect::<Result<Vec<_>, _>>()?;
        // SAFETY: the conversions made these values; allocating frees
        // nothing.
        unsafe { cx.list_value(&items) }
    }
}

/// A vector, each element converted to `T`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vector<T>(pub Vec<T>);

impl<T: FromScheme> FromScheme for Vector<T> {
    const EXPECTED: &'static str = "a vector";

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller; the elements are its values.
        unsafe {
            cx.vector(v)?
                .into_iter()
                .map(|item| T::from_scheme(cx, item))
                .collect::<Option<Vec<T>>>()
                .map(Vector)
        }
    }
}

impl<T: IntoScheme> IntoScheme for Vector<T> {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        let items = self
            .0
            .into_iter()
            .map(|item| item.into_scheme(cx))
            .collect::<Result<Vec<_>, _>>()?;
        // SAFETY: as for lists.
        unsafe { cx.vector_value(&items) }
    }
}

/// `None` is `#f`.
impl<T: IntoScheme> IntoScheme for Option<T> {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        match self {
            Some(x) => x.into_scheme(cx),
            None => Ok(Value::FALSE),
        }
    }
}

/// An error is raised as a condition, its text the message.
impl<T: IntoScheme, E: fmt::Display> IntoScheme for Result<T, E> {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        match self {
            Ok(x) => x.into_scheme(cx),
            Err(e) => Err(Error::scheme(e.to_string())),
        }
    }
}

/// A Rust value moves into the isolate's table.
impl<T: RustObject> IntoScheme for T {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.object_value(Box::new(self))
    }
}

/// A typed handle to a Rust object of type `T`: to keep it beyond a call,
/// or to return it again. Its value is reached through the isolate
/// ([`Cx::with`]).
pub struct Obj<T: RustObject> {
    handle: Handle,
    marker: PhantomData<fn() -> T>,
}

impl<T: RustObject> Clone for Obj<T> {
    fn clone(&self) -> Self {
        Obj {
            handle: self.handle.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: RustObject> fmt::Debug for Obj<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Obj<{}>({:?})", T::NAME, self.handle)
    }
}

impl<T: RustObject> Obj<T> {
    /// The handle.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl<T: RustObject> FromScheme for Obj<T> {
    const EXPECTED: &'static str = T::NAME;

    unsafe fn from_scheme(cx: &mut dyn Isolate, v: Value) -> Option<Self> {
        // SAFETY: passed on to the caller.
        unsafe {
            cx.is_object(v, TypeId::of::<T>()).then(|| Obj {
                handle: cx.root(v),
                marker: PhantomData,
            })
        }
    }
}

impl<T: RustObject> IntoScheme for Obj<T> {
    fn into_scheme(self, cx: &mut dyn Isolate) -> Result<Value, Error> {
        cx.handle_value(&self.handle)
    }
}

impl<S: Strategy> Cx<S> {
    /// Calls `f` with the Rust object `obj` refers to.
    pub fn with<T: RustObject, R>(
        &mut self,
        obj: &Obj<T>,
        f: impl FnOnce(&mut T) -> R,
    ) -> Result<R, Error> {
        self.with_object(&obj.handle, f)
    }
}

// Functions and methods.

type Call = dyn Fn(&mut dyn Isolate, &[Value]) -> Result<Value, Error> + Send + Sync;

/// A Rust function exposed to Scheme: its name, arity and conversions.
pub struct NativeFunction {
    pub(crate) name: Box<str>,
    pub(crate) arity: Arity,
    call: Box<Call>,
}

impl fmt::Debug for NativeFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeFunction")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .finish_non_exhaustive()
    }
}

impl NativeFunction {
    /// Its Scheme name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The arguments it takes.
    pub fn arity(&self) -> Arity {
        self.arity
    }

    /// Calls it with `args`, which the VM has checked against its arity.
    /// A panic becomes an error naming the function.
    pub(crate) fn invoke(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error> {
        match catch_unwind(AssertUnwindSafe(|| (self.call)(cx, args))) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(Error::Scheme(mut e))) => {
                if !e.message.starts_with(&*self.name) {
                    e.message = format!("{}: {}", self.name, e.message);
                }
                Err(Error::Scheme(e))
            }
            Ok(Err(e)) => Err(Error::scheme(format!("{}: {e}", self.name))),
            Err(panic) => {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a panic".into());
                Err(Error::scheme(format!("{}: panicked: {what}", self.name)))
            }
        }
    }
}

/// A conversion failure as an error.
fn bad_arg(cx: &dyn Isolate, bad: BadArg) -> Error {
    // SAFETY: the value is one the bridge was called with.
    let got = unsafe { cx.written(bad.got) };
    Error::scheme(format!(
        "argument {}: expected {}, got {got}",
        bad.position, bad.expected
    ))
}

/// A Rust function a bridge can call: `Fn(A, ...) -> R` with each `A` a
/// [`Param`] and `R` an [`IntoScheme`]. `M` tells the implementations
/// apart.
pub trait IntoFunction<M>: Send + Sync + 'static {
    /// Its arity.
    fn arity() -> Arity;
    /// Converts the arguments, calls it, and converts the result.
    ///
    /// # Safety
    ///
    /// `args` must be the values the bridge was called with.
    unsafe fn call(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error>;
}

/// A Rust method a bridge can call: its receiver `&T` or `&mut T`, then
/// parameters as for [`IntoFunction`].
pub trait IntoMethod<T, M>: Send + Sync + 'static {
    /// Its arity, the receiver included.
    fn arity() -> Arity;
    /// Borrows the receiver, converts the other arguments, calls it, and
    /// converts the result.
    ///
    /// # Safety
    ///
    /// As [`IntoFunction::call`].
    unsafe fn call(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error>;
}

/// Marks a method whose receiver is `&mut T`.
#[derive(Debug)]
pub struct Exclusive;
/// Marks a method whose receiver is `&T`.
#[derive(Debug)]
pub struct Shared;

macro_rules! functions {
    ($($a:ident),*) => {
        impl<F, R, $($a,)*> IntoFunction<(R, $($a,)*)> for F
        where
            F: Fn($($a),*) -> R + Send + Sync + 'static,
            R: IntoScheme,
            $($a: Param,)*
        {
            fn arity() -> Arity {
                arity_of(&[$($a::KIND),*])
            }

            #[allow(non_snake_case, unused_mut, unused_variables)]
            unsafe fn call(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error> {
                let mut next = 0;
                // SAFETY: passed on to the caller.
                $(let $a = unsafe { $a::take(cx, args, &mut next) }.map_err(|bad| bad_arg(cx, bad))?;)*
                (self)($($a),*).into_scheme(cx)
            }
        }

        impl<T, F, R, $($a,)*> IntoMethod<T, (Exclusive, R, $($a,)*)> for F
        where
            T: RustObject,
            F: for<'a> Fn(&'a mut T, $($a),*) -> R + Send + Sync + 'static,
            R: IntoScheme,
            $($a: Param,)*
        {
            fn arity() -> Arity {
                arity_of(&[ParamKind::Required, $($a::KIND),*])
            }

            #[allow(non_snake_case, unused_mut, unused_variables)]
            unsafe fn call(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error> {
                let mut next = 1;
                // SAFETY: passed on to the caller.
                $(let $a = unsafe { $a::take(cx, args, &mut next) }.map_err(|bad| bad_arg(cx, bad))?;)*
                // SAFETY: as above; the receiver is borrowed exclusively until
                // it is released below, and the pointer is valid until then.
                let result = unsafe {
                    let mut receiver = receiver(cx, args[0], TypeId::of::<T>(), true, T::NAME)?;
                    let target = receiver
                        .as_mut()
                        .as_any_mut()
                        .downcast_mut::<T>()
                        .expect("the type was checked");
                    let result = catch_unwind(AssertUnwindSafe(|| (self)(target, $($a),*)));
                    cx.release_object(args[0]);
                    result
                };
                match result {
                    Ok(r) => r.into_scheme(cx),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        }

        impl<T, F, R, $($a,)*> IntoMethod<T, (Shared, R, $($a,)*)> for F
        where
            T: RustObject,
            F: for<'a> Fn(&'a T, $($a),*) -> R + Send + Sync + 'static,
            R: IntoScheme,
            $($a: Param,)*
        {
            fn arity() -> Arity {
                arity_of(&[ParamKind::Required, $($a::KIND),*])
            }

            #[allow(non_snake_case, unused_mut, unused_variables)]
            unsafe fn call(&self, cx: &mut dyn Isolate, args: &[Value]) -> Result<Value, Error> {
                let mut next = 1;
                // SAFETY: passed on to the caller.
                $(let $a = unsafe { $a::take(cx, args, &mut next) }.map_err(|bad| bad_arg(cx, bad))?;)*
                // SAFETY: as above, borrowed shared.
                let result = unsafe {
                    let receiver = receiver(cx, args[0], TypeId::of::<T>(), false, T::NAME)?;
                    let target = receiver
                        .as_ref()
                        .as_any()
                        .downcast_ref::<T>()
                        .expect("the type was checked");
                    let result = catch_unwind(AssertUnwindSafe(|| (self)(target, $($a),*)));
                    cx.release_object(args[0]);
                    result
                };
                match result {
                    Ok(r) => r.into_scheme(cx),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        }
    };
}

functions!();
functions!(A1);
functions!(A1, A2);
functions!(A1, A2, A3);
functions!(A1, A2, A3, A4);
functions!(A1, A2, A3, A4, A5);
functions!(A1, A2, A3, A4, A5, A6);

/// Borrows a method's receiver, or an error saying why it cannot be.
///
/// # Safety
///
/// As [`Isolate::borrow_object`].
unsafe fn receiver(
    cx: &mut dyn Isolate,
    v: Value,
    id: TypeId,
    exclusive: bool,
    name: &'static str,
) -> Result<NonNull<dyn ErasedObject>, Error> {
    // SAFETY: passed on to the caller.
    unsafe { cx.borrow_object(v, id, exclusive) }.map_err(|e| match e {
        BorrowError::WrongType => bad_arg(
            cx,
            BadArg {
                position: 1,
                expected: name,
                got: v,
            },
        ),
        BorrowError::Borrowed => Error::scheme(format!("{name} is already borrowed")),
    })
}

/// A library of Rust functions and methods, built once and bound into any
/// number of isolates (ADR 0016).
#[derive(Debug, Default)]
pub struct NativeLibrary {
    name: Vec<String>,
    functions: Vec<Arc<NativeFunction>>,
}

impl NativeLibrary {
    /// An empty library named `name`, as in `(import (vali ports))`.
    pub fn new(name: &[&str]) -> NativeLibrary {
        NativeLibrary {
            name: name.iter().map(|&part| part.to_owned()).collect(),
            functions: Vec::new(),
        }
    }

    /// Its name.
    pub fn name(&self) -> &[String] {
        &self.name
    }

    /// Its functions, in the order they were added.
    pub fn functions(&self) -> &[Arc<NativeFunction>] {
        &self.functions
    }

    fn add(mut self, name: &str, arity: Arity, call: Box<Call>) -> NativeLibrary {
        self.functions.push(Arc::new(NativeFunction {
            name: name.into(),
            arity,
            call,
        }));
        self
    }

    /// Adds the function `f`, as `name`.
    pub fn function<M, F: IntoFunction<M>>(self, name: &str, f: F) -> NativeLibrary {
        // SAFETY: the VM calls it with the values it was called with.
        self.add(
            name,
            F::arity(),
            Box::new(move |cx, args| unsafe { f.call(cx, args) }),
        )
    }

    /// Adds the method `f`, whose receiver is a `T`, as `name`.
    pub fn method<T: RustObject, M, F: IntoMethod<T, M>>(self, name: &str, f: F) -> NativeLibrary {
        // SAFETY: as above.
        self.add(
            name,
            F::arity(),
            Box::new(move |cx, args| unsafe { f.call(cx, args) }),
        )
    }

    /// Adds `name`, a predicate true of the Rust objects of type `T`.
    pub fn predicate<T: RustObject>(self, name: &str) -> NativeLibrary {
        self.add(
            name,
            Arity::exactly(1),
            // SAFETY: the value is one the bridge was called with.
            Box::new(|cx, args| {
                Ok(Value::boolean(unsafe {
                    cx.is_object(args[0], TypeId::of::<T>())
                }))
            }),
        )
    }
}
