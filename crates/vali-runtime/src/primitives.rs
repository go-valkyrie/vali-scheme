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
    // Two fixnums: no conversion. Only the quotient of the most negative
    // fixnum by -1 leaves the fixnum range, and that takes the slow path.
    if let (Some(x), Some(y)) = (a.as_fixnum(), b.as_fixnum())
        && y != 0
        && let Some(result) = Value::fixnum(if remainder { x % y } else { x / y })
    {
        return Ok(result);
    }
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
    // Truncating division, as R7RS's quotient and remainder are.
    let result = if remainder { &x % &y } else { &x / &y };
    Ok(cx.integer_raw(&result)?)
}

fn quotient<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    division(cx, args, "quotient", false)
}
fn remainder<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    division(cx, args, "remainder", true)
}

/// `modulo`: the remainder of division rounding towards negative infinity,
/// with the sign of the divisor.
fn modulo<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let r = division(cx, args, "modulo", true)?;
    let (r_sign, divisor_sign) = (sign(cx, "modulo", r)?, sign(cx, "modulo", args[1])?);
    if r_sign != 0 && r_sign != divisor_sign {
        cx.arith(Arith::Add, r, args[1])
    } else {
        Ok(r)
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

/// `(error message irritant ...)`: raises an error whose message is
/// `message` followed by each irritant as `write` prints it.
fn error<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut text = if args[0].tag() == Tag::String {
        // SAFETY: a live string.
        unsafe { cx.string_str(args[0]) }.to_owned()
    } else {
        // SAFETY: arguments are valid values.
        let message = unsafe { cx.root(args[0]) };
        cx.write_string(&message)?
    };
    for &irritant in &args[1..] {
        // SAFETY: as above.
        let irritant = unsafe { cx.root(irritant) };
        text.push(' ');
        text.push_str(&cx.write_string(&irritant)?);
    }
    Err(Error::scheme(text))
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

fn string_arg<'a, S: Strategy>(cx: &'a Cx<S>, name: &str, v: Value) -> Result<&'a str, Error> {
    if v.tag() == Tag::String {
        // SAFETY: a live string, read before anything can change it.
        Ok(unsafe { cx.string_str(v) })
    } else {
        Err(type_error(name, "a string", v))
    }
}

fn char_arg(name: &str, v: Value) -> Result<char, Error> {
    v.as_character()
        .ok_or_else(|| type_error(name, "a character", v))
}

fn index_arg(name: &str, v: Value, len: usize) -> Result<usize, Error> {
    match v.as_fixnum() {
        Some(i) if (0..=len as i64).contains(&i) => Ok(i as usize),
        _ => Err(Error::scheme(format!(
            "{name}: index {v:?} is out of range for length {len}"
        ))),
    }
}

fn string_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    string_arg(cx, "string-length", args[0])?;
    // SAFETY: a live string.
    let n = unsafe { cx.string_length(args[0]) };
    Ok(Value::fixnum(n as i64).expect("lengths fit"))
}

/// `string-ref`: a direct index into all-ASCII strings, a scan otherwise,
/// until strings carry index hints (ADR 0002 #11).
fn string_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "string-ref", args[0])?;
    let len = text.chars().count();
    let i = index_arg("string-ref", args[1], len)?;
    let c = if text.is_ascii() {
        text.as_bytes().get(i).map(|&b| char::from(b))
    } else {
        text.chars().nth(i)
    };
    c.map(Value::character).ok_or_else(|| {
        Error::scheme(format!(
            "string-ref: index {i} is out of range for length {len}"
        ))
    })
}

fn string_eq<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let first = string_arg(cx, "string=?", args[0])?;
    let mut result = true;
    for &other in &args[1..] {
        result &= string_arg(cx, "string=?", other)? == first;
    }
    Ok(Value::boolean(result))
}

fn string_lt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut result = true;
    for pair in args.windows(2) {
        result &= string_arg(cx, "string<?", pair[0])? < string_arg(cx, "string<?", pair[1])?;
    }
    Ok(Value::boolean(result))
}

fn string_append<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut text = String::new();
    for &s in args {
        text.push_str(string_arg(cx, "string-append", s)?);
    }
    Ok(cx.string_raw(&text, false)?)
}

fn substring<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "substring", args[0])?;
    let len = text.chars().count();
    let start = index_arg("substring", args[1], len)?;
    let end = index_arg("substring", args[2], len)?;
    if start > end {
        return Err(Error::scheme(format!(
            "substring: start {start} is after end {end}"
        )));
    }
    let part: String = text.chars().skip(start).take(end - start).collect();
    Ok(cx.string_raw(&part, false)?)
}

fn string_to_list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let chars: Vec<char> = string_arg(cx, "string->list", args[0])?.chars().collect();
    let mut list = Value::EMPTY_LIST;
    for c in chars.into_iter().rev() {
        // SAFETY: valid values.
        list = unsafe { cx.cons_raw(Value::character(c), list)? };
    }
    Ok(list)
}

fn list_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = elements(cx, "list->string", args[0])?
        .into_iter()
        .map(|v| char_arg("list->string", v))
        .collect::<Result<String, _>>()?;
    Ok(cx.string_raw(&text, false)?)
}

fn is_string<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::String))
}

fn is_symbol<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::Symbol))
}

fn symbol_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != Tag::Symbol {
        return Err(type_error("symbol->string", "a symbol", args[0]));
    }
    // SAFETY: a live symbol; its name is copied before allocating.
    let name = unsafe { cx.symbol_name(args[0]) }.to_owned();
    // The result must not be modified, so it shares nothing and is
    // immutable, as R7RS allows.
    Ok(cx.string_raw(&name, true)?)
}

fn string_to_symbol<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = string_arg(cx, "string->symbol", args[0])?.to_owned();
    Ok(cx.intern(&name)?)
}

fn is_char<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].as_character().is_some()))
}

fn char_to_integer<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let c = char_arg("char->integer", args[0])?;
    Ok(Value::fixnum(i64::from(u32::from(c))).expect("fits"))
}

fn integer_to_char<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    args[0]
        .as_fixnum()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(char::from_u32)
        .map(Value::character)
        .ok_or_else(|| type_error("integer->char", "a Unicode scalar value", args[0]))
}

fn char_eq<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut result = true;
    for pair in args.windows(2) {
        result &= char_arg("char=?", pair[0])? == char_arg("char=?", pair[1])?;
    }
    Ok(Value::boolean(result))
}

fn char_lt<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut result = true;
    for pair in args.windows(2) {
        result &= char_arg("char<?", pair[0])? < char_arg("char<?", pair[1])?;
    }
    Ok(Value::boolean(result))
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

fn number_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<Num, Error> {
    cx.number(v).ok_or_else(|| type_error(name, "a number", v))
}

fn is_number<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(cx.number(args[0]).is_some()))
}

fn is_integer<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(match cx.number(args[0]) {
        Some(Num::Fixnum(_) | Num::Integer(_)) => true,
        Some(Num::Flonum(x)) => x.is_finite() && x.fract() == 0.0,
        None => false,
    }))
}

fn is_exact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(
        number_arg(cx, "exact?", args[0])?.exact().is_some(),
    ))
}

fn is_inexact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(
        number_arg(cx, "inexact?", args[0])?.exact().is_none(),
    ))
}

/// The sign of a number: -1, 0 or 1, with NaN as 0.
fn sign<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<i32, Error> {
    Ok(match number_arg(cx, name, v)? {
        Num::Fixnum(n) => n.signum() as i32,
        Num::Integer(n) if n.is_zero() => 0,
        Num::Integer(n) => {
            if n.is_negative() {
                -1
            } else {
                1
            }
        }
        Num::Flonum(x) if x > 0.0 => 1,
        Num::Flonum(x) if x < 0.0 => -1,
        Num::Flonum(_) => 0,
    })
}

fn is_zero<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let zero = match number_arg(cx, "zero?", args[0])? {
        Num::Flonum(x) => x == 0.0,
        _ => sign(cx, "zero?", args[0])? == 0,
    };
    Ok(Value::boolean(zero))
}

fn is_positive<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(sign(cx, "positive?", args[0])? > 0))
}

fn is_negative<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(sign(cx, "negative?", args[0])? < 0))
}

fn parity<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<bool, Error> {
    match cx.number(v) {
        Some(Num::Fixnum(n)) => Ok(n % 2 != 0),
        Some(Num::Integer(n)) => Ok(n.limbs().1.first().is_some_and(|l| l & 1 != 0)),
        Some(Num::Flonum(x)) if x.is_finite() && x.fract() == 0.0 => Ok(x % 2.0 != 0.0),
        _ => Err(type_error(name, "an integer", v)),
    }
}

fn is_odd<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(parity(cx, "odd?", args[0])?))
}

fn is_even<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(!parity(cx, "even?", args[0])?))
}

fn abs<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if sign(cx, "abs", args[0])? < 0 {
        cx.arith(Arith::Sub, Value::fixnum(0).expect("fits"), args[0])
    } else {
        Ok(args[0])
    }
}

/// `min` and `max`: if any argument is inexact, so is the result.
fn extremum<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    pick_greater: bool,
) -> Result<Value, Error> {
    let mut best = args[0];
    number_arg(cx, name, best)?;
    let mut inexact = cx.number(best).is_some_and(|n| n.exact().is_none());
    for &x in &args[1..] {
        inexact |= number_arg(cx, name, x)?.exact().is_none();
        let x_less = cx.compare(Compare::Lt, x, best)?;
        let best_less = cx.compare(Compare::Lt, best, x)?;
        if (pick_greater && best_less) || (!pick_greater && x_less) {
            best = x;
        }
    }
    if inexact {
        let x = cx.number(best).expect("checked").to_f64();
        return Ok(cx.flonum_raw(x)?);
    }
    Ok(best)
}

fn min<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extremum(cx, "min", args, false)
}

fn max<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extremum(cx, "max", args, true)
}

fn exact_to_inexact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let x = number_arg(cx, "exact->inexact", args[0])?.to_f64();
    Ok(cx.flonum_raw(x)?)
}

/// `inexact->exact` for whole numbers; other flonums need rationals.
fn inexact_to_exact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match number_arg(cx, "inexact->exact", args[0])? {
        Num::Flonum(x) if x.is_finite() && x.fract() == 0.0 => {
            Ok(cx.integer_raw(&whole_flonum(x))?)
        }
        Num::Flonum(_) => Err(Error::scheme(
            "inexact->exact: rationals are not supported in the heap yet",
        )),
        _ => Ok(args[0]),
    }
}

/// The exact integer a finite, whole flonum equals: its significand shifted
/// by its exponent.
fn whole_flonum(x: f64) -> vali_num::Integer {
    use vali_num::Integer;
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let significand = if exponent == 0 {
        fraction << 1
    } else {
        fraction | 1 << 52
    };
    // x = significand × 2^shift
    let shift = exponent - 1075;
    let mut n = if shift >= 0 {
        let mut n = Integer::from(significand);
        let mut left = shift;
        while left > 0 {
            let step = left.min(62);
            n = &n * &Integer::from(1u64 << step);
            left -= step;
        }
        n
    } else {
        // Whole, so the bits shifted out are zero.
        Integer::from(significand >> (-shift).min(63))
    };
    if x < 0.0 {
        n = &Integer::from(0) - &n;
    }
    n
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

fn radix_arg(name: &str, args: &[Value]) -> Result<vali_num::Radix, Error> {
    use vali_num::Radix;
    match args.get(1).copied().filter(|v| *v != Value::DEFAULT) {
        None => Ok(Radix::Decimal),
        Some(v) => match v.as_fixnum() {
            Some(2) => Ok(Radix::Binary),
            Some(8) => Ok(Radix::Octal),
            Some(10) => Ok(Radix::Decimal),
            Some(16) => Ok(Radix::Hexadecimal),
            _ => Err(type_error(name, "a radix of 2, 8, 10 or 16", v)),
        },
    }
}

fn number_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let radix = radix_arg("number->string", args)?;
    let number = match number_arg(cx, "number->string", args[0])? {
        Num::Fixnum(n) => vali_num::Number::Integer(n.into()),
        Num::Integer(n) => vali_num::Number::Integer(n),
        Num::Flonum(x) => vali_num::Number::Real(x),
    };
    let text = number
        .to_string_radix(radix)
        .ok_or_else(|| Error::scheme("number->string: inexact numbers print only in radix 10"))?;
    Ok(cx.string_raw(&text, false)?)
}

/// `string->number`: `#f` for text that is not a number.
fn string_to_number<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let radix = radix_arg("string->number", args)?;
    let text = string_arg(cx, "string->number", args[0])?.to_owned();
    match vali_num::parse(&text, radix) {
        Err(_) => Ok(Value::FALSE),
        Ok(vali_num::Number::Integer(n)) => Ok(cx.integer_raw(&n)?),
        Ok(vali_num::Number::Real(x)) => Ok(cx.flonum_raw(x)?),
        Ok(_) => Err(Error::scheme(
            "string->number: rationals and complex numbers are not supported in the heap yet",
        )),
    }
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

/// A one-character case mapping, keeping the character when Unicode maps it
/// to several.
fn single(mut mapped: impl Iterator<Item = char>, c: char) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m,
        _ => c,
    }
}

fn char_upcase<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let c = char_arg("char-upcase", args[0])?;
    Ok(Value::character(single(c.to_uppercase(), c)))
}

fn char_downcase<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let c = char_arg("char-downcase", args[0])?;
    Ok(Value::character(single(c.to_lowercase(), c)))
}

fn char_alphabetic<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(
        char_arg("char-alphabetic?", args[0])?.is_alphabetic(),
    ))
}

fn char_numeric<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(
        char_arg("char-numeric?", args[0])?.is_numeric(),
    ))
}

fn char_whitespace<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(
        char_arg("char-whitespace?", args[0])?.is_whitespace(),
    ))
}

fn string_upcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "string-upcase", args[0])?.to_uppercase();
    Ok(cx.string_raw(&text, false)?)
}

fn string_downcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "string-downcase", args[0])?.to_lowercase();
    Ok(cx.string_raw(&text, false)?)
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
    vec![
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
        ("modulo", Arity::exactly(2), modulo),
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
        ("string?", Arity::exactly(1), is_string),
        ("string-length", Arity::exactly(1), string_length),
        ("string-ref", Arity::exactly(2), string_ref),
        ("string=?", at_least(1), string_eq),
        ("string<?", at_least(1), string_lt),
        ("string-append", at_least(0), string_append),
        ("substring", Arity::exactly(3), substring),
        ("string->list", Arity::exactly(1), string_to_list),
        ("list->string", Arity::exactly(1), list_to_string),
        ("symbol?", Arity::exactly(1), is_symbol),
        ("symbol->string", Arity::exactly(1), symbol_to_string),
        ("string->symbol", Arity::exactly(1), string_to_symbol),
        ("char?", Arity::exactly(1), is_char),
        ("char->integer", Arity::exactly(1), char_to_integer),
        ("integer->char", Arity::exactly(1), integer_to_char),
        ("char=?", at_least(1), char_eq),
        ("char<?", at_least(1), char_lt),
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
        ("char-upcase", Arity::exactly(1), char_upcase),
        ("char-downcase", Arity::exactly(1), char_downcase),
        ("char-alphabetic?", Arity::exactly(1), char_alphabetic),
        ("char-numeric?", Arity::exactly(1), char_numeric),
        ("char-whitespace?", Arity::exactly(1), char_whitespace),
        ("string-upcase", Arity::exactly(1), string_upcase),
        ("string-downcase", Arity::exactly(1), string_downcase),
        (
            "number->string",
            Arity {
                required: 1,
                optional: 1,
                rest: false,
            },
            number_to_string,
        ),
        (
            "string->number",
            Arity {
                required: 1,
                optional: 1,
                rest: false,
            },
            string_to_number,
        ),
        ("number?", Arity::exactly(1), is_number),
        ("integer?", Arity::exactly(1), is_integer),
        ("exact?", Arity::exactly(1), is_exact),
        ("inexact?", Arity::exactly(1), is_inexact),
        ("zero?", Arity::exactly(1), is_zero),
        ("positive?", Arity::exactly(1), is_positive),
        ("negative?", Arity::exactly(1), is_negative),
        ("odd?", Arity::exactly(1), is_odd),
        ("even?", Arity::exactly(1), is_even),
        ("abs", Arity::exactly(1), abs),
        ("min", at_least(1), min),
        ("max", at_least(1), max),
        ("exact->inexact", Arity::exactly(1), exact_to_inexact),
        ("inexact->exact", Arity::exactly(1), inexact_to_exact),
        ("boolean?", Arity::exactly(1), is_boolean),
        ("procedure?", Arity::exactly(1), is_procedure),
        ("vector?", Arity::exactly(1), is_vector),
        ("list?", Arity::exactly(1), is_list),
        ("symbol=?", at_least(1), symbol_eq),
        ("set-car!", Arity::exactly(2), set_car),
        ("set-cdr!", Arity::exactly(2), set_cdr),
        ("error", at_least(1), error),
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
    ]
}

/// The names of the standard procedures implemented in Rust, which
/// [`Cx::install_primitives`] binds.
pub fn primitive_names() -> Vec<&'static str> {
    table::<crate::NoCollection>()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

impl<S: Strategy> Cx<S> {
    /// Binds the standard procedures implemented so far as globals.
    pub fn install_primitives(&mut self) -> Result<(), Error> {
        for (name, arity, f) in table::<S>() {
            self.define_native(name, arity, f)?;
        }
        Ok(())
    }
}
