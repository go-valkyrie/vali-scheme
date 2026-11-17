//! Rust objects (ADR 0016): Rust values exposed to Scheme.
//!
//! A Rust object lives boxed in its isolate's table; a heap object of type
//! `RustObject`, a header and a table index, stands for it in Scheme. The
//! value never moves, so a moving collector may move the heap object freely.
//! When the memory strategy frees the heap object, at an epoch or in a
//! cycle collection, the table drops the value; whatever is left is dropped
//! with the isolate. `Drop` runs with no access to the isolate, so it cannot
//! call Scheme.
//!
//! Each entry carries a borrow state, checked whenever Rust borrows the
//! value through a handle or a bridge argument, so a conflicting borrow is
//! an error, never two references that alias.

use std::any::Any;
use std::fmt;

use vali_abi::{Header, Tag, TypeCode};

use crate::cx::{Cx, get, put};
use crate::handle::{Error, Handle};
use crate::page::OutOfMemory;
use crate::strategy::Strategy;
use crate::value::Value;

/// A Rust type whose values Scheme can hold.
///
/// It is `Send`, since an isolate is, and holds Scheme values only through
/// handles (ADR 0003), which keep what they refer to alive.
pub trait RustObject: Any + Send {
    /// The name Scheme shows it by: `#<NAME>`, and in errors.
    const NAME: &'static str;

    /// How `write` and `display` show it. By default `#<NAME>`, never with
    /// an address (ADR 0001).
    fn write(&self, out: &mut String) {
        out.push_str("#<");
        out.push_str(Self::NAME);
        out.push('>');
    }
}

/// A Rust object with its type erased, as the table keeps it and bridges'
/// conversions see it. Every [`RustObject`] is one.
#[doc(hidden)]
pub trait ErasedObject: Any + Send {
    /// How `write` shows it.
    fn write_to(&self, out: &mut String);
    /// It, to downcast.
    fn as_any(&self) -> &dyn Any;
    /// It, to downcast and change.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: RustObject> ErasedObject for T {
    fn write_to(&self, out: &mut String) {
        self.write(out);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// How an entry is borrowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Borrow {
    #[default]
    Free,
    /// Borrowed shared this many times.
    Shared(u32),
    Exclusive,
}

pub(crate) struct Entry {
    pub(crate) value: Box<dyn ErasedObject>,
    pub(crate) borrow: Borrow,
}

impl Entry {
    /// The value's type.
    pub(crate) fn type_id(&self) -> std::any::TypeId {
        self.value.as_any().type_id()
    }

    /// The value, to borrow while the entry's borrow state says so.
    pub(crate) fn pointer(&mut self) -> std::ptr::NonNull<dyn ErasedObject> {
        std::ptr::NonNull::from(&mut *self.value)
    }
}

/// An isolate's Rust objects, by index. Freed indices are reused.
#[derive(Default)]
pub(crate) struct Objects {
    entries: Vec<Option<Entry>>,
    free: Vec<usize>,
}

impl fmt::Debug for Objects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Objects")
            .field("live", &self.live())
            .finish()
    }
}

impl Objects {
    pub(crate) fn insert(&mut self, value: Box<dyn ErasedObject>) -> usize {
        let entry = Some(Entry {
            value,
            borrow: Borrow::Free,
        });
        match self.free.pop() {
            Some(i) => {
                self.entries[i] = entry;
                i
            }
            None => {
                self.entries.push(entry);
                self.entries.len() - 1
            }
        }
    }

    /// Drops the value at `i`, whose heap object is gone.
    pub(crate) fn remove(&mut self, i: usize) {
        if self.entries.get_mut(i).and_then(Option::take).is_some() {
            self.free.push(i);
        }
    }

    pub(crate) fn entry(&self, i: usize) -> Option<&Entry> {
        self.entries.get(i).and_then(Option::as_ref)
    }

    pub(crate) fn entry_mut(&mut self, i: usize) -> Option<&mut Entry> {
        self.entries.get_mut(i).and_then(Option::as_mut)
    }

    /// How many Rust objects are alive.
    pub(crate) fn live(&self) -> usize {
        self.entries.len() - self.free.len()
    }
}

impl<S: Strategy> Cx<S> {
    /// The table index of `v`, if it is a Rust object.
    pub(crate) fn rust_object_index(v: Value) -> Option<usize> {
        if v.tag() != Tag::Other {
            return None;
        }
        let t = TypeCode::RustObject;
        let header = Self::LAYOUT.header(t)?;
        // SAFETY: a valid value tagged "other" is a live object starting
        // with its header; a Rust object's one field is its index.
        unsafe {
            (Header(get(v, header)).type_code() == Some(t))
                .then(|| get(v, Self::LAYOUT.field(t, 0)) as usize)
        }
    }

    /// The table index of `v`, if it is a Rust object or a continuation,
    /// whose record the table holds too.
    pub(crate) fn table_index(v: Value) -> Option<usize> {
        if v.tag() != Tag::Other {
            return None;
        }
        let header = Self::LAYOUT.header(TypeCode::RustObject)?;
        // SAFETY: a valid value tagged "other" is a live object starting
        // with its header; both types keep the index in their first field.
        unsafe {
            match Header(get(v, header)).type_code() {
                Some(t @ (TypeCode::RustObject | TypeCode::Continuation)) => {
                    Some(get(v, Self::LAYOUT.field(t, 0)) as usize)
                }
                _ => None,
            }
        }
    }

    /// A new Rust object holding `value`.
    pub(crate) fn object_raw(
        &mut self,
        value: Box<dyn ErasedObject>,
    ) -> Result<Value, OutOfMemory> {
        let t = TypeCode::RustObject;
        let v = self.new_object(t, 1, 0)?;
        let i = self.objects.insert(value);
        // SAFETY: just allocated with room for the index.
        unsafe { put(v, Self::LAYOUT.field(t, 0), i as u64) };
        Ok(v)
    }

    /// Drops the Rust value `v` stands for, if it is a Rust object, or its
    /// record, if it is a continuation: the strategy is freeing it.
    pub(crate) fn finalize(&mut self, v: Value) {
        if let Some(i) = Self::table_index(v) {
            self.objects.remove(i);
        }
    }

    /// How `write` shows the Rust object at `i`.
    pub(crate) fn write_object(&self, i: usize, out: &mut String) {
        match self.objects.entry(i) {
            Some(entry) => entry.value.write_to(out),
            None => out.push_str("#<freed>"),
        }
    }

    /// Moves `value` into the isolate, as a Scheme value.
    pub fn object<T: RustObject>(&mut self, value: T) -> Result<Handle, Error> {
        let v = self.object_raw(Box::new(value))?;
        // SAFETY: just allocated.
        Ok(unsafe { self.root(v) })
    }

    /// Whether `h` is a Rust object of type `T`.
    pub fn is_object<T: RustObject>(&self, h: &Handle) -> bool {
        self.get(h)
            .ok()
            .and_then(Self::rust_object_index)
            .and_then(|i| self.objects.entry(i))
            .is_some_and(|entry| entry.value.as_any().is::<T>())
    }

    /// Calls `f` with the Rust object of type `T` that `h` is. An error if
    /// it is not one, or if it is already borrowed.
    pub fn with_object<T: RustObject, R>(
        &mut self,
        h: &Handle,
        f: impl FnOnce(&mut T) -> R,
    ) -> Result<R, Error> {
        let not_one = || Error::WrongType { expected: T::NAME };
        let i = Self::rust_object_index(self.get(h)?).ok_or_else(not_one)?;
        let entry = self.objects.entry_mut(i).ok_or_else(not_one)?;
        if entry.borrow != Borrow::Free {
            return Err(Error::scheme(format!("{} is already borrowed", T::NAME)));
        }
        let value = entry
            .value
            .as_any_mut()
            .downcast_mut::<T>()
            .ok_or_else(not_one)?;
        Ok(f(value))
    }

    /// How many Rust objects the isolate holds.
    pub fn live_objects(&self) -> usize {
        self.objects.live()
    }
}
