//! Syntax objects in the heap (ADR 0015 #13), conversions between heap
//! values and datums, and the host's data slot.
//!
//! A syntax object is a Rust [`Syntax`]. The heap object of type `Syntax`
//! holds an index into the context's table of them, so Scheme code can
//! hold, pass and store syntax objects while Rust keeps their scopes and
//! pending changes. The table only grows: nothing is collected yet.

use std::any::Any;
use std::collections::HashSet;

use vali_abi::{Tag, TypeCode};
use vali_syntax::{Datum, DatumKind, ScopeSetId, Span, Syntax};

use crate::cx::{Cx, get, put};
use crate::handle::{Error, Handle};
use crate::page::OutOfMemory;
use crate::strategy::Strategy;
use crate::value::Value;

impl<S: Strategy> Cx<S> {
    /// A new heap object for `syntax`.
    pub fn syntax_raw(&mut self, syntax: Syntax) -> Result<Value, OutOfMemory> {
        let index = Value::fixnum(self.syntaxes.len() as i64).expect("an index fits a fixnum");
        let v = self.new_object(TypeCode::Syntax, 1, 0)?;
        // SAFETY: the object was just allocated with its one field.
        unsafe { put(v, Self::LAYOUT.field(TypeCode::Syntax, 0), index.raw()) };
        self.syntaxes.push(syntax);
        Ok(v)
    }

    /// A new heap object for `syntax`, as a handle.
    pub fn syntax(&mut self, syntax: Syntax) -> Result<Handle, Error> {
        let v = self.syntax_raw(syntax)?;
        // SAFETY: just made.
        Ok(unsafe { self.root(v) })
    }

    /// The syntax object `v` is, if it is one.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub unsafe fn syntax_value(&self, v: Value) -> Option<&Syntax> {
        if !self.is_other(v, TypeCode::Syntax) {
            return None;
        }
        // SAFETY: `v` is a live syntax object, whose field is its index.
        let index = Value::from_raw(unsafe { get(v, Self::LAYOUT.field(TypeCode::Syntax, 0)) });
        self.syntaxes.get(usize::try_from(index.as_fixnum()?).ok()?)
    }

    /// The datum `v` is, with `span` on every part, if it is one: a number,
    /// boolean, character, string, symbol, bytevector, or a list or vector
    /// of datums. Anything else, a procedure or a syntax object or a
    /// circular list, gives `None`.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub unsafe fn value_datum(&self, v: Value, span: Span) -> Option<Datum> {
        // SAFETY: passed on to the caller.
        unsafe { self.datum_of(v, span, &mut HashSet::new()) }
    }

    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn datum_of(&self, v: Value, span: Span, open: &mut HashSet<u64>) -> Option<Datum> {
        let datum = |kind| Some(Datum { kind, span });
        if let Some(n) = self.number(v) {
            return datum(DatumKind::Number(n));
        }
        if let Some(b) = v.as_boolean() {
            return datum(DatumKind::Boolean(b));
        }
        if let Some(c) = v.as_character() {
            return datum(DatumKind::Character(c));
        }
        if v == Value::EMPTY_LIST {
            return datum(DatumKind::List {
                items: Vec::new(),
                tail: None,
            });
        }
        match v.tag() {
            // SAFETY: valid values with these tags are live objects of the
            // type the tag names.
            Tag::String => datum(DatumKind::String(unsafe { self.string_str(v) }.to_owned())),
            Tag::Symbol => datum(DatumKind::Symbol(unsafe { self.symbol_name(v) }.to_owned())),
            Tag::Pair => {
                // Lists are walked along their spine; a pair met again on
                // the way is a cycle, which no datum can be.
                let mut items = Vec::new();
                let mut rest = v;
                let mut spine = Vec::new();
                while rest.is_pair() {
                    if !open.insert(rest.address()) {
                        return None;
                    }
                    spine.push(rest.address());
                    // SAFETY: `rest` is a live pair.
                    let (car, cdr) = unsafe { (self.car(rest), self.cdr(rest)) };
                    // SAFETY: fields of live objects are valid values.
                    items.push(unsafe { self.datum_of(car, span, open) }?);
                    rest = cdr;
                }
                let tail = if rest == Value::EMPTY_LIST {
                    None
                } else {
                    // SAFETY: as above.
                    Some(Box::new(unsafe { self.datum_of(rest, span, open) }?))
                };
                for address in spine {
                    open.remove(&address);
                }
                datum(DatumKind::List { items, tail })
            }
            Tag::Other if self.keyword_name(v).is_some() => datum(DatumKind::Keyword(
                self.keyword_name(v).expect("a keyword").to_owned(),
            )),
            Tag::Other if self.is_other(v, TypeCode::Bytevector) => {
                // SAFETY: `v` is a live bytevector.
                datum(DatumKind::Bytevector(
                    unsafe { self.bytevector_bytes(v) }.to_vec(),
                ))
            }
            Tag::Other if self.is_other(v, TypeCode::Vector) => {
                if !open.insert(v.address()) {
                    return None;
                }
                // SAFETY: `v` is a live vector, and its elements are valid.
                let items = (0..unsafe { self.vector_length(v) })
                    .map(|i| unsafe { self.datum_of(self.vector_ref(v, i), span, open) })
                    .collect::<Option<Vec<_>>>()?;
                open.remove(&v.address());
                datum(DatumKind::Vector(items))
            }
            _ => None,
        }
    }

    /// `v` as a syntax object. A syntax object is itself; lists and vectors
    /// of syntax objects and datums become syntax objects whose parts are
    /// those, as R6RS lets a transformer return; other datums get `scopes`
    /// and `span`. Values that are neither give `None`.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub unsafe fn value_syntax(&self, v: Value, scopes: ScopeSetId, span: Span) -> Option<Syntax> {
        // SAFETY: passed on to the caller.
        unsafe { self.syntax_of(v, scopes, span, &mut HashSet::new()) }
    }

    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn syntax_of(
        &self,
        v: Value,
        scopes: ScopeSetId,
        span: Span,
        open: &mut HashSet<u64>,
    ) -> Option<Syntax> {
        // SAFETY: `v` is valid.
        if let Some(syntax) = unsafe { self.syntax_value(v) } {
            return Some(syntax.clone());
        }
        if v.is_pair() {
            let mut items = Vec::new();
            let mut rest = v;
            let mut spine = Vec::new();
            while rest.is_pair() {
                if !open.insert(rest.address()) {
                    return None;
                }
                spine.push(rest.address());
                // SAFETY: `rest` is a live pair, whose fields are valid.
                let (car, cdr) = unsafe { (self.car(rest), self.cdr(rest)) };
                items.push(unsafe { self.syntax_of(car, scopes, span, open) }?);
                rest = cdr;
            }
            let tail = if rest == Value::EMPTY_LIST {
                None
            } else {
                // SAFETY: as above.
                Some(unsafe { self.syntax_of(rest, scopes, span, open) }?)
            };
            for address in spine {
                open.remove(&address);
            }
            return Some(Syntax::list(items, tail, scopes, span, None));
        }
        if self.is_other(v, TypeCode::Vector) {
            if !open.insert(v.address()) {
                return None;
            }
            // SAFETY: `v` is a live vector, and its elements are valid.
            let items = (0..unsafe { self.vector_length(v) })
                .map(|i| unsafe { self.syntax_of(self.vector_ref(v, i), scopes, span, open) })
                .collect::<Option<Vec<_>>>()?;
            open.remove(&v.address());
            return Some(Syntax::vector(items, scopes, span, None));
        }
        // SAFETY: `v` is valid.
        let datum = unsafe { self.datum_of(v, span, open) }?;
        Some(Syntax::from_datum(&datum, scopes))
    }

    /// Keeps `data` for the host's Rust procedures to reach with
    /// [`Cx::host_data`], returning what was kept before.
    pub fn set_host_data(
        &mut self,
        data: Option<Box<dyn Any + Send>>,
    ) -> Option<Box<dyn Any + Send>> {
        std::mem::replace(&mut self.host_data, data)
    }

    /// Keeps the expander's data, which the syntax procedures use, apart
    /// from the host's, returning what was kept before.
    pub fn set_expander_data(
        &mut self,
        data: Option<Box<dyn Any + Send>>,
    ) -> Option<Box<dyn Any + Send>> {
        std::mem::replace(&mut self.expander_data, data)
    }

    /// Keeps what `eval` needs, apart from the host's data, returning what
    /// was kept before. The compiler sets it; the runtime only holds it.
    #[doc(hidden)]
    pub fn set_evaluator(
        &mut self,
        data: Option<Box<dyn Any + Send>>,
    ) -> Option<Box<dyn Any + Send>> {
        std::mem::replace(&mut self.evaluator, data)
    }

    /// The host's data, if it is a `T`.
    pub fn host_data<T: Any>(&mut self) -> Option<&mut T> {
        let data: &mut dyn Any = self.host_data.as_mut()?.as_mut();
        data.downcast_mut()
    }
}
