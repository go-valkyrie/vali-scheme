//! Reading Scheme text straight into the heap (ADR 0014 #4): the runtime's
//! datum builder for the reader's datum sink.

use std::collections::{HashMap, HashSet};

use vali_abi::TypeCode;
use vali_num::Number;
use vali_syntax::{DatumBuilder, SourceId, Span};

use crate::cx::Cx;
use crate::handle::{Error, Handle};
use crate::strategy::Strategy;
use crate::value::Value;

/// Builds heap values from the reader's events. Allocation can fail but the
/// builder's methods cannot, so the first failure is kept and reported
/// after reading.
struct HeapBuilder<'a, S: Strategy> {
    cx: &'a mut Cx<S>,
    error: Option<Error>,
    /// Labels already defined, by number.
    defined: HashMap<u64, Value>,
    /// Placeholders for labels referred to inside their own definition.
    pending: HashMap<u64, Value>,
}

impl<S: Strategy> HeapBuilder<'_, S> {
    fn fail(&mut self, error: Error) -> Value {
        self.error.get_or_insert(error);
        Value::FALSE
    }

    fn attempt(&mut self, result: Result<Value, impl Into<Error>>) -> Value {
        match result {
            Ok(v) => v,
            Err(e) => self.fail(e.into()),
        }
    }

    /// Replaces every reference to `placeholder` inside `datum` with
    /// `datum` itself, closing the cycle a label made.
    fn patch(&mut self, datum: Value, placeholder: Value) {
        let mut seen = HashSet::new();
        let mut stack = vec![datum];
        while let Some(v) = stack.pop() {
            if !seen.insert(v.raw()) {
                continue;
            }
            // SAFETY: values the builder made are live; pairs and vectors
            // are checked before their fields are read or written.
            unsafe {
                if v.is_pair() {
                    let (car, cdr) = (self.cx.car(v), self.cx.cdr(v));
                    if car == placeholder {
                        self.cx.set_car(v, datum);
                    } else {
                        stack.push(car);
                    }
                    if cdr == placeholder {
                        self.cx.set_cdr(v, datum);
                    } else {
                        stack.push(cdr);
                    }
                } else if self.cx.header_type(v) == Some(TypeCode::Vector) {
                    for i in 0..self.cx.vector_length(v) {
                        let item = self.cx.vector_ref(v, i);
                        if item == placeholder {
                            self.cx.vector_set(v, i, datum);
                        } else {
                            stack.push(item);
                        }
                    }
                }
            }
        }
    }
}

impl<S: Strategy> DatumBuilder for HeapBuilder<'_, S> {
    type Datum = Value;

    fn number(&mut self, value: Number, _span: Span) -> Value {
        match value {
            Number::Integer(n) => {
                let result = self.cx.integer_raw(&n);
                self.attempt(result)
            }
            Number::Real(x) => {
                let result = self.cx.flonum_raw(x);
                self.attempt(result)
            }
            _ => self.fail(Error::scheme(
                "read: rationals and complex numbers are not supported in the heap yet",
            )),
        }
    }

    fn boolean(&mut self, value: bool, _span: Span) -> Value {
        Value::boolean(value)
    }

    fn character(&mut self, value: char, _span: Span) -> Value {
        Value::character(value)
    }

    fn string(&mut self, value: String, _span: Span) -> Value {
        let result = self.cx.string_raw(&value, false);
        self.attempt(result)
    }

    fn symbol(&mut self, name: &str, _span: Span) -> Value {
        let result = self.cx.intern(name);
        self.attempt(result)
    }

    fn keyword(&mut self, _name: &str, _span: Span) -> Value {
        self.fail(Error::scheme(
            "read: keywords are not supported in the heap yet",
        ))
    }

    fn list(&mut self, items: Vec<Value>, tail: Option<Value>, _span: Span) -> Value {
        let mut list = tail.unwrap_or(Value::EMPTY_LIST);
        for item in items.into_iter().rev() {
            // SAFETY: both are values the builder made.
            let result = unsafe { self.cx.cons_raw(item, list) };
            list = self.attempt(result);
        }
        list
    }

    fn vector(&mut self, items: Vec<Value>, _span: Span) -> Value {
        // SAFETY: the fill is a valid value.
        let result = unsafe { self.cx.make_vector_raw(items.len(), Value::FALSE) };
        let v = self.attempt(result);
        if self.error.is_none() {
            for (i, item) in items.into_iter().enumerate() {
                // SAFETY: `v` has `items.len()` elements.
                unsafe { self.cx.vector_set(v, i, item) };
            }
        }
        v
    }

    fn bytevector(&mut self, bytes: Vec<u8>, _span: Span) -> Value {
        let result = self.cx.bytevector_raw(&bytes);
        self.attempt(result)
    }

    fn label_def(&mut self, label: u64, datum: Value, _span: Span) -> Value {
        if let Some(placeholder) = self.pending.remove(&label) {
            self.patch(datum, placeholder);
        }
        self.defined.insert(label, datum);
        datum
    }

    fn label_ref(&mut self, label: u64, _span: Span) -> Value {
        if let Some(&v) = self.defined.get(&label) {
            return v;
        }
        if let Some(&placeholder) = self.pending.get(&label) {
            return placeholder;
        }
        // A fresh box stands in until the label's datum exists.
        let result = self.cx.new_object(TypeCode::Box, 1, 0);
        let placeholder = self.attempt(result);
        self.pending.insert(label, placeholder);
        placeholder
    }
}

impl<S: Strategy> Cx<S> {
    /// Reads every datum in `source` into this isolate's heap, as `read`
    /// does: strings are fresh and mutable, symbols are interned, and datum
    /// labels build shared and circular structure.
    pub fn read(&mut self, source: &str) -> Result<Vec<Handle>, Error> {
        let mut builder = HeapBuilder {
            cx: self,
            error: None,
            defined: HashMap::new(),
            pending: HashMap::new(),
        };
        let read = vali_syntax::read_with(source, SourceId(0), &mut builder);
        if let Some(error) = builder.error {
            return Err(error);
        }
        if let Some(e) = read.errors.first() {
            return Err(Error::scheme(format!(
                "read: {} at byte {}",
                e.kind,
                u32::from(e.range.start())
            )));
        }
        Ok(read
            .datums
            .into_iter()
            // SAFETY: the builder made these values.
            .map(|v| unsafe { self.root(v) })
            .collect())
    }

    /// The type code of a heap object under the "other" tag, if `v` is one.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub(crate) unsafe fn header_type(&self, v: Value) -> Option<TypeCode> {
        if v.tag() != vali_abi::Tag::Other {
            return None;
        }
        // SAFETY: passed on to the caller.
        unsafe { self.header(v, TypeCode::Record) }.and_then(vali_abi::Header::type_code)
    }
}
