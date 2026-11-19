//! Syntax objects in the heap (ADR 0015 #13), conversions between heap
//! values and datums, and the host's data slot.
//!
//! A syntax object is a Rust [`Syntax`]. The heap object of type `Syntax`
//! holds an index into the context's table of them, so Scheme code can
//! hold, pass and store syntax objects while Rust keeps their scopes and
//! pending changes. The table only grows: nothing is collected yet.

use std::any::Any;
use std::collections::{HashMap, HashSet};

use vali_abi::{Tag, TypeCode};
use vali_syntax::{Datum, DatumKind, ScopeSetId, Span, Syntax};

use crate::cx::{Cx, get, put};
use crate::handle::{Error, Handle};
use crate::strategy::Strategy;
use crate::value::Value;

/// What a value is, for [`Cx::convert`].
enum Node<T> {
    /// Something with no parts to convert: its conversion, or `None` if it
    /// cannot be converted.
    Leaf(Option<T>),
    /// A pair.
    Pair,
    /// A vector.
    Vector,
}

/// A value's shape.
enum Shape {
    Pair,
    Vector,
    Atom,
}

impl<S: Strategy> Cx<S> {
    /// A new heap object for `syntax`.
    pub fn syntax_raw(&mut self, syntax: Syntax) -> Result<Value, Error> {
        if let Some(max) = self.limits.max_syntaxes
            && self.syntaxes.len() >= max
        {
            return Err(Error::LimitExceeded(crate::Limit::Syntaxes));
        }
        self.charge_rust_bytes(crate::cx::SYNTAX_SLOT_BYTES)?;
        let index = Value::fixnum(self.syntaxes.len() as i64).expect("an index fits a fixnum");
        let v = match self.new_object(TypeCode::Syntax, 1, 0) {
            Ok(v) => v,
            Err(e) => {
                self.release_rust_bytes(crate::cx::SYNTAX_SLOT_BYTES);
                return Err(e.into());
            }
        };
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
        let index = unsafe { Value::from_raw(get(v, Self::LAYOUT.field(TypeCode::Syntax, 0))) };
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
        unsafe {
            self.convert(
                v,
                |v| match self.classify(v) {
                    // The datum of a `(quote datum)` is a constant, which
                    // may share parts and be circular.
                    Shape::Pair => match self.quoted_operand(v) {
                        Some(operand) => Node::Leaf(self.quote_datum(operand, span)),
                        None => Node::Pair,
                    },
                    Shape::Vector => Node::Vector,
                    Shape::Atom => Node::Leaf(self.datum_atom(v, span)),
                },
                |items, tail| Datum {
                    kind: DatumKind::List {
                        items,
                        tail: tail.map(Box::new),
                    },
                    span,
                },
                |items| Datum {
                    kind: DatumKind::Vector(items),
                    span,
                },
            )
        }
    }

    /// The datum `v` is, copying shared parts and refusing cycles.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn plain_datum(&self, v: Value, span: Span) -> Option<Datum> {
        // SAFETY: passed on to the caller.
        unsafe {
            self.convert(
                v,
                |v| match self.classify(v) {
                    Shape::Pair => Node::Pair,
                    Shape::Vector => Node::Vector,
                    Shape::Atom => Node::Leaf(self.datum_atom(v, span)),
                },
                |items, tail| Datum {
                    kind: DatumKind::List {
                        items,
                        tail: tail.map(Box::new),
                    },
                    span,
                },
                |items| Datum {
                    kind: DatumKind::Vector(items),
                    span,
                },
            )
        }
    }

    /// Whether `v` is a pair, a vector, or neither.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn classify(&self, v: Value) -> Shape {
        if v.is_pair() {
            Shape::Pair
        } else if self.is_other(v, TypeCode::Vector) {
            Shape::Vector
        } else {
            Shape::Atom
        }
    }

    /// The datum `v` is when it is neither a pair nor a vector.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn datum_atom(&self, v: Value, span: Span) -> Option<Datum> {
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
            Tag::Other if self.keyword_name(v).is_some() => datum(DatumKind::Keyword(
                self.keyword_name(v).expect("a keyword").to_owned(),
            )),
            Tag::Other if self.is_other(v, TypeCode::Bytevector) => {
                // SAFETY: `v` is a live bytevector.
                datum(DatumKind::Bytevector(
                    unsafe { self.bytevector_bytes(v) }.to_vec(),
                ))
            }
            _ => None,
        }
    }

    /// Builds a `T` for the tree of pairs and vectors under `v`, with an
    /// explicit stack so that a deeply nested value cannot overflow the
    /// Rust stack. `node` says what each value is, `list` makes a list from
    /// its elements and its dotted tail, and `vector` a vector. A pair or
    /// vector met again inside itself is a cycle, which gives `None`.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn convert<T>(
        &self,
        v: Value,
        node: impl Fn(Value) -> Node<T>,
        list: impl Fn(Vec<T>, Option<T>) -> T,
        vector: impl Fn(Vec<T>) -> T,
    ) -> Option<T> {
        /// A pair or vector whose parts are being converted.
        enum Frame<T> {
            /// A list's elements so far, and what is left after the one
            /// being converted.
            List {
                items: Vec<T>,
                spine: Vec<u64>,
                rest: Value,
            },
            /// A list's dotted tail is being converted.
            Tail { items: Vec<T>, spine: Vec<u64> },
            /// A vector's elements so far, and the index of the next.
            Vector {
                vector: Value,
                items: Vec<T>,
                next: usize,
            },
        }
        enum Step<T> {
            Convert(Value),
            Return(T),
        }
        // The pairs and vectors being converted, which no part may be.
        let mut open = HashSet::new();
        let mut stack: Vec<Frame<T>> = Vec::new();
        let mut step = Step::Convert(v);
        loop {
            step = match step {
                Step::Convert(v) => match node(v) {
                    Node::Leaf(leaf) => Step::Return(leaf?),
                    Node::Pair => {
                        if !open.insert(v.address()) {
                            return None;
                        }
                        // SAFETY: `v` is a live pair.
                        let (car, cdr) = unsafe { (self.car(v), self.cdr(v)) };
                        stack.push(Frame::List {
                            items: Vec::new(),
                            spine: vec![v.address()],
                            rest: cdr,
                        });
                        Step::Convert(car)
                    }
                    Node::Vector => {
                        if !open.insert(v.address()) {
                            return None;
                        }
                        // SAFETY: `v` is a live vector.
                        if unsafe { self.vector_length(v) } == 0 {
                            open.remove(&v.address());
                            Step::Return(vector(Vec::new()))
                        } else {
                            stack.push(Frame::Vector {
                                vector: v,
                                items: Vec::new(),
                                next: 1,
                            });
                            // SAFETY: the vector has an element 0.
                            Step::Convert(unsafe { self.vector_ref(v, 0) })
                        }
                    }
                },
                Step::Return(done) => match stack.pop() {
                    None => return Some(done),
                    Some(Frame::List {
                        mut items,
                        mut spine,
                        rest,
                    }) => {
                        items.push(done);
                        if rest.is_pair() {
                            // Lists are walked along their spine; a pair met
                            // again on the way is a cycle.
                            if !open.insert(rest.address()) {
                                return None;
                            }
                            spine.push(rest.address());
                            // SAFETY: `rest` is a live pair.
                            let (car, cdr) = unsafe { (self.car(rest), self.cdr(rest)) };
                            stack.push(Frame::List {
                                items,
                                spine,
                                rest: cdr,
                            });
                            Step::Convert(car)
                        } else if rest == Value::EMPTY_LIST {
                            for address in spine {
                                open.remove(&address);
                            }
                            Step::Return(list(items, None))
                        } else {
                            stack.push(Frame::Tail { items, spine });
                            Step::Convert(rest)
                        }
                    }
                    Some(Frame::Tail { items, spine }) => {
                        for address in spine {
                            open.remove(&address);
                        }
                        Step::Return(list(items, Some(done)))
                    }
                    Some(Frame::Vector {
                        vector: v,
                        mut items,
                        next,
                    }) => {
                        items.push(done);
                        // SAFETY: `v` is a live vector, and its elements are
                        // valid.
                        if next < unsafe { self.vector_length(v) } {
                            stack.push(Frame::Vector {
                                vector: v,
                                items,
                                next: next + 1,
                            });
                            Step::Convert(unsafe { self.vector_ref(v, next) })
                        } else {
                            open.remove(&v.address());
                            Step::Return(vector(items))
                        }
                    }
                },
            };
        }
    }

    /// The datum of `v`, if it is `(quote datum)` with `quote` the symbol.
    ///
    /// # Safety
    ///
    /// `v` must be a live pair.
    unsafe fn quoted_operand(&self, v: Value) -> Option<Value> {
        // SAFETY: `v` is a live pair, and so is each one tested before it
        // is read.
        unsafe {
            let head = self.car(v);
            if head.tag() != Tag::Symbol || self.symbol_name(head) != "quote" {
                return None;
            }
            let rest = self.cdr(v);
            if !rest.is_pair() || self.cdr(rest) != Value::EMPTY_LIST {
                return None;
            }
            Some(self.car(rest))
        }
    }

    /// Whether `v` is an object with an identity that `eq?` can tell: what
    /// a datum label can stand for.
    fn has_identity(&self, v: Value) -> bool {
        v.is_pair()
            || v.tag() == Tag::String
            || self.is_other(v, TypeCode::Vector)
            || self.is_other(v, TypeCode::Bytevector)
    }

    /// The datum of the constant `root`, as `quote` takes it: a pair,
    /// vector, string or bytevector that is reached more than once, by
    /// sharing or by a cycle, is labelled where it is first met and
    /// referred to after, so the constant keeps its structure.
    ///
    /// # Safety
    ///
    /// `root` must be a valid value.
    unsafe fn quoted_datum(&self, root: Value, span: Span) -> Option<Datum> {
        // First, which objects are reached twice. Each is entered once, so
        // a cycle ends the walk.
        let mut seen = HashSet::new();
        let mut shared = HashSet::new();
        let mut pending = vec![root];
        while let Some(v) = pending.pop() {
            if !self.has_identity(v) {
                continue;
            }
            if !seen.insert(v.address()) {
                shared.insert(v.address());
                continue;
            }
            // SAFETY: live objects, whose fields are valid values.
            unsafe {
                if v.is_pair() {
                    pending.push(self.cdr(v));
                    pending.push(self.car(v));
                } else if self.is_other(v, TypeCode::Vector) {
                    pending.extend((0..self.vector_length(v)).map(|i| self.vector_ref(v, i)));
                }
            }
        }
        if shared.is_empty() {
            // Nothing to label: the usual conversion, which keeps to its
            // own stack.
            // SAFETY: passed on to the caller.
            return unsafe { self.plain_datum(root, span) };
        }
        // SAFETY: passed on to the caller.
        unsafe { self.labelled_of(root, span, &shared, &mut HashMap::new()) }
    }

    /// The datum of `(quote operand)`, whose `operand` may share parts and
    /// be circular.
    ///
    /// # Safety
    ///
    /// `operand` must be a valid value.
    unsafe fn quote_datum(&self, operand: Value, span: Span) -> Option<Datum> {
        let quote = Datum {
            kind: DatumKind::Symbol("quote".to_owned()),
            span,
        };
        // SAFETY: passed on to the caller.
        let operand = unsafe { self.quoted_datum(operand, span) }?;
        Some(Datum {
            kind: DatumKind::List {
                items: vec![quote, operand],
                tail: None,
            },
            span,
        })
    }

    /// The datum of `v`, labelling the objects in `shared`, which
    /// `labels` numbers as they are first met. It keeps a stack of its own,
    /// so a deeply nested constant cannot overflow the native stack.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    unsafe fn labelled_of(
        &self,
        v: Value,
        span: Span,
        shared: &HashSet<u64>,
        labels: &mut HashMap<u64, u64>,
    ) -> Option<Datum> {
        /// A pair or vector whose parts are being converted, with the label
        /// it was given, if any.
        enum Frame {
            /// A list's elements so far, and what follows the one being
            /// converted. A list is walked along its spine, which ends at
            /// a pair that is labelled, as that one stands for itself.
            List {
                items: Vec<Datum>,
                rest: Value,
                label: Option<u64>,
            },
            /// A list's dotted tail is being converted.
            Tail {
                items: Vec<Datum>,
                label: Option<u64>,
            },
            /// A vector's elements so far, and the index of the next.
            Vector {
                vector: Value,
                items: Vec<Datum>,
                next: usize,
                label: Option<u64>,
            },
        }
        enum Step {
            Convert(Value),
            Return(Datum),
        }
        let datum = |kind| Datum { kind, span };
        let wrap = |label: Option<u64>, inner: Datum| match label {
            Some(label) => datum(DatumKind::Labelled(label, Box::new(inner))),
            None => inner,
        };
        let mut stack: Vec<Frame> = Vec::new();
        let mut step = Step::Convert(v);
        loop {
            step = match step {
                Step::Convert(v) => {
                    if !self.has_identity(v) {
                        // SAFETY: passed on to the caller.
                        Step::Return(unsafe { self.datum_atom(v, span) }?)
                    } else {
                        let mut label = None;
                        if shared.contains(&v.address()) {
                            if let Some(&known) = labels.get(&v.address()) {
                                step = Step::Return(datum(DatumKind::LabelRef(known)));
                                continue;
                            }
                            let new = labels.len() as u64;
                            labels.insert(v.address(), new);
                            label = Some(new);
                        }
                        if v.is_pair() {
                            // SAFETY: `v` is a live pair, whose fields are
                            // valid.
                            let (car, cdr) = unsafe { (self.car(v), self.cdr(v)) };
                            stack.push(Frame::List {
                                items: Vec::new(),
                                rest: cdr,
                                label,
                            });
                            Step::Convert(car)
                        } else if self.is_other(v, TypeCode::Vector) {
                            // SAFETY: `v` is a live vector.
                            if unsafe { self.vector_length(v) } == 0 {
                                Step::Return(wrap(label, datum(DatumKind::Vector(Vec::new()))))
                            } else {
                                stack.push(Frame::Vector {
                                    vector: v,
                                    items: Vec::new(),
                                    next: 1,
                                    label,
                                });
                                // SAFETY: the vector has an element 0.
                                Step::Convert(unsafe { self.vector_ref(v, 0) })
                            }
                        } else {
                            // A string or bytevector.
                            // SAFETY: passed on to the caller.
                            let atom = unsafe { self.datum_atom(v, span) }?;
                            Step::Return(wrap(label, atom))
                        }
                    }
                }
                Step::Return(done) => match stack.pop() {
                    None => return Some(done),
                    Some(Frame::List {
                        mut items,
                        rest,
                        label,
                    }) => {
                        items.push(done);
                        if rest.is_pair() && !shared.contains(&rest.address()) {
                            // SAFETY: `rest` is a live pair.
                            let (car, cdr) = unsafe { (self.car(rest), self.cdr(rest)) };
                            stack.push(Frame::List {
                                items,
                                rest: cdr,
                                label,
                            });
                            Step::Convert(car)
                        } else if rest == Value::EMPTY_LIST {
                            Step::Return(wrap(label, datum(DatumKind::List { items, tail: None })))
                        } else {
                            stack.push(Frame::Tail { items, label });
                            Step::Convert(rest)
                        }
                    }
                    Some(Frame::Tail { items, label }) => Step::Return(wrap(
                        label,
                        datum(DatumKind::List {
                            items,
                            tail: Some(Box::new(done)),
                        }),
                    )),
                    Some(Frame::Vector {
                        vector,
                        mut items,
                        next,
                        label,
                    }) => {
                        items.push(done);
                        // SAFETY: `vector` is a live vector.
                        if next < unsafe { self.vector_length(vector) } {
                            stack.push(Frame::Vector {
                                vector,
                                items,
                                next: next + 1,
                                label,
                            });
                            // SAFETY: `next` is below the length.
                            Step::Convert(unsafe { self.vector_ref(vector, next) })
                        } else {
                            Step::Return(wrap(label, datum(DatumKind::Vector(items))))
                        }
                    }
                },
            };
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
        unsafe {
            self.convert(
                v,
                |v| {
                    if let Some(syntax) = self.syntax_value(v) {
                        return Node::Leaf(Some(syntax.clone()));
                    }
                    match self.classify(v) {
                        Shape::Pair => Node::Pair,
                        Shape::Vector => Node::Vector,
                        Shape::Atom => Node::Leaf(
                            self.datum_atom(v, span)
                                .map(|datum| Syntax::from_datum(&datum, scopes)),
                        ),
                    }
                },
                |items, tail| Syntax::list(items, tail, scopes, span, None),
                |items| Syntax::vector(items, scopes, span, None),
            )
        }
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
