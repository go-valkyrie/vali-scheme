//! Numbers in the heap, and everything numeric off the VM's fixnum fast
//! paths (ADR 0002, ADR 0007).
//!
//! Fixnums and flonums have tags; bignums, ratnums and complex numbers are
//! headered objects. A ratnum holds its numerator and denominator, and an
//! exact complex number its two exact real parts, all as values; an inexact
//! complex number holds the bits of its two flonums. Arithmetic on them goes
//! through `vali_num::arith`, which decides exactness as R7RS does.

use std::cmp::Ordering;

use vali_abi::{Tag, TypeCode};
use vali_bytecode::Arity;
use vali_num::arith::{self, ArithError, Class, Division, Rounding, Trig};
use vali_num::{Number, Radix};

use crate::code::NativeFn;
use crate::cx::{Cx, get, put};
use crate::handle::Error;
use crate::page::OutOfMemory;
use crate::records;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::{Arith, Compare, type_error, wrong_type};

impl<S: Strategy> Cx<S> {
    /// The number `v` is, if it is one.
    pub(crate) fn number(&self, v: Value) -> Option<Number> {
        if let Some(n) = v.as_fixnum() {
            return Some(Number::from(n));
        }
        // SAFETY: a valid value with a pointer tag points to a live object
        // of its type, and a number's fields hold what its type says.
        unsafe {
            match v.tag() {
                Tag::Flonum => Some(Number::Real(self.flonum_value(v))),
                Tag::Other => {
                    let field = |t, i| get(v, Self::LAYOUT.field(t, i));
                    let value = |t, i| self.number(Value::from_raw(field(t, i)));
                    match self.header_type(v)? {
                        TypeCode::Bignum => Some(Number::Integer(self.bignum_value(v))),
                        TypeCode::Ratnum => {
                            let (n, d) = (value(TypeCode::Ratnum, 0)?, value(TypeCode::Ratnum, 1)?);
                            arith::div(&n, &d).ok()
                        }
                        TypeCode::ExactComplex => {
                            let t = TypeCode::ExactComplex;
                            arith::make_rectangular(&value(t, 0)?, &value(t, 1)?).ok()
                        }
                        TypeCode::InexactComplex => {
                            let t = TypeCode::InexactComplex;
                            let re = Number::Real(f64::from_bits(field(t, 0)));
                            let im = Number::Real(f64::from_bits(field(t, 1)));
                            arith::make_rectangular(&re, &im).ok()
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    /// `n` as a value: a fixnum or a flonum if it is one, a heap object
    /// otherwise.
    pub(crate) fn number_raw(&mut self, n: &Number) -> Result<Value, OutOfMemory> {
        match n {
            Number::Integer(i) => self.integer_raw(i),
            Number::Real(x) => self.flonum_raw(*x),
            Number::Rational(r) => {
                let numerator = self.integer_raw(&r.numerator())?;
                let denominator = self.integer_raw(&r.denominator())?;
                self.number_object(TypeCode::Ratnum, numerator, denominator)
            }
            Number::Complex(c) if c.is_exact() => {
                let re = self.number_raw(&c.real_part())?;
                let im = self.number_raw(&c.imag_part())?;
                self.number_object(TypeCode::ExactComplex, re, im)
            }
            Number::Complex(c) => {
                let (Number::Real(re), Number::Real(im)) = (c.real_part(), c.imag_part()) else {
                    unreachable!("an inexact complex number has inexact parts")
                };
                let t = TypeCode::InexactComplex;
                let v = self.new_object(t, 2, 0)?;
                // SAFETY: just allocated with room for two words.
                unsafe {
                    put(v, Self::LAYOUT.field(t, 0), re.to_bits());
                    put(v, Self::LAYOUT.field(t, 1), im.to_bits());
                }
                Ok(v)
            }
            _ => unreachable!("vali-num has no other kinds of number"),
        }
    }

    /// A ratnum or an exact complex number from its two parts.
    fn number_object(&mut self, t: TypeCode, a: Value, b: Value) -> Result<Value, OutOfMemory> {
        let v = self.new_object(t, 2, 0)?;
        // SAFETY: just allocated with room for two fields.
        unsafe {
            self.init_field(v, Self::LAYOUT.field(t, 0), a);
            self.init_field(v, Self::LAYOUT.field(t, 1), b);
        }
        Ok(v)
    }

    /// The number `v` is, or an error from `name`.
    fn number_arg(&self, name: &str, v: Value) -> Result<Number, Error> {
        self.number(v)
            .ok_or_else(|| wrong_type(self, name, "a number", v))
    }

    /// Generic arithmetic: the slow path behind the fixnum fast paths.
    pub(crate) fn arith(&mut self, op: Arith, a: Value, b: Value) -> Result<Value, Error> {
        if let (Some(x), Some(y)) = (a.as_fixnum(), b.as_fixnum()) {
            let result = match op {
                Arith::Add => x.checked_add(y),
                Arith::Sub => x.checked_sub(y),
                Arith::Mul => x.checked_mul(y),
            };
            if let Some(v) = result.and_then(Value::fixnum) {
                return Ok(v);
            }
        }
        let name = match op {
            Arith::Add => "+",
            Arith::Sub => "-",
            Arith::Mul => "*",
        };
        let (x, y) = (self.number_arg(name, a)?, self.number_arg(name, b)?);
        let result = match op {
            Arith::Add => arith::add(&x, &y),
            Arith::Sub => arith::sub(&x, &y),
            Arith::Mul => arith::mul(&x, &y),
        };
        Ok(self.number_raw(&result)?)
    }

    /// Generic comparison. A NaN compares false with everything.
    pub(crate) fn compare(&self, op: Compare, a: Value, b: Value) -> Result<bool, Error> {
        let name = match op {
            Compare::Lt => "<",
            Compare::Le => "<=",
            Compare::Eq => "=",
        };
        let (x, y) = (self.number_arg(name, a)?, self.number_arg(name, b)?);
        if let Compare::Eq = op {
            return Ok(arith::num_eq(&x, &y));
        }
        let ordering = arith::compare(&x, &y).map_err(|_| {
            wrong_type(self, name, "a real number", if x.is_real() { b } else { a })
        })?;
        Ok(match (op, ordering) {
            (_, None) => false,
            (Compare::Lt, Some(o)) => o.is_lt(),
            (Compare::Le, Some(o)) => o.is_le(),
            (Compare::Eq, Some(o)) => o.is_eq(),
        })
    }
}

/// An arithmetic error as an error from `name`, blaming the first of `args`
/// outside the operation's domain.
fn arith_error<S: Strategy>(cx: &Cx<S>, name: &str, e: ArithError, args: &[Value]) -> Error {
    match e {
        ArithError::Domain(expected) => {
            let fits = |n: &Number| match expected {
                "a real number" => n.is_real(),
                "an integer" => n.is_integer(),
                "a rational number" => n.is_rational(),
                "a finite number" => arith::classify(n, Class::Finite),
                "an exact non-negative integer" => {
                    matches!(n, Number::Integer(i) if !i.is_negative())
                }
                _ => true,
            };
            let culprit = args
                .iter()
                .copied()
                .find(|&v| cx.number(v).is_some_and(|n| !fits(&n)))
                .unwrap_or(args[0]);
            wrong_type(cx, name, expected, culprit)
        }
        other => Error::scheme(format!("{name}: {other}")),
    }
}

/// A one-argument operation on numbers that cannot fail.
fn unary<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    v: Value,
    f: impl FnOnce(&Number) -> Number,
) -> Result<Value, Error> {
    let n = cx.number_arg(name, v)?;
    Ok(cx.number_raw(&f(&n))?)
}

/// A one-argument operation on numbers that can fail.
fn unary_checked<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    v: Value,
    f: impl FnOnce(&Number) -> arith::Result<Number>,
) -> Result<Value, Error> {
    let n = cx.number_arg(name, v)?;
    let result = f(&n).map_err(|e| arith_error(cx, name, e, &[v]))?;
    Ok(cx.number_raw(&result)?)
}

/// A two-argument operation on numbers that can fail.
fn binary<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    a: Value,
    b: Value,
    f: impl FnOnce(&Number, &Number) -> arith::Result<Number>,
) -> Result<Value, Error> {
    let (x, y) = (cx.number_arg(name, a)?, cx.number_arg(name, b)?);
    let result = f(&x, &y).map_err(|e| arith_error(cx, name, e, &[a, b]))?;
    Ok(cx.number_raw(&result)?)
}

/// Whether an optional argument was left out.
fn absent(args: &[Value], i: usize) -> bool {
    args.get(i).is_none_or(|&v| v == Value::DEFAULT)
}

const ONE: Value = match Value::fixnum(1) {
    Some(v) => v,
    None => unreachable!(),
};

const ZERO: Value = match Value::fixnum(0) {
    Some(v) => v,
    None => unreachable!(),
};

const MINUS_ONE: Value = match Value::fixnum(-1) {
    Some(v) => v,
    None => unreachable!(),
};

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

fn add<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, Arith::Add, ZERO, args)
}

fn mul<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, Arith::Mul, ONE, args)
}

fn sub<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match args {
        // Negation, as -1 times x rather than 0 minus x: (- 0.0) is -0.0.
        [x] => cx.arith(Arith::Mul, MINUS_ONE, *x),
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

fn div<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match args {
        [x] => binary(cx, "/", ONE, *x, arith::div),
        [x, rest @ ..] => {
            let mut acc = *x;
            for &y in rest {
                acc = binary(cx, "/", acc, y, arith::div)?;
            }
            Ok(acc)
        }
        [] => unreachable!("arity checked"),
    }
}

/// A chain of comparisons: `(< a b c)` is `a < b` and `b < c`. `>` and
/// `>=` swap their operands.
fn chain<S: Strategy>(cx: &Cx<S>, op: Compare, swap: bool, args: &[Value]) -> Result<Value, Error> {
    let mut result = true;
    if let [x] = args {
        cx.compare(op, *x, *x)?;
    }
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

/// Which results of an integer division a procedure returns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Quotient,
    Remainder,
    Both,
}

/// The integer divisions: `quotient`, `remainder`, `modulo`, and R7RS's
/// `floor/` and `truncate/` families. Two fixnums take a fast path.
fn integer_division<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    division: Division,
    part: Part,
) -> Result<Value, Error> {
    let (a, b) = (args[0], args[1]);
    if let (Some(x), Some(y)) = (a.as_fixnum(), b.as_fixnum())
        && y != 0
        && let (Some(mut q), Some(mut r)) = (x.checked_div(y), x.checked_rem(y))
    {
        if division == Division::Floor && r != 0 && (r < 0) != (y < 0) {
            q -= 1;
            r += y;
        }
        if let (Some(q), Some(r)) = (Value::fixnum(q), Value::fixnum(r)) {
            return match part {
                Part::Quotient => Ok(q),
                Part::Remainder => Ok(r),
                Part::Both => records::values(cx, &[q, r]),
            };
        }
    }
    let (x, y) = (cx.number_arg(name, a)?, cx.number_arg(name, b)?);
    let (q, r) = arith::divide(&x, &y, division).map_err(|e| arith_error(cx, name, e, args))?;
    match part {
        Part::Quotient => Ok(cx.number_raw(&q)?),
        Part::Remainder => Ok(cx.number_raw(&r)?),
        Part::Both => {
            let (q, r) = (cx.number_raw(&q)?, cx.number_raw(&r)?);
            records::values(cx, &[q, r])
        }
    }
}

macro_rules! division {
    ($($f:ident $name:literal $division:ident $part:ident),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            integer_division(cx, $name, args, Division::$division, Part::$part)
        }
    )*};
}

division!(
    quotient "quotient" Truncate Quotient,
    remainder "remainder" Truncate Remainder,
    modulo "modulo" Floor Remainder,
    floor_quotient "floor-quotient" Floor Quotient,
    floor_remainder "floor-remainder" Floor Remainder,
    floor_divide "floor/" Floor Both,
    truncate_quotient "truncate-quotient" Truncate Quotient,
    truncate_remainder "truncate-remainder" Truncate Remainder,
    truncate_divide "truncate/" Truncate Both,
);

/// `gcd` or `lcm` of any number of integers.
fn gcd_lcm<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    start: i64,
    f: fn(&Number, &Number) -> arith::Result<Number>,
) -> Result<Value, Error> {
    let mut acc = Number::from(start);
    for &v in args {
        let n = cx.number_arg(name, v)?;
        acc = f(&acc, &n).map_err(|e| arith_error(cx, name, e, &[v]))?;
    }
    Ok(cx.number_raw(&acc)?)
}

fn gcd<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    gcd_lcm(cx, "gcd", args, 0, arith::gcd)
}

fn lcm<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    gcd_lcm(cx, "lcm", args, 1, arith::lcm)
}

/// A predicate on any value: false for what is not a number.
fn is_kind<S: Strategy>(cx: &Cx<S>, v: Value, f: impl FnOnce(&Number) -> bool) -> Value {
    Value::boolean(cx.number(v).is_some_and(|n| f(&n)))
}

fn is_number<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(is_kind(cx, args[0], |_| true))
}
fn is_real<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(is_kind(cx, args[0], Number::is_real))
}
fn is_rational<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(is_kind(cx, args[0], Number::is_rational))
}
fn is_integer<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(is_kind(cx, args[0], Number::is_integer))
}
fn is_exact_integer<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(is_kind(cx, args[0], |n| matches!(n, Number::Integer(_))))
}

/// A predicate on numbers: an error for anything else.
fn number_test<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    v: Value,
    f: impl FnOnce(&Number) -> bool,
) -> Result<Value, Error> {
    Ok(Value::boolean(f(&cx.number_arg(name, v)?)))
}

fn is_exact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "exact?", args[0], Number::is_exact)
}
fn is_inexact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "inexact?", args[0], |n| !n.is_exact())
}
fn is_zero<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "zero?", args[0], arith::is_zero)
}
fn is_nan<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "nan?", args[0], |n| arith::classify(n, Class::Nan))
}
fn is_infinite<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "infinite?", args[0], |n| {
        arith::classify(n, Class::Infinite)
    })
}
fn is_finite<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    number_test(cx, "finite?", args[0], |n| {
        arith::classify(n, Class::Finite)
    })
}

/// Whether a real's sign is `want`; a NaN has none.
fn has_sign<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    v: Value,
    want: Ordering,
) -> Result<Value, Error> {
    let n = cx.number_arg(name, v)?;
    let sign = arith::sign(&n).map_err(|e| arith_error(cx, name, e, &[v]))?;
    Ok(Value::boolean(sign == Some(want)))
}

fn is_positive<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    has_sign(cx, "positive?", args[0], Ordering::Greater)
}
fn is_negative<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    has_sign(cx, "negative?", args[0], Ordering::Less)
}

/// Whether an integer is odd.
fn parity<S: Strategy>(cx: &mut Cx<S>, name: &str, v: Value) -> Result<bool, Error> {
    if let Some(n) = v.as_fixnum() {
        return Ok(n % 2 != 0);
    }
    let n = cx.number_arg(name, v)?;
    let (_, r) = arith::divide(&n, &Number::from(2), Division::Truncate)
        .map_err(|e| arith_error(cx, name, e, &[v]))?;
    Ok(!arith::is_zero(&r))
}

fn is_odd<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(parity(cx, "odd?", args[0])?))
}
fn is_even<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(!parity(cx, "even?", args[0])?))
}

fn abs<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if let Some(n) = args[0].as_fixnum()
        && let Some(v) = n.checked_abs().and_then(Value::fixnum)
    {
        return Ok(v);
    }
    unary_checked(cx, "abs", args[0], arith::abs)
}

/// `min` and `max`: if any argument is inexact, so is the result.
fn extremum<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    greater: bool,
) -> Result<Value, Error> {
    let mut best = args[0];
    let mut inexact = false;
    for &x in args {
        let n = cx.number_arg(name, x)?;
        if !n.is_real() {
            return Err(wrong_type(cx, name, "a real number", x));
        }
        inexact |= !n.is_exact();
        let better = if greater {
            cx.compare(Compare::Lt, best, x)?
        } else {
            cx.compare(Compare::Lt, x, best)?
        };
        // A NaN wins: it compares false, so it is caught here.
        if better || arith::classify(&n, Class::Nan) {
            best = x;
        }
    }
    if inexact {
        let n = cx.number(best).expect("checked");
        return Ok(cx.number_raw(&arith::to_inexact(&n))?);
    }
    Ok(best)
}

fn min<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extremum(cx, "min", args, false)
}
fn max<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    extremum(cx, "max", args, true)
}

macro_rules! rounding {
    ($($f:ident $name:literal $rounding:ident),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            if args[0].as_fixnum().is_some() {
                return Ok(args[0]);
            }
            unary_checked(cx, $name, args[0], |n| arith::round_real(n, Rounding::$rounding))
        }
    )*};
}

rounding!(
    floor "floor" Floor,
    ceiling "ceiling" Ceiling,
    truncate "truncate" Truncate,
    round "round" Round,
);

fn numerator<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary_checked(cx, "numerator", args[0], arith::numerator)
}
fn denominator<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary_checked(cx, "denominator", args[0], arith::denominator)
}

fn exact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary_checked(cx, "exact", args[0], arith::to_exact)
}
fn inexact<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "inexact", args[0], arith::to_inexact)
}

fn exact_integer_sqrt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = cx.number_arg("exact-integer-sqrt", args[0])?;
    let (s, r) = arith::exact_integer_sqrt(&n)
        .map_err(|e| arith_error(cx, "exact-integer-sqrt", e, args))?;
    let (s, r) = (cx.number_raw(&s)?, cx.number_raw(&r)?);
    records::values(cx, &[s, r])
}

fn sqrt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "sqrt", args[0], arith::sqrt)
}

fn expt<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    binary(cx, "expt", args[0], args[1], arith::expt)
}

fn exp<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "exp", args[0], arith::exp)
}

/// `(log z)` or `(log z base)`.
fn log<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if absent(args, 1) {
        unary(cx, "log", args[0], arith::log)
    } else {
        binary(cx, "log", args[0], args[1], arith::log_base)
    }
}

macro_rules! trigonometry {
    ($($f:ident $name:literal $trig:ident),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            unary(cx, $name, args[0], |z| arith::trig(Trig::$trig, z))
        }
    )*};
}

trigonometry!(
    sin "sin" Sin,
    cos "cos" Cos,
    tan "tan" Tan,
    asin "asin" Asin,
    acos "acos" Acos,
);

/// `(atan z)` or `(atan y x)`.
fn atan<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if absent(args, 1) {
        unary(cx, "atan", args[0], |z| arith::trig(Trig::Atan, z))
    } else {
        binary(cx, "atan", args[0], args[1], arith::atan2)
    }
}

fn make_rectangular<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    binary(
        cx,
        "make-rectangular",
        args[0],
        args[1],
        arith::make_rectangular,
    )
}
fn make_polar<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    binary(cx, "make-polar", args[0], args[1], arith::make_polar)
}
fn real_part<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "real-part", args[0], arith::real_part)
}
fn imag_part<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "imag-part", args[0], arith::imag_part)
}
fn magnitude<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "magnitude", args[0], arith::magnitude)
}
fn angle<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    unary(cx, "angle", args[0], arith::angle)
}

fn radix_arg(name: &str, args: &[Value]) -> Result<Radix, Error> {
    if absent(args, 1) {
        return Ok(Radix::Decimal);
    }
    match args[1].as_fixnum() {
        Some(2) => Ok(Radix::Binary),
        Some(8) => Ok(Radix::Octal),
        Some(10) => Ok(Radix::Decimal),
        Some(16) => Ok(Radix::Hexadecimal),
        _ => Err(type_error(name, "a radix of 2, 8, 10 or 16", args[1])),
    }
}

fn number_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let radix = radix_arg("number->string", args)?;
    let number = cx.number_arg("number->string", args[0])?;
    let text = number
        .to_string_radix(radix)
        .ok_or_else(|| Error::scheme("number->string: inexact numbers print only in radix 10"))?;
    Ok(cx.string_raw(&text, false)?)
}

/// `string->number`: `#f` for text that is not a number.
fn string_to_number<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let radix = radix_arg("string->number", args)?;
    if args[0].tag() != Tag::String {
        return Err(wrong_type(cx, "string->number", "a string", args[0]));
    }
    // SAFETY: a live string.
    let text = unsafe { cx.string_str(args[0]) }.to_owned();
    match vali_num::parse(&text, radix) {
        Ok(n) => Ok(cx.number_raw(&n)?),
        Err(_) => Ok(Value::FALSE),
    }
}

fn exactly(n: u8) -> Arity {
    Arity::exactly(n)
}

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

const fn one_or_two() -> Arity {
    Arity {
        required: 1,
        optional: 1,
        rest: false,
    }
}

/// The numeric procedures implemented in Rust.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    vec![
        ("+", at_least(0), add),
        ("*", at_least(0), mul),
        ("-", at_least(1), sub),
        ("/", at_least(1), div),
        ("=", at_least(1), num_eq),
        ("<", at_least(1), lt),
        ("<=", at_least(1), le),
        (">", at_least(1), gt),
        (">=", at_least(1), ge),
        ("quotient", exactly(2), quotient),
        ("remainder", exactly(2), remainder),
        ("modulo", exactly(2), modulo),
        ("floor-quotient", exactly(2), floor_quotient),
        ("floor-remainder", exactly(2), floor_remainder),
        ("floor/", exactly(2), floor_divide),
        ("truncate-quotient", exactly(2), truncate_quotient),
        ("truncate-remainder", exactly(2), truncate_remainder),
        ("truncate/", exactly(2), truncate_divide),
        ("gcd", at_least(0), gcd),
        ("lcm", at_least(0), lcm),
        ("number?", exactly(1), is_number),
        ("complex?", exactly(1), is_number),
        ("real?", exactly(1), is_real),
        ("rational?", exactly(1), is_rational),
        ("integer?", exactly(1), is_integer),
        ("exact-integer?", exactly(1), is_exact_integer),
        ("exact?", exactly(1), is_exact),
        ("inexact?", exactly(1), is_inexact),
        ("zero?", exactly(1), is_zero),
        ("positive?", exactly(1), is_positive),
        ("negative?", exactly(1), is_negative),
        ("odd?", exactly(1), is_odd),
        ("even?", exactly(1), is_even),
        ("nan?", exactly(1), is_nan),
        ("infinite?", exactly(1), is_infinite),
        ("finite?", exactly(1), is_finite),
        ("abs", exactly(1), abs),
        ("min", at_least(1), min),
        ("max", at_least(1), max),
        ("floor", exactly(1), floor),
        ("ceiling", exactly(1), ceiling),
        ("truncate", exactly(1), truncate),
        ("round", exactly(1), round),
        ("numerator", exactly(1), numerator),
        ("denominator", exactly(1), denominator),
        ("exact", exactly(1), exact),
        ("inexact", exactly(1), inexact),
        ("exact->inexact", exactly(1), inexact),
        ("inexact->exact", exactly(1), exact),
        ("exact-integer-sqrt", exactly(1), exact_integer_sqrt),
        ("sqrt", exactly(1), sqrt),
        ("expt", exactly(2), expt),
        ("exp", exactly(1), exp),
        ("log", one_or_two(), log),
        ("sin", exactly(1), sin),
        ("cos", exactly(1), cos),
        ("tan", exactly(1), tan),
        ("asin", exactly(1), asin),
        ("acos", exactly(1), acos),
        ("atan", one_or_two(), atan),
        ("make-rectangular", exactly(2), make_rectangular),
        ("make-polar", exactly(2), make_polar),
        ("real-part", exactly(1), real_part),
        ("imag-part", exactly(1), imag_part),
        ("magnitude", exactly(1), magnitude),
        ("angle", exactly(1), angle),
        ("number->string", one_or_two(), number_to_string),
        ("string->number", one_or_two(), string_to_number),
    ]
}
