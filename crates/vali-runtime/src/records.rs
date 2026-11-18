//! Records (R7RS 5.5), as `define-record-type` in `(scheme base)` builds
//! them from the procedures here.
//!
//! A record type is a heap object of type `RecordType` holding its name, a
//! symbol, and its field names, a vector of symbols. A record is a heap
//! object of type `Record` holding its type, then its fields.

use vali_abi::TypeCode;

use crate::cx::{Cx, get};
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::type_error;

impl<S: Strategy> Cx<S> {
    fn is_record_type(&self, v: Value) -> bool {
        self.is_other(v, TypeCode::RecordType)
    }

    /// A record type's name and field names.
    ///
    /// # Safety
    ///
    /// `rtd` must be a live record type.
    unsafe fn record_type_parts(&self, rtd: Value) -> (Value, Value) {
        // SAFETY: passed on to the caller.
        unsafe {
            (
                Value::from_raw(get(rtd, Self::LAYOUT.field(TypeCode::RecordType, 0))),
                Value::from_raw(get(rtd, Self::LAYOUT.field(TypeCode::RecordType, 1))),
            )
        }
    }

    /// The name of the record type of `v`, if `v` is a record.
    ///
    /// # Safety
    ///
    /// `v` must be a valid value.
    pub(crate) unsafe fn record_type_name(&self, v: Value) -> Option<&str> {
        let rtd = if self.is_other(v, TypeCode::Record) {
            // SAFETY: a live record, whose first field is its type.
            unsafe { Value::from_raw(get(v, Self::LAYOUT.field(TypeCode::Record, 0))) }
        } else if self.is_record_type(v) {
            v
        } else {
            return None;
        };
        // SAFETY: a live record type, whose name is a symbol.
        Some(unsafe { self.symbol_name(self.record_type_parts(rtd).0) })
    }
}

/// The record type in `v`, or an error naming `name`.
fn rtd_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<Value, Error> {
    if cx.is_record_type(v) {
        Ok(v)
    } else {
        Err(type_error(name, "a record type", v))
    }
}

/// The index of the field named `field` in `rtd`.
fn field_index<S: Strategy>(cx: &Cx<S>, rtd: Value, field: Value) -> Result<usize, Error> {
    // SAFETY: a live record type, whose field names are a vector.
    let fields = unsafe { cx.record_type_parts(rtd).1 };
    // SAFETY: as above.
    let count = unsafe { cx.vector_length(fields) };
    (0..count)
        // SAFETY: as above.
        .position(|i| unsafe { cx.vector_ref(fields, i) } == field)
        .ok_or_else(|| {
            // SAFETY: valid values.
            let type_name = unsafe { cx.record_type_name(rtd) }.unwrap_or("?");
            Error::scheme(format!("{type_name} has no field {field:?}"))
        })
}

/// `(%make-record-type name (field ...))`.
pub(crate) fn make_record_type<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    if args[0].tag() != vali_abi::Tag::Symbol {
        return Err(type_error("define-record-type", "a symbol", args[0]));
    }
    let mut names = Vec::new();
    let mut rest = args[1];
    while rest.is_pair() {
        // SAFETY: a live pair.
        let (name, next) = unsafe { (cx.car(rest), cx.cdr(rest)) };
        names.push(name);
        rest = next;
    }
    // SAFETY: the fill is valid.
    let fields = unsafe { cx.make_vector_raw(names.len(), Value::FALSE) }?;
    for (i, name) in names.into_iter().enumerate() {
        // SAFETY: the vector has that many elements.
        unsafe { cx.vector_set(fields, i, name) };
    }
    let rtd = cx.new_object(TypeCode::RecordType, 2, 0)?;
    // SAFETY: just allocated with both fields.
    unsafe {
        cx.init_field(rtd, Cx::<S>::LAYOUT.field(TypeCode::RecordType, 0), args[0]);
        cx.init_field(rtd, Cx::<S>::LAYOUT.field(TypeCode::RecordType, 1), fields);
    }
    Ok(rtd)
}

/// `(%record-field-index rtd field)`.
pub(crate) fn record_field_index<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    let rtd = rtd_arg(cx, "record field", args[0])?;
    let i = field_index(cx, rtd, args[1])?;
    Ok(Value::fixnum(i as i64).expect("a field index fits"))
}

/// `(%record-constructor-indices rtd (field ...))`: a vector of the index of
/// each field a constructor takes, in its order.
pub(crate) fn record_constructor_indices<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    let rtd = rtd_arg(cx, "record constructor", args[0])?;
    let mut indices = Vec::new();
    let mut rest = args[1];
    while rest.is_pair() {
        // SAFETY: a live pair.
        let (field, next) = unsafe { (cx.car(rest), cx.cdr(rest)) };
        indices.push(field_index(cx, rtd, field)?);
        rest = next;
    }
    // SAFETY: the fill is valid.
    let v = unsafe { cx.make_vector_raw(indices.len(), Value::FALSE) }?;
    for (i, index) in indices.into_iter().enumerate() {
        let index = Value::fixnum(index as i64).expect("a field index fits");
        // SAFETY: the vector has that many elements.
        unsafe { cx.vector_set(v, i, index) };
    }
    Ok(v)
}

/// `(%make-record rtd indices value ...)`: a record whose field at each of
/// `indices` holds the matching value; the others are unspecified.
pub(crate) fn make_record<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let rtd = rtd_arg(cx, "record constructor", args[0])?;
    // SAFETY: a live record type.
    let count = unsafe { cx.vector_length(cx.record_type_parts(rtd).1) };
    // These procedures are importable, so the indices are checked too.
    let indices = args[1];
    // SAFETY: a valid value, and a live vector when it is one.
    if !cx.is_other(indices, TypeCode::Vector)
        || unsafe { cx.vector_length(indices) } != args.len() - 2
    {
        return Err(Error::scheme("record constructor: corrupt indices"));
    }
    let record = cx.new_object(TypeCode::Record, 1 + count, count as u32)?;
    // SAFETY: just allocated with its type and `count` fields; the indices
    // vector was made by `%record-constructor-indices` for this type.
    unsafe {
        cx.init_field(record, Cx::<S>::LAYOUT.field(TypeCode::Record, 0), rtd);
        for i in 0..count {
            cx.init_field(
                record,
                Cx::<S>::LAYOUT.field(TypeCode::Record, 1 + i),
                Value::UNSPECIFIED,
            );
        }
        for (k, &value) in args[2..].iter().enumerate() {
            let i = cx
                .vector_ref(indices, k)
                .as_fixnum()
                .and_then(|i| usize::try_from(i).ok())
                .filter(|&i| i < count)
                .ok_or_else(|| Error::scheme("record constructor: corrupt indices"))?;
            cx.write_field(
                record,
                Cx::<S>::LAYOUT.field(TypeCode::Record, 1 + i),
                value,
            );
        }
    }
    Ok(record)
}

/// Whether `v` is a record of type `rtd`.
fn is_record_of<S: Strategy>(cx: &Cx<S>, v: Value, rtd: Value) -> bool {
    cx.is_other(v, TypeCode::Record)
        // SAFETY: a live record, whose first field is its type.
        && unsafe { Value::from_raw(get(v, Cx::<S>::LAYOUT.field(TypeCode::Record, 0))) } == rtd
}

/// `(%record? obj rtd)`.
pub(crate) fn is_record<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(is_record_of(cx, args[0], args[1])))
}

/// The field index `i` of a record of type `rtd`, after checking `v` is one.
fn checked<S: Strategy>(cx: &Cx<S>, v: Value, rtd: Value, i: Value) -> Result<i32, Error> {
    if !is_record_of(cx, v, rtd) {
        // SAFETY: valid values.
        let type_name = unsafe { cx.record_type_name(rtd) }.unwrap_or("?");
        return Err(type_error(
            "record field",
            &format!("a {type_name} record"),
            v,
        ));
    }
    // These procedures are importable, so the index is checked, not trusted.
    // SAFETY: a live record type.
    let count = unsafe { cx.vector_length(cx.record_type_parts(rtd).1) };
    let i = i
        .as_fixnum()
        .and_then(|i| usize::try_from(i).ok())
        .filter(|&i| i < count)
        .ok_or_else(|| Error::scheme("record field: no such field"))?;
    Ok(Cx::<S>::LAYOUT.field(TypeCode::Record, 1 + i))
}

/// `(%record-ref obj rtd i)`.
pub(crate) fn record_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let offset = checked(cx, args[0], args[1], args[2])?;
    // SAFETY: a live record of that type, which has the field.
    Ok(unsafe { Value::from_raw(get(args[0], offset)) })
}

/// `(%record-set! obj rtd i value)`.
pub(crate) fn record_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let offset = checked(cx, args[0], args[1], args[2])?;
    // SAFETY: as above, and the value is valid.
    unsafe { cx.write_field(args[0], offset, args[3]) };
    Ok(Value::UNSPECIFIED)
}

// Error objects and multiple values: records of types the runtime makes
// itself, when first needed.

impl<S: Strategy> Cx<S> {
    /// A record type named `name` with `fields`, made by the runtime.
    fn internal_type(&mut self, name: &str, fields: &[&str]) -> Result<Value, Error> {
        let name = self.intern(name)?;
        let mut list = Value::EMPTY_LIST;
        for field in fields.iter().rev() {
            let field = self.intern(field)?;
            // SAFETY: valid values; nothing is collected meanwhile.
            list = unsafe { self.cons_raw(field, list) }?;
        }
        make_record_type(self, &[name, list])
    }

    /// A record of the runtime's type `rtd` holding `values`.
    fn internal_record(&mut self, rtd: Value, values: &[Value]) -> Result<Value, Error> {
        let record = self.new_object(TypeCode::Record, 1 + values.len(), values.len() as u32)?;
        // SAFETY: just allocated with its type and fields.
        unsafe {
            self.init_field(record, Self::LAYOUT.field(TypeCode::Record, 0), rtd);
            for (i, &v) in values.iter().enumerate() {
                self.init_field(record, Self::LAYOUT.field(TypeCode::Record, 1 + i), v);
            }
        }
        Ok(record)
    }

    /// Field `i` of `v`, if `v` is a record of type `rtd`.
    fn internal_field(&self, v: Value, rtd: Option<Value>, i: usize) -> Option<Value> {
        let rtd = rtd?;
        // SAFETY: a live record of that type, which has the field.
        is_record_of(self, v, rtd).then(|| unsafe {
            Value::from_raw(get(v, Self::LAYOUT.field(TypeCode::Record, 1 + i)))
        })
    }

    /// An error object (R7RS 6.11) with `message` and `irritants`.
    pub(crate) fn error_object_raw(
        &mut self,
        message: Value,
        irritants: Value,
    ) -> Result<Value, Error> {
        let rtd = match self.error_object_type {
            Some(rtd) => rtd,
            None => {
                let rtd = self.internal_type("error-object", &["message", "irritants"])?;
                self.hold(rtd);
                self.error_object_type = Some(rtd);
                rtd
            }
        };
        self.internal_record(rtd, &[message, irritants])
    }

    /// The message and irritants of `v`, if it is an error object.
    pub(crate) fn error_object_parts(&self, v: Value) -> Option<(Value, Value)> {
        Some((
            self.internal_field(v, self.error_object_type, 0)?,
            self.internal_field(v, self.error_object_type, 1)?,
        ))
    }

    /// How an uncaught `raised` reads in the error the host sees: an error
    /// object as its message and irritants, anything else as written.
    pub(crate) fn describe_raised(&mut self, raised: Value) -> Result<String, Error> {
        let Some((message, irritants)) = self.error_object_parts(raised) else {
            // SAFETY: a valid value.
            let raised = unsafe { self.root(raised) };
            return Ok(format!(
                "uncaught exception: {}",
                self.write_string(&raised)?
            ));
        };
        let mut text = if message.tag() == vali_abi::Tag::String {
            // SAFETY: a live string.
            unsafe { self.string_str(message) }.to_owned()
        } else {
            // SAFETY: a valid value.
            let message = unsafe { self.root(message) };
            self.write_string(&message)?
        };
        let mut rest = irritants;
        while rest.is_pair() {
            // SAFETY: a live pair, whose fields are valid.
            let (irritant, next) = unsafe { (self.car(rest), self.cdr(rest)) };
            let irritant = unsafe { self.root(irritant) };
            text.push(' ');
            text.push_str(&self.write_string(&irritant)?);
            rest = next;
        }
        Ok(text)
    }

    /// The type of multiple values.
    fn values_type(&mut self) -> Result<Value, Error> {
        match self.values_type {
            Some(rtd) => Ok(rtd),
            None => {
                let rtd = self.internal_type("values", &["list"])?;
                self.hold(rtd);
                self.values_type = Some(rtd);
                Ok(rtd)
            }
        }
    }
}

/// Raises `raised`.
fn raise_value<S: Strategy>(cx: &mut Cx<S>, raised: Value) -> Error {
    match cx.describe_raised(raised) {
        // SAFETY: a valid value.
        Ok(message) => Error::raise(message, unsafe { cx.root(raised) }),
        Err(e) => e,
    }
}

/// `(%raise obj)`: raises `obj` straight to the innermost `%catch`,
/// handlers or not.
pub(crate) fn raise<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Err(match cx.describe_raised(args[0]) {
        // SAFETY: a valid value.
        Ok(message) => Error::escape(message, unsafe { cx.root(args[0]) }),
        Err(e) => e,
    })
}

/// `(error message irritant ...)`: raises an error object.
pub(crate) fn error<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut irritants = Value::EMPTY_LIST;
    for &irritant in args[1..].iter().rev() {
        // SAFETY: valid values; nothing is collected meanwhile.
        irritants = unsafe { cx.cons_raw(irritant, irritants) }?;
    }
    let object = cx.error_object_raw(args[0], irritants)?;
    Err(raise_value(cx, object))
}

pub(crate) fn is_error_object<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(cx.error_object_parts(args[0]).is_some()))
}

pub(crate) fn error_object_message<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    cx.error_object_parts(args[0])
        .map(|(message, _)| message)
        .ok_or_else(|| type_error("error-object-message", "an error object", args[0]))
}

pub(crate) fn error_object_irritants<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    cx.error_object_parts(args[0])
        .map(|(_, irritants)| irritants)
        .ok_or_else(|| type_error("error-object-irritants", "an error object", args[0]))
}

/// `(values obj ...)`: one value is itself; any other number is a values
/// object, which `call-with-values` spreads. Until the compiler handles
/// multiple values itself, they are an object like any other.
pub(crate) fn values<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if let [one] = args {
        return Ok(*one);
    }
    let mut list = Value::EMPTY_LIST;
    for &v in args.iter().rev() {
        // SAFETY: valid values; nothing is collected meanwhile.
        list = unsafe { cx.cons_raw(v, list) }?;
    }
    let rtd = cx.values_type()?;
    cx.internal_record(rtd, &[list])
}

/// `(%values->list obj)`: the values in a values object, or a list of
/// `obj` alone.
pub(crate) fn values_to_list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match cx.internal_field(args[0], cx.values_type, 0) {
        Some(list) => Ok(list),
        // SAFETY: a valid value.
        None => Ok(unsafe { cx.cons_raw(args[0], Value::EMPTY_LIST) }?),
    }
}
