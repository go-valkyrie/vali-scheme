//! Standard procedures implemented in Rust, bound as globals by
//! [`Cx::install_primitives`] until libraries exist. Each takes raw values,
//! valid until its next allocation.

use std::collections::HashSet;

use vali_abi::{Tag, TypeCode};
use vali_bytecode::Arity;

use crate::code::NativeFn;
use crate::cx::Cx;
use crate::handle::Error;
use crate::records;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::type_error;

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

fn cons<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: arguments are valid values.
    Ok(unsafe { cx.cons_raw(args[0], args[1])? })
}

fn pair_arg(name: &str, v: Value) -> Result<Value, Error> {
    if v.is_pair() {
        Ok(v)
    } else {
        Err(type_error(name, "a pair", v))
    }
}

fn car<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let pair = pair_arg("car", args[0])?;
    // SAFETY: a live pair.
    Ok(unsafe { cx.car(pair) })
}

fn cdr<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let pair = pair_arg("cdr", args[0])?;
    // SAFETY: a live pair.
    Ok(unsafe { cx.cdr(pair) })
}

fn set_car<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let pair = pair_arg("set-car!", args[0])?;
    // SAFETY: a live pair, and a valid value.
    unsafe { cx.set_car(pair, args[1]) };
    Ok(Value::UNSPECIFIED)
}

fn set_cdr<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let pair = pair_arg("set-cdr!", args[0])?;
    // SAFETY: a live pair, and a valid value.
    unsafe { cx.set_cdr(pair, args[1]) };
    Ok(Value::UNSPECIFIED)
}

fn list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut list = Value::EMPTY_LIST;
    for &x in args.iter().rev() {
        // SAFETY: valid values.
        list = unsafe { cx.cons_raw(x, list)? };
    }
    Ok(list)
}

/// The elements of a proper list, or an error naming `name`.
pub(crate) fn elements<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    mut list: Value,
) -> Result<Vec<Value>, Error> {
    let start = list;
    let mut items = Vec::new();
    while list.is_pair() {
        // SAFETY: a live pair.
        unsafe {
            items.push(cx.car(list));
            list = cx.cdr(list);
        }
        if items.len() > 1 << 40 {
            break;
        }
    }
    if list == Value::EMPTY_LIST {
        Ok(items)
    } else {
        Err(type_error(name, "a proper list", start))
    }
}

fn length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = elements(cx, "length", args[0])?.len();
    Ok(Value::fixnum(n as i64).expect("lengths fit"))
}

fn reverse<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut result = Value::EMPTY_LIST;
    for x in elements(cx, "reverse", args[0])? {
        // SAFETY: valid values.
        result = unsafe { cx.cons_raw(x, result)? };
    }
    Ok(result)
}

fn append<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let Some((&last, lists)) = args.split_last() else {
        return Ok(Value::EMPTY_LIST);
    };
    let mut result = last;
    for &list in lists.iter().rev() {
        for x in elements(cx, "append", list)?.into_iter().rev() {
            // SAFETY: valid values.
            result = unsafe { cx.cons_raw(x, result)? };
        }
    }
    Ok(result)
}

fn is_null<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0] == Value::EMPTY_LIST))
}
fn is_pair<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].is_pair()))
}
fn not<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(!args[0].is_true()))
}
fn eq<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0] == args[1]))
}

/// `eqv?`: `eq?`, or numbers of the same exactness and value. Flonums
/// compare by their bits, with every NaN alike, so `0.0` and `-0.0` differ
/// (ADR 0007).
fn eqv_values<S: Strategy>(cx: &Cx<S>, a: Value, b: Value) -> bool {
    if a == b {
        return true;
    }
    // Distinct fixnums and immediates are never eqv.
    if !a.is_pointer() && !b.is_pointer() {
        return false;
    }
    match (cx.number(a), cx.number(b)) {
        (Some(x), Some(y)) => x.eqv(&y),
        _ => false,
    }
}

fn eqv<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(eqv_values(cx, args[0], args[1])))
}

/// `equal?`: structural equality over pairs, vectors, strings and
/// bytevectors, `eqv?` otherwise. A pair of objects already being compared
/// counts as equal, so circular structure terminates.
fn equal<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(equal_values(cx, args[0], args[1])))
}

fn equal_values<S: Strategy>(cx: &Cx<S>, a: Value, b: Value) -> bool {
    let mut assumed = HashSet::new();
    let mut work = vec![(a, b)];
    while let Some((a, b)) = work.pop() {
        if eqv_values(cx, a, b) || !assumed.insert((a.raw(), b.raw())) {
            continue;
        }
        // SAFETY: values reachable from the arguments are live, and each
        // accessor is used only on its type.
        let same = unsafe {
            if a.is_pair() && b.is_pair() {
                work.push((cx.cdr(a), cx.cdr(b)));
                work.push((cx.car(a), cx.car(b)));
                true
            } else if a.tag() == Tag::String && b.tag() == Tag::String {
                cx.string_str(a) == cx.string_str(b)
            } else {
                match (cx.header_type(a), cx.header_type(b)) {
                    (Some(TypeCode::Vector), Some(TypeCode::Vector)) => {
                        let n = cx.vector_length(a);
                        if n == cx.vector_length(b) {
                            for i in (0..n).rev() {
                                work.push((cx.vector_ref(a, i), cx.vector_ref(b, i)));
                            }
                            true
                        } else {
                            false
                        }
                    }
                    (Some(TypeCode::Bytevector), Some(TypeCode::Bytevector)) => {
                        cx.bytevector_bytes(a) == cx.bytevector_bytes(b)
                    }
                    _ => false,
                }
            }
        };
        if !same {
            return false;
        }
    }
    true
}

fn vector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: the fill is valid, and `v` has `args.len()` elements.
    unsafe {
        let v = cx.make_vector_raw(args.len(), Value::FALSE)?;
        for (i, &x) in args.iter().enumerate() {
            cx.vector_set(v, i, x);
        }
        Ok(v)
    }
}

fn make_vector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let Some(n) = args[0].as_fixnum().filter(|n| *n >= 0) else {
        return Err(type_error("make-vector", "a non-negative fixnum", args[0]));
    };
    let fill = args.get(1).copied().filter(|v| *v != Value::DEFAULT);
    // SAFETY: the fill is a valid value.
    Ok(unsafe { cx.make_vector_raw(n as usize, fill.unwrap_or(Value::FALSE))? })
}

fn vector_index<S: Strategy>(cx: &Cx<S>, name: &str, v: Value, i: Value) -> Result<usize, Error> {
    // SAFETY: valid values.
    if unsafe { cx.header_type(v) } != Some(TypeCode::Vector) {
        return Err(type_error(name, "a vector", v));
    }
    // SAFETY: a live vector.
    let len = unsafe { cx.vector_length(v) };
    match i.as_fixnum() {
        Some(i) if (0..len as i64).contains(&i) => Ok(i as usize),
        _ => Err(Error::scheme(format!(
            "{name}: index {i:?} is out of range for a vector of length {len}"
        ))),
    }
}

fn vector_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = vector_index(cx, "vector-ref", args[0], args[1])?;
    // SAFETY: checked above.
    Ok(unsafe { cx.vector_ref(args[0], i) })
}

fn vector_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = vector_index(cx, "vector-set!", args[0], args[1])?;
    // SAFETY: checked above.
    unsafe { cx.vector_set(args[0], i, args[2]) };
    Ok(Value::UNSPECIFIED)
}

fn vector_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: valid values.
    if unsafe { cx.header_type(args[0]) } != Some(TypeCode::Vector) {
        return Err(type_error("vector-length", "a vector", args[0]));
    }
    // SAFETY: a live vector.
    let len = unsafe { cx.vector_length(args[0]) };
    Ok(Value::fixnum(len as i64).expect("lengths fit"))
}

/// The first tail of `list` whose car matches `x` under `same`, or `#f`.
fn member_by<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    x: Value,
    list: Value,
    same: fn(&Cx<S>, Value, Value) -> bool,
) -> Result<Value, Error> {
    let mut tail = list;
    while tail.is_pair() {
        // SAFETY: a live pair.
        if same(cx, x, unsafe { cx.car(tail) }) {
            return Ok(tail);
        }
        // SAFETY: a live pair.
        tail = unsafe { cx.cdr(tail) };
    }
    if tail == Value::EMPTY_LIST {
        Ok(Value::FALSE)
    } else {
        Err(type_error(name, "a proper list", list))
    }
}

/// The first pair in the association list `alist` whose car matches `x`
/// under `same`, or `#f`.
fn assoc_by<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    x: Value,
    alist: Value,
    same: fn(&Cx<S>, Value, Value) -> bool,
) -> Result<Value, Error> {
    for entry in elements(cx, name, alist)? {
        if !entry.is_pair() {
            return Err(type_error(name, "an association list", alist));
        }
        // SAFETY: a live pair.
        if same(cx, x, unsafe { cx.car(entry) }) {
            return Ok(entry);
        }
    }
    Ok(Value::FALSE)
}

fn same_eq<S: Strategy>(_cx: &Cx<S>, a: Value, b: Value) -> bool {
    a == b
}

fn memq<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    member_by(cx, "memq", args[0], args[1], same_eq)
}
fn memv<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    member_by(cx, "memv", args[0], args[1], eqv_values)
}
fn member<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    member_by(cx, "member", args[0], args[1], equal_values)
}
fn assq<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    assoc_by(cx, "assq", args[0], args[1], same_eq)
}
fn assv<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    assoc_by(cx, "assv", args[0], args[1], eqv_values)
}
fn assoc<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    assoc_by(cx, "assoc", args[0], args[1], equal_values)
}

/// `list-tail`: `list` with its first `k` pairs dropped.
fn tail_at<S: Strategy>(cx: &Cx<S>, name: &str, list: Value, k: Value) -> Result<Value, Error> {
    let Some(k) = k.as_fixnum().filter(|k| *k >= 0) else {
        return Err(type_error(name, "a non-negative fixnum", k));
    };
    let mut tail = list;
    for _ in 0..k {
        if !tail.is_pair() {
            return Err(Error::scheme(format!(
                "{name}: the list is shorter than {k}"
            )));
        }
        // SAFETY: a live pair.
        tail = unsafe { cx.cdr(tail) };
    }
    Ok(tail)
}

fn list_tail<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    tail_at(cx, "list-tail", args[0], args[1])
}

fn list_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let tail = tail_at(cx, "list-ref", args[0], args[1])?;
    let pair = pair_arg("list-ref", tail)?;
    // SAFETY: a live pair.
    Ok(unsafe { cx.car(pair) })
}

fn is_boolean<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].as_boolean().is_some()))
}

fn is_procedure<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::Closure))
}

fn is_vector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: a valid value.
    Ok(Value::boolean(
        unsafe { cx.header_type(args[0]) } == Some(TypeCode::Vector),
    ))
}

/// `list?`: a proper list, which is never circular.
fn is_list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let (mut slow, mut fast) = (args[0], args[0]);
    loop {
        for _ in 0..2 {
            if fast == Value::EMPTY_LIST {
                return Ok(Value::TRUE);
            }
            if !fast.is_pair() {
                return Ok(Value::FALSE);
            }
            // SAFETY: a live pair.
            fast = unsafe { cx.cdr(fast) };
        }
        // SAFETY: the slow pointer trails the fast one through pairs already
        // checked.
        slow = unsafe { cx.cdr(slow) };
        if slow == fast {
            return Ok(Value::FALSE);
        }
    }
}

fn symbol_eq<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    for &x in args {
        if x.tag() != Tag::Symbol {
            return Err(type_error("symbol=?", "a symbol", x));
        }
    }
    Ok(Value::boolean(args.windows(2).all(|p| p[0] == p[1])))
}

fn vector_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<Value, Error> {
    // SAFETY: a valid value.
    if unsafe { cx.header_type(v) } == Some(TypeCode::Vector) {
        Ok(v)
    } else {
        Err(type_error(name, "a vector", v))
    }
}

fn vector_to_list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let v = vector_arg(cx, "vector->list", args[0])?;
    // SAFETY: a live vector; its elements are read before allocating.
    let items: Vec<Value> = unsafe {
        (0..cx.vector_length(v))
            .map(|i| cx.vector_ref(v, i))
            .collect()
    };
    list(cx, &items)
}

fn list_to_vector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let items = elements(cx, "list->vector", args[0])?;
    vector(cx, &items)
}

fn vector_fill<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let v = vector_arg(cx, "vector-fill!", args[0])?;
    // SAFETY: a live vector.
    unsafe {
        for i in 0..cx.vector_length(v) {
            cx.vector_set(v, i, args[1]);
        }
    }
    Ok(Value::UNSPECIFIED)
}

/// `(apply f a ... list)`: asks the VM to tail-call `f`.
fn apply<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let (&last, rest) = args[1..].split_last().unwrap_or((&Value::EMPTY_LIST, &[]));
    let mut call_args = rest.to_vec();
    call_args.extend(elements(cx, "apply", last)?);
    cx.tail_call(args[0], call_args);
    Ok(Value::UNSPECIFIED)
}

/// The standard procedures implemented in Rust.
fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    let mut table: Vec<(&'static str, Arity, NativeFn<S>)> = vec![
        ("cons", Arity::exactly(2), cons),
        ("car", Arity::exactly(1), car),
        ("cdr", Arity::exactly(1), cdr),
        ("list", at_least(0), list),
        ("length", Arity::exactly(1), length),
        ("reverse", Arity::exactly(1), reverse),
        ("append", at_least(0), append),
        ("null?", Arity::exactly(1), is_null),
        ("pair?", Arity::exactly(1), is_pair),
        ("not", Arity::exactly(1), not),
        ("eq?", Arity::exactly(2), eq),
        ("eqv?", Arity::exactly(2), eqv),
        ("equal?", Arity::exactly(2), equal),
        ("vector", at_least(0), vector),
        (
            "make-vector",
            Arity {
                required: 1,
                optional: 1,
                rest: false,
            },
            make_vector,
        ),
        ("vector-ref", Arity::exactly(2), vector_ref),
        ("vector-set!", Arity::exactly(3), vector_set),
        ("vector-length", Arity::exactly(1), vector_length),
        ("memq", Arity::exactly(2), memq),
        ("memv", Arity::exactly(2), memv),
        ("member", Arity::exactly(2), member),
        ("assq", Arity::exactly(2), assq),
        ("assv", Arity::exactly(2), assv),
        ("assoc", Arity::exactly(2), assoc),
        ("list-tail", Arity::exactly(2), list_tail),
        ("list-ref", Arity::exactly(2), list_ref),
        ("apply", at_least(1), apply),
        ("vector->list", Arity::exactly(1), vector_to_list),
        ("list->vector", Arity::exactly(1), list_to_vector),
        ("vector-fill!", Arity::exactly(2), vector_fill),
        ("boolean?", Arity::exactly(1), is_boolean),
        ("procedure?", Arity::exactly(1), is_procedure),
        ("vector?", Arity::exactly(1), is_vector),
        ("list?", Arity::exactly(1), is_list),
        ("symbol=?", at_least(1), symbol_eq),
        ("set-car!", Arity::exactly(2), set_car),
        ("set-cdr!", Arity::exactly(2), set_cdr),
        ("error", at_least(1), records::error),
        ("%raise", Arity::exactly(1), records::raise),
        ("error-object?", Arity::exactly(1), records::is_error_object),
        (
            "error-object-message",
            Arity::exactly(1),
            records::error_object_message,
        ),
        (
            "error-object-irritants",
            Arity::exactly(1),
            records::error_object_irritants,
        ),
        ("values", at_least(0), records::values),
        ("%values->list", Arity::exactly(1), records::values_to_list),
        ("%write-to", Arity::exactly(3), crate::ports::write_to),
        ("%read", Arity::exactly(1), crate::ports::read),
        (
            "%console-input",
            Arity::exactly(0),
            crate::ports::console_input,
        ),
        (
            "%console-output",
            Arity::exactly(0),
            crate::ports::console_output,
        ),
        (
            "%console-error",
            Arity::exactly(0),
            crate::ports::console_error,
        ),
        ("eof-object", Arity::exactly(0), crate::ports::eof_object),
        (
            "eof-object?",
            Arity::exactly(1),
            crate::ports::is_eof_object,
        ),
        (
            "%make-record-type",
            Arity::exactly(2),
            records::make_record_type,
        ),
        (
            "%record-field-index",
            Arity::exactly(2),
            records::record_field_index,
        ),
        (
            "%record-constructor-indices",
            Arity::exactly(2),
            records::record_constructor_indices,
        ),
        ("%make-record", at_least(2), records::make_record),
        ("%record?", Arity::exactly(2), records::is_record),
        ("%record-ref", Arity::exactly(3), records::record_ref),
        ("%record-set!", Arity::exactly(4), records::record_set),
    ];
    table.extend(crate::numbers::table());
    table.extend(crate::strings::table());
    table.push(("features", Arity::exactly(0), features));
    table.extend(crate::continuations::dynamic_table());
    table
}

/// The features `cond-expand` tests for and `features` lists (R7RS
/// appendix B).
pub const FEATURES: &[&str] = &[
    "r7rs",
    "exact-closed",
    "exact-complex",
    "ieee-float",
    "full-unicode",
    "ratios",
    "vali",
];

/// `(features)`: a fresh list of the features, as symbols.
fn features<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    let mut list = Value::EMPTY_LIST;
    for name in FEATURES.iter().rev() {
        let symbol = cx.intern(name)?;
        // SAFETY: valid values; nothing is collected meanwhile.
        list = unsafe { cx.cons_raw(symbol, list) }?;
    }
    Ok(list)
}

/// The names of the standard procedures implemented in Rust, which
/// [`Cx::install_primitives`] binds.
pub fn primitive_names() -> Vec<&'static str> {
    table::<crate::NoCollection>()
        .into_iter()
        .map(|(name, _, _)| name)
        .chain(["%catch", "%call/cc", "%continue"])
        .collect()
}

impl<S: Strategy> Cx<S> {
    /// Binds the standard procedures implemented so far as primitives,
    /// which `(vali primitive)` exports, and
    /// registers Vali's own native libraries, which are deterministic, so
    /// they work at expansion time too.
    pub fn install_primitives(&mut self) -> Result<(), Error> {
        for (name, arity, f) in table::<S>() {
            self.define_primitive(name, arity, f)?;
        }
        self.define_control()?;
        self.register_native_library(crate::ports::PORTS_IDENTITY, crate::ports::library());
        Ok(())
    }
}
