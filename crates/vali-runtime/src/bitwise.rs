//! The bitwise primitives of SRFI 151 (ADR 0007), over fixnums and
//! bignums with two's-complement semantics for negatives.
//!
//! Fixnum arguments take a fast path on machine words: the result of
//! `and`, `ior`, `xor` and `not` on fixnums is always a fixnum. Anything
//! else goes through `vali_num::bitwise`, whose results are bounded in
//! size, so a huge shift is a Scheme error and not an allocation.

use vali_bytecode::Arity;
use vali_num::arith::ArithError;
use vali_num::{Integer, Number, bitwise};

use crate::code::NativeFn;
use crate::cx::Cx;
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::wrong_type;

const EXACT_INTEGER: &str = "an exact integer";

impl<S: Strategy> Cx<S> {
    /// The exact integer `v` is, or a type error from `name`.
    pub(crate) fn integer_arg(&self, name: &str, v: Value) -> Result<Integer, Error> {
        if let Some(n) = v.as_fixnum() {
            return Ok(Integer::from(n));
        }
        match self.number(v) {
            Some(Number::Integer(i)) => Ok(i),
            _ => Err(wrong_type(self, name, EXACT_INTEGER, v)),
        }
    }

    /// An integer as a value, or a Scheme error from `name` if it is out of
    /// memory.
    fn integer_result(&mut self, i: &Integer) -> Result<Value, Error> {
        Ok(self.integer_raw(i)?)
    }
}

/// `e` as an error from `name`, blaming `culprit` for a domain error.
fn bit_error<S: Strategy>(cx: &Cx<S>, name: &str, e: ArithError, culprit: Value) -> Error {
    match e {
        ArithError::Domain(expected) => wrong_type(cx, name, expected, culprit),
        other => Error::scheme(format!("{name}: {other}")),
    }
}

fn boolean_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<bool, Error> {
    v.as_boolean()
        .ok_or_else(|| wrong_type(cx, name, "a boolean", v))
}

/// `(bitwise-and i ...)` and its kin: a fold of one operation, from the
/// identity.
fn fold<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    identity: i64,
    args: &[Value],
    fast: fn(i64, i64) -> i64,
    slow: fn(&Integer, &Integer) -> Integer,
) -> Result<Value, Error> {
    for &a in args {
        cx.integer_arg(name, a)?;
    }
    let mut acc = Value::fixnum(identity).expect("a small fixnum");
    for &x in args {
        acc = match (acc.as_fixnum(), x.as_fixnum()) {
            (Some(a), Some(b)) => Value::fixnum(fast(a, b)).expect("a fixnum result"),
            _ => {
                let r = slow(&cx.integer_arg(name, acc)?, &cx.integer_arg(name, x)?);
                cx.integer_result(&r)?
            }
        };
    }
    Ok(acc)
}

fn bitwise_and<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "bitwise-and", -1, args, |a, b| a & b, bitwise::and)
}
fn bitwise_ior<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "bitwise-ior", 0, args, |a, b| a | b, bitwise::ior)
}
fn bitwise_xor<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "bitwise-xor", 0, args, |a, b| a ^ b, bitwise::xor)
}

fn bitwise_not<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if let Some(n) = args[0].as_fixnum() {
        return Ok(Value::fixnum(!n).expect("a fixnum result"));
    }
    let i = cx.integer_arg("bitwise-not", args[0])?;
    cx.integer_result(&bitwise::not(&i))
}

fn arithmetic_shift<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "arithmetic-shift";
    let (i, count) = (
        cx.integer_arg(name, args[0])?,
        cx.integer_arg(name, args[1])?,
    );
    let r = bitwise::arithmetic_shift(&i, &count).map_err(|e| bit_error(cx, name, e, args[1]))?;
    cx.integer_result(&r)
}

fn bit_count<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = cx.integer_arg("bit-count", args[0])?;
    cx.integer_result(&Integer::from(bitwise::bit_count(&i)))
}

fn integer_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = cx.integer_arg("integer-length", args[0])?;
    cx.integer_result(&Integer::from(bitwise::integer_length(&i)))
}

/// `(bit-set? index i)`.
fn bit_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "bit-set?";
    let (index, i) = (
        cx.integer_arg(name, args[0])?,
        cx.integer_arg(name, args[1])?,
    );
    let r = bitwise::bit_set(&index, &i).map_err(|e| bit_error(cx, name, e, args[0]))?;
    Ok(Value::boolean(r))
}

fn first_set_bit<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = cx.integer_arg("first-set-bit", args[0])?;
    cx.integer_result(&Integer::from(bitwise::first_set_bit(&i)))
}

/// `(copy-bit index i boolean)`.
fn copy_bit<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "copy-bit";
    let (index, i) = (
        cx.integer_arg(name, args[0])?,
        cx.integer_arg(name, args[1])?,
    );
    let value = boolean_arg(cx, name, args[2])?;
    let r = bitwise::copy_bit(&index, &i, value).map_err(|e| bit_error(cx, name, e, args[0]))?;
    cx.integer_result(&r)
}

/// `(bit-field i start end)`.
fn bit_field<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "bit-field";
    let i = cx.integer_arg(name, args[0])?;
    let start = cx.integer_arg(name, args[1])?;
    let end = cx.integer_arg(name, args[2])?;
    let r = bitwise::bit_field(&i, &start, &end).map_err(|e| {
        let culprit = if start.is_negative() {
            args[1]
        } else {
            args[2]
        };
        bit_error(cx, name, e, culprit)
    })?;
    cx.integer_result(&r)
}

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

/// The bitwise procedures implemented in Rust.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    vec![
        ("bitwise-and", at_least(0), bitwise_and),
        ("bitwise-ior", at_least(0), bitwise_ior),
        ("bitwise-xor", at_least(0), bitwise_xor),
        ("bitwise-not", Arity::exactly(1), bitwise_not),
        ("arithmetic-shift", Arity::exactly(2), arithmetic_shift),
        ("bit-count", Arity::exactly(1), bit_count),
        ("integer-length", Arity::exactly(1), integer_length),
        ("bit-set?", Arity::exactly(2), bit_set),
        ("first-set-bit", Arity::exactly(1), first_set_bit),
        ("copy-bit", Arity::exactly(3), copy_bit),
        ("bit-field", Arity::exactly(3), bit_field),
    ]
}
