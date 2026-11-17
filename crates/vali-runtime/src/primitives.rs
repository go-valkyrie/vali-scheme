//! Standard procedures implemented in Rust, bound as globals by
//! [`Cx::install_primitives`] until libraries exist. Each takes raw values,
//! valid until its next allocation.

use std::collections::HashSet;

use vali_abi::{Tag, TypeCode};
use vali_bytecode::Arity;

use crate::code::NativeFn;
use crate::cx::Cx;
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::{Arith, Compare, Num, type_error};

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

fn add<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, Arith::Add, Value::fixnum(0).expect("fits"), args)
}

fn mul<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, Arith::Mul, Value::fixnum(1).expect("fits"), args)
}

fn sub<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match args {
        [x] => cx.arith(Arith::Sub, Value::fixnum(0).expect("fits"), *x),
        [x, rest @ ..] => {
            let mut acc = *x;
            for &y in rest {
                acc = cx.arith(Arith::Sub, acc, y)?;
            }
            Ok(acc)
        }
        [] => unreachable!("arity checked"),
    }
}

fn fold<S: Strategy>(
    cx: &mut Cx<S>,
    op: Arith,
    start: Value,
    args: &[Value],
) -> Result<Value, Error> {
    let mut acc = start;
    for &x in args {
        acc = cx.arith(op, acc, x)?;
    }
    Ok(acc)
}

/// A chain of comparisons: `(< a b c)` is `a < b` and `b < c`. `>` and
/// `>=` swap their operands.
fn chain<S: Strategy>(cx: &Cx<S>, op: Compare, swap: bool, args: &[Value]) -> Result<Value, Error> {
    let mut result = true;
    for pair in args.windows(2) {
        let (a, b) = if swap {
            (pair[1], pair[0])
        } else {
            (pair[0], pair[1])
        };
        // Every argument is checked, even after the answer is known.
        result &= cx.compare(op, a, b)?;
    }
    Ok(Value::boolean(result))
}

fn num_eq<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    chain(cx, Compare::Eq, false, args)
}
fn lt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    chain(cx, Compare::Lt, false, args)
}
fn le<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    chain(cx, Compare::Le, false, args)
}
fn gt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    chain(cx, Compare::Lt, true, args)
}
fn ge<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    chain(cx, Compare::Le, true, args)
}

fn division<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
    name: &str,
    remainder: bool,
) -> Result<Value, Error> {
    let (a, b) = (args[0], args[1]);
    let (Some(x), Some(y)) = (
        cx.number(a).and_then(|n| n.exact()),
        cx.number(b).and_then(|n| n.exact()),
    ) else {
        let culprit = if cx.number(a).and_then(|n| n.exact()).is_none() {
            a
        } else {
            b
        };
        return Err(type_error(name, "an exact integer", culprit));
    };
    if y.is_zero() {
        return Err(Error::scheme(format!("{name}: division by zero")));
    }
    // Fixnum fast path; bignum division comes with the rest of vali-num's
    // arithmetic.
    match (x.to_i64(), y.to_i64()) {
        (Some(x), Some(y)) if !(x == i64::MIN && y == -1) => {
            let r = if remainder { x % y } else { x / y };
            Ok(Value::fixnum(r).expect("a quotient or remainder of fixnums fits"))
        }
        _ => Err(Error::scheme(format!(
            "{name}: bignum division is not supported yet"
        ))),
    }
}

fn quotient<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    division(cx, args, "quotient", false)
}
fn remainder<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    division(cx, args, "remainder", true)
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

fn list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut list = Value::EMPTY_LIST;
    for &x in args.iter().rev() {
        // SAFETY: valid values.
        list = unsafe { cx.cons_raw(x, list)? };
    }
    Ok(list)
}

/// The elements of a proper list, or an error naming `name`.
fn elements<S: Strategy>(cx: &Cx<S>, name: &str, mut list: Value) -> Result<Vec<Value>, Error> {
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
    match (cx.number(a), cx.number(b)) {
        (Some(Num::Flonum(x)), Some(Num::Flonum(y))) => {
            x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
        }
        (Some(Num::Integer(x)), Some(Num::Integer(y))) => x == y,
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
    let mut assumed = HashSet::new();
    let mut work = vec![(args[0], args[1])];
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
            return Ok(Value::FALSE);
        }
    }
    Ok(Value::TRUE)
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

impl<S: Strategy> Cx<S> {
    /// Binds the standard procedures implemented so far as globals, which
    /// variables of the same name in modules loaded afterwards are bound to.
    pub fn install_primitives(&mut self) -> Result<(), Error> {
        let table: &[(&str, Arity, NativeFn<S>)] = &[
            ("+", at_least(0), add),
            ("*", at_least(0), mul),
            ("-", at_least(1), sub),
            ("=", at_least(1), num_eq),
            ("<", at_least(1), lt),
            ("<=", at_least(1), le),
            (">", at_least(1), gt),
            (">=", at_least(1), ge),
            ("quotient", Arity::exactly(2), quotient),
            ("remainder", Arity::exactly(2), remainder),
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
        ];
        for &(name, arity, f) in table {
            self.define_native(name, arity, f)?;
        }
        Ok(())
    }
}
