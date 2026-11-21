//! The fixnum primitives of SRFI 143 (ADR 0007).
//!
//! Vali's fixnums are 61-bit two's-complement integers, `fx-width` 61. The
//! SRFI makes an argument that is not a fixnum, and a result that is not
//! representable as one, an error that an implementation need not signal.
//! These primitives always signal it: a non-fixnum argument is a type
//! error, and a result outside the fixnum range is an error that says
//! "fixnum overflow". They never wrap around, and never return a bignum.

use vali_abi::fixnum;
use vali_bytecode::Arity;

use crate::code::NativeFn;
use crate::cx::Cx;
use crate::handle::Error;
use crate::records;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::wrong_type;

/// The width of a fixnum in bits.
const WIDTH: i64 = fixnum::BITS as i64;

fn fx<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<i64, Error> {
    v.as_fixnum()
        .ok_or_else(|| wrong_type(cx, name, "a fixnum", v))
}

fn overflow(name: &str) -> Error {
    Error::scheme(format!("{name}: fixnum overflow"))
}

/// `n` as a fixnum value, or an overflow error from `name`.
fn make(name: &str, n: i128) -> Result<Value, Error> {
    i64::try_from(n)
        .ok()
        .and_then(Value::fixnum)
        .ok_or_else(|| overflow(name))
}

fn bool_value(b: bool) -> Result<Value, Error> {
    Ok(Value::boolean(b))
}

fn fixnum_p<S: Strategy>(_: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    bool_value(args[0].as_fixnum().is_some())
}

/// A chain of comparisons over fixnums, every one of them checked.
fn chain<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    args: &[Value],
    holds: fn(i64, i64) -> bool,
) -> Result<Value, Error> {
    let mut all = Vec::with_capacity(args.len());
    for &a in args {
        all.push(fx(cx, name, a)?);
    }
    bool_value(all.windows(2).all(|w| holds(w[0], w[1])))
}

macro_rules! comparisons {
    ($($f:ident $name:literal $op:tt),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            chain(cx, $name, args, |a, b| a $op b)
        }
    )*};
}
comparisons!(
    fx_eq "fx=?" ==, fx_lt "fx<?" <, fx_gt "fx>?" >, fx_le "fx<=?" <=, fx_ge "fx>=?" >=,
);

macro_rules! tests {
    ($($f:ident $name:literal $test:expr),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            let test: fn(i64) -> bool = $test;
            bool_value(test(fx(cx, $name, args[0])?))
        }
    )*};
}
tests!(
    fx_zero "fxzero?" |i| i == 0,
    fx_positive "fxpositive?" |i| i > 0,
    fx_negative "fxnegative?" |i| i < 0,
    fx_odd "fxodd?" |i| i & 1 == 1,
    fx_even "fxeven?" |i| i & 1 == 0,
);

fn extreme<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    args: &[Value],
    pick: fn(i64, i64) -> i64,
) -> Result<Value, Error> {
    let mut acc = fx(cx, name, args[0])?;
    for &a in &args[1..] {
        acc = pick(acc, fx(cx, name, a)?);
    }
    make(name, acc.into())
}
fn fx_max<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extreme(cx, "fxmax", args, i64::max)
}
fn fx_min<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extreme(cx, "fxmin", args, i64::min)
}

macro_rules! binary {
    ($($f:ident $name:literal |$a:ident, $b:ident| $body:expr),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            let ($a, $b) = (fx(cx, $name, args[0])? as i128, fx(cx, $name, args[1])? as i128);
            let r: Option<i128> = $body;
            make($name, r.ok_or_else(|| Error::scheme(concat!($name, ": division by zero")))?)
        }
    )*};
}
binary!(
    fx_add "fx+" |a, b| Some(a + b),
    fx_sub "fx-" |a, b| Some(a - b),
    fx_mul "fx*" |a, b| Some(a * b),
    fx_quotient "fxquotient" |a, b| a.checked_div(b),
    fx_remainder "fxremainder" |a, b| a.checked_rem(b),
);

fn fx_neg<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    make("fxneg", -i128::from(fx(cx, "fxneg", args[0])?))
}
fn fx_abs<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    make("fxabs", i128::from(fx(cx, "fxabs", args[0])?).abs())
}
fn fx_square<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = i128::from(fx(cx, "fxsquare", args[0])?);
    make("fxsquare", i * i)
}

/// `(fxsqrt i)`: `s` and `r` with `i = s² + r`.
fn fx_sqrt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = fx(cx, "fxsqrt", args[0])?;
    if i < 0 {
        return Err(wrong_type(cx, "fxsqrt", "a non-negative fixnum", args[0]));
    }
    let mut s = (i as f64).sqrt() as i64;
    while s * s > i {
        s -= 1;
    }
    while (s + 1) * (s + 1) <= i {
        s += 1;
    }
    let (s, r) = (
        make("fxsqrt", s.into())?,
        make("fxsqrt", (i - s * s).into())?,
    );
    records::values(cx, &[s, r])
}

/// The carry operations: `s` split by balanced division by 2^fx-width, as
/// (values remainder quotient).
fn carry<S: Strategy>(cx: &mut Cx<S>, name: &str, s: i128) -> Result<Value, Error> {
    let modulus = 1i128 << WIDTH;
    let half = modulus / 2;
    let r = (s + half).rem_euclid(modulus) - half;
    let q = (s - r) / modulus;
    let (r, q) = (make(name, r)?, make(name, q)?);
    records::values(cx, &[r, q])
}

fn fx_add_carry<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fx+/carry";
    let (i, j, k) = (
        fx(cx, n, args[0])?,
        fx(cx, n, args[1])?,
        fx(cx, n, args[2])?,
    );
    carry(cx, n, i128::from(i) + i128::from(j) + i128::from(k))
}
fn fx_sub_carry<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fx-/carry";
    let (i, j, k) = (
        fx(cx, n, args[0])?,
        fx(cx, n, args[1])?,
        fx(cx, n, args[2])?,
    );
    carry(cx, n, i128::from(i) - i128::from(j) - i128::from(k))
}
fn fx_mul_carry<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fx*/carry";
    let (i, j, k) = (
        fx(cx, n, args[0])?,
        fx(cx, n, args[1])?,
        fx(cx, n, args[2])?,
    );
    carry(cx, n, i128::from(i) * i128::from(j) + i128::from(k))
}

fn logic<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    identity: i64,
    args: &[Value],
    op: fn(i64, i64) -> i64,
) -> Result<Value, Error> {
    let mut acc = identity;
    for &a in args {
        acc = op(acc, fx(cx, name, a)?);
    }
    make(name, acc.into())
}
fn fx_and<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    logic(cx, "fxand", -1, args, |a, b| a & b)
}
fn fx_ior<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    logic(cx, "fxior", 0, args, |a, b| a | b)
}
fn fx_xor<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    logic(cx, "fxxor", 0, args, |a, b| a ^ b)
}
fn fx_not<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    make("fxnot", (!fx(cx, "fxnot", args[0])?).into())
}

/// `(fxif mask i j)`.
fn fx_if<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxif";
    let (m, i, j) = (
        fx(cx, n, args[0])?,
        fx(cx, n, args[1])?,
        fx(cx, n, args[2])?,
    );
    make(n, ((m & i) | (!m & j)).into())
}

/// A shift of `i` by `count`: left when `count` is positive. `|count|` may
/// not pass fx-width - 1, and `direction` limits which way it may go.
fn shift<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    args: &[Value],
    direction: Option<bool>,
) -> Result<Value, Error> {
    let (i, count) = (fx(cx, name, args[0])?, fx(cx, name, args[1])?);
    let allowed = match direction {
        None => count.abs() < WIDTH,
        Some(left) => count.abs() < WIDTH && (count == 0 || (count > 0) == left),
    };
    if !allowed {
        return Err(wrong_type(
            cx,
            name,
            "a shift count within the fixnum width",
            args[1],
        ));
    }
    if count >= 0 {
        make(name, i128::from(i) << count)
    } else {
        make(name, (i >> -count).into())
    }
}
fn fx_shift<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    shift(cx, "fxarithmetic-shift", args, None)
}
fn fx_shift_left<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    shift(cx, "fxarithmetic-shift-left", args, Some(true))
}
fn fx_shift_right<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // A shift right by `count`, which the shared check sees as left by
    // `-count`.
    let name = "fxarithmetic-shift-right";
    let (i, count) = (fx(cx, name, args[0])?, fx(cx, name, args[1])?);
    if !(0..WIDTH).contains(&count) {
        return Err(wrong_type(
            cx,
            name,
            "a shift count within the fixnum width",
            args[1],
        ));
    }
    make(name, (i >> count).into())
}

fn fx_bit_count<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = fx(cx, "fxbit-count", args[0])?;
    make(
        "fxbit-count",
        (if i < 0 { !i } else { i }).count_ones().into(),
    )
}

fn fx_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = fx(cx, "fxlength", args[0])?;
    let magnitude = if i < 0 { !i } else { i };
    make("fxlength", (64 - magnitude.leading_zeros()).into())
}

fn fx_first_set_bit<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let i = fx(cx, "fxfirst-set-bit", args[0])?;
    make(
        "fxfirst-set-bit",
        if i == 0 {
            -1
        } else {
            i.trailing_zeros().into()
        },
    )
}

/// A bit index, which must be within the fixnum width.
fn index<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<i64, Error> {
    match v.as_fixnum() {
        Some(n) if (0..WIDTH).contains(&n) => Ok(n),
        _ => Err(wrong_type(
            cx,
            name,
            "a bit index below the fixnum width",
            v,
        )),
    }
}

fn fx_bit_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxbit-set?";
    let (k, i) = (index(cx, n, args[0])?, fx(cx, n, args[1])?);
    bool_value((i >> k) & 1 == 1)
}

fn fx_copy_bit<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxcopy-bit";
    let (k, i) = (index(cx, n, args[0])?, fx(cx, n, args[1])?);
    let Some(on) = args[2].as_boolean() else {
        return Err(wrong_type(cx, n, "a boolean", args[2]));
    };
    let bit = 1i128 << k;
    let i = i128::from(i);
    make(n, if on { i | bit } else { i & !bit })
}

/// A field `start` up to `end`, as a pair of bit positions, no later than
/// `limit`.
fn field<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    start: Value,
    end: Value,
    limit: i64,
) -> Result<(i64, i64), Error> {
    let (s, e) = (fx(cx, name, start)?, fx(cx, name, end)?);
    if s < 0 {
        return Err(wrong_type(cx, name, "a non-negative field start", start));
    }
    if e < s || e > limit {
        return Err(wrong_type(cx, name, "a field end within the fixnum", end));
    }
    Ok((s, e))
}

/// `(fxbit-field i start end)`. A field past the top reads as `i`'s sign.
fn fx_bit_field<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxbit-field";
    let i = fx(cx, n, args[0])?;
    let (start, end) = field(cx, n, args[1], args[2], i64::MAX)?;
    let shifted = i >> start.min(WIDTH);
    let width = end - start;
    if width >= WIDTH {
        // A negative integer has no top to stop at: its field is not a
        // fixnum.
        return if shifted < 0 {
            Err(overflow(n))
        } else {
            make(n, shifted.into())
        };
    }
    make(n, (shifted & ((1 << width) - 1)).into())
}

/// A word with the bits of `word` from `start` up to `end` replaced by
/// `new`, as a fixnum.
fn replace(word: i64, new: u64, start: i64, end: i64) -> i64 {
    let mask = ((1u64 << (end - start)) - 1) << start;
    let w = (word as u64 & !mask) | ((new << start) & mask);
    ((w << 3) as i64) >> 3
}

/// `(fxbit-field-rotate i count start end)`. The field must lie within the
/// fixnum's width.
fn fx_bit_field_rotate<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxbit-field-rotate";
    let (i, count) = (fx(cx, n, args[0])?, fx(cx, n, args[1])?);
    let (start, end) = field(cx, n, args[2], args[3], WIDTH)?;
    let width = end - start;
    if width == 0 {
        return Ok(args[0]);
    }
    let c = count.rem_euclid(width);
    let mask = (1u64 << width) - 1;
    let f = (i as u64 >> start) & mask;
    let rotated = ((f << c) | (f >> (width - c))) & mask;
    make(n, replace(i, rotated, start, end).into())
}

/// `(fxbit-field-reverse i start end)`. The field must lie within the
/// fixnum's width.
fn fx_bit_field_reverse<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fxbit-field-reverse";
    let i = fx(cx, n, args[0])?;
    let (start, end) = field(cx, n, args[1], args[2], WIDTH)?;
    let width = end - start;
    if width == 0 {
        return Ok(args[0]);
    }
    let f = (i as u64 >> start) & ((1u64 << width) - 1);
    let reversed = f.reverse_bits() >> (64 - width);
    make(n, replace(i, reversed, start, end).into())
}

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

/// The fixnum procedures implemented in Rust.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    let one = Arity::exactly(1);
    let two = Arity::exactly(2);
    let three = Arity::exactly(3);
    vec![
        ("fixnum?", one, fixnum_p),
        ("fx=?", at_least(1), fx_eq),
        ("fx<?", at_least(1), fx_lt),
        ("fx>?", at_least(1), fx_gt),
        ("fx<=?", at_least(1), fx_le),
        ("fx>=?", at_least(1), fx_ge),
        ("fxzero?", one, fx_zero),
        ("fxpositive?", one, fx_positive),
        ("fxnegative?", one, fx_negative),
        ("fxodd?", one, fx_odd),
        ("fxeven?", one, fx_even),
        ("fxmax", at_least(1), fx_max),
        ("fxmin", at_least(1), fx_min),
        ("fx+", two, fx_add),
        ("fx-", two, fx_sub),
        ("fxneg", one, fx_neg),
        ("fx*", two, fx_mul),
        ("fxquotient", two, fx_quotient),
        ("fxremainder", two, fx_remainder),
        ("fxabs", one, fx_abs),
        ("fxsquare", one, fx_square),
        ("fxsqrt", one, fx_sqrt),
        ("fx+/carry", three, fx_add_carry),
        ("fx-/carry", three, fx_sub_carry),
        ("fx*/carry", three, fx_mul_carry),
        ("fxnot", one, fx_not),
        ("fxand", at_least(0), fx_and),
        ("fxior", at_least(0), fx_ior),
        ("fxxor", at_least(0), fx_xor),
        ("fxarithmetic-shift", two, fx_shift),
        ("fxarithmetic-shift-left", two, fx_shift_left),
        ("fxarithmetic-shift-right", two, fx_shift_right),
        ("fxbit-count", one, fx_bit_count),
        ("fxlength", one, fx_length),
        ("fxif", three, fx_if),
        ("fxbit-set?", two, fx_bit_set),
        ("fxcopy-bit", three, fx_copy_bit),
        ("fxfirst-set-bit", one, fx_first_set_bit),
        ("fxbit-field", three, fx_bit_field),
        ("fxbit-field-rotate", Arity::exactly(4), fx_bit_field_rotate),
        ("fxbit-field-reverse", three, fx_bit_field_reverse),
    ]
}
