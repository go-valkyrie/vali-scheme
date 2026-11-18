//! Arithmetic on the numeric tower (R7RS 6.2.6): the generic operations a
//! runtime falls back to once its fast paths for small integers and
//! flonums don't apply.
//!
//! Exactness follows R7RS. Exact arguments give an exact result whenever
//! one exists, and an inexact argument makes the result inexact. An exact
//! number and a flonum compare exactly, so comparisons stay transitive.
//! Complex results come only from complex arguments, or from operations
//! such as `sqrt` and `log` of a negative real; `(sqrt -4)` is exactly
//! `+2i`.

use std::cmp::Ordering;
use std::f64::consts::{FRAC_PI_2, PI};

use dashu_int::ops::{BitTest, DivRem, Gcd, SquareRootRem, UnsignedAbs};
use dashu_int::{IBig, UBig};
use dashu_ratio::RBig;

use crate::complex::{Complex, Repr};
use crate::rational::{exact_from_f64, from_rbig};
use crate::{Integer, Number};

/// Why an operation has no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithError {
    /// An exact number divided by exact zero.
    DivisionByZero,
    /// An argument outside the operation's domain: what it needs, such as
    /// "a real number".
    Domain(&'static str),
    /// `exact` of an infinity or a NaN, which have no exact value.
    NoExactValue,
}

impl std::fmt::Display for ArithError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArithError::DivisionByZero => f.write_str("division by zero"),
            ArithError::Domain(expected) => write!(f, "expected {expected}"),
            ArithError::NoExactValue => f.write_str("no exact value"),
        }
    }
}

impl std::error::Error for ArithError {}

/// The result of an operation that can fail.
pub type Result<T> = std::result::Result<T, ArithError>;

/// A real number, exact or inexact.
#[derive(Clone, Debug)]
enum Real {
    Exact(RBig),
    Inexact(f64),
}

impl Real {
    fn of(n: &Number) -> Option<Real> {
        match n {
            Number::Integer(i) => Some(Real::Exact(RBig::from(i.0.clone()))),
            Number::Rational(r) => Some(Real::Exact(r.0.clone())),
            Number::Real(x) => Some(Real::Inexact(*x)),
            Number::Complex(_) => None,
        }
    }

    fn to_f64(&self) -> f64 {
        match self {
            Real::Exact(r) => r.to_f64().value(),
            Real::Inexact(x) => *x,
        }
    }

    fn into_number(self) -> Number {
        match self {
            Real::Exact(r) => from_rbig(r),
            Real::Inexact(x) => Number::Real(x),
        }
    }
}

fn real(n: &Number) -> Result<Real> {
    Real::of(n).ok_or(ArithError::Domain("a real number"))
}

/// Two reals, both exact or both made inexact.
enum RealPair {
    Exact(RBig, RBig),
    Inexact(f64, f64),
}

fn real_pair(x: Real, y: Real) -> RealPair {
    match (x, y) {
        (Real::Exact(x), Real::Exact(y)) => RealPair::Exact(x, y),
        (x, y) => RealPair::Inexact(x.to_f64(), y.to_f64()),
    }
}

/// A number's real and imaginary parts, both exact or both inexact.
enum Parts {
    Exact(RBig, RBig),
    Inexact(f64, f64),
}

fn parts(n: &Number) -> Parts {
    match n {
        Number::Complex(c) => match &c.0 {
            Repr::Exact { re, im } => Parts::Exact(re.clone(), im.clone()),
            Repr::Inexact { re, im } => Parts::Inexact(*re, *im),
        },
        Number::Real(x) => Parts::Inexact(*x, 0.0),
        Number::Integer(i) => Parts::Exact(RBig::from(i.0.clone()), RBig::ZERO),
        Number::Rational(r) => Parts::Exact(r.0.clone(), RBig::ZERO),
    }
}

impl Parts {
    fn inexact(self) -> C {
        match self {
            Parts::Exact(re, im) => C::new(re.to_f64().value(), im.to_f64().value()),
            Parts::Inexact(re, im) => C::new(re, im),
        }
    }
}

/// Two numbers' parts, all exact or all made inexact.
enum ComplexPair {
    Exact((RBig, RBig), (RBig, RBig)),
    Inexact(C, C),
}

fn complex_pair(a: &Number, b: &Number) -> ComplexPair {
    match (parts(a), parts(b)) {
        (Parts::Exact(a, b), Parts::Exact(c, d)) => ComplexPair::Exact((a, b), (c, d)),
        (x, y) => ComplexPair::Inexact(x.inexact(), y.inexact()),
    }
}

fn exact_zero(n: &Number) -> bool {
    matches!(n, Number::Integer(i) if i.is_zero())
}

fn exact_one(n: &Number) -> bool {
    matches!(n, Number::Integer(i) if i.0 == IBig::ONE)
}

fn exact(r: RBig) -> Number {
    from_rbig(r)
}

/// `a + b`.
pub fn add(a: &Number, b: &Number) -> Number {
    if let (Some(x), Some(y)) = (Real::of(a), Real::of(b)) {
        return match real_pair(x, y) {
            RealPair::Exact(x, y) => exact(x + y),
            RealPair::Inexact(x, y) => Number::Real(x + y),
        };
    }
    match complex_pair(a, b) {
        ComplexPair::Exact((a, b), (c, d)) => Complex::exact(a + c, b + d),
        ComplexPair::Inexact(z, w) => z.add(w).number(),
    }
}

/// `a - b`.
pub fn sub(a: &Number, b: &Number) -> Number {
    if let (Some(x), Some(y)) = (Real::of(a), Real::of(b)) {
        return match real_pair(x, y) {
            RealPair::Exact(x, y) => exact(x - y),
            RealPair::Inexact(x, y) => Number::Real(x - y),
        };
    }
    match complex_pair(a, b) {
        ComplexPair::Exact((a, b), (c, d)) => Complex::exact(a - c, b - d),
        ComplexPair::Inexact(z, w) => z.sub(w).number(),
    }
}

/// `a × b`.
pub fn mul(a: &Number, b: &Number) -> Number {
    if let (Some(x), Some(y)) = (Real::of(a), Real::of(b)) {
        return match real_pair(x, y) {
            RealPair::Exact(x, y) => exact(x * y),
            RealPair::Inexact(x, y) => Number::Real(x * y),
        };
    }
    match complex_pair(a, b) {
        ComplexPair::Exact((a, b), (c, d)) => Complex::exact(&a * &c - &b * &d, &a * &d + &b * &c),
        ComplexPair::Inexact(z, w) => z.mul(w).number(),
    }
}

/// `a / b`. Dividing by exact zero is an error; by an inexact zero, it
/// gives an infinity or a NaN.
pub fn div(a: &Number, b: &Number) -> Result<Number> {
    if exact_zero(b) {
        return Err(ArithError::DivisionByZero);
    }
    if let (Some(x), Some(y)) = (Real::of(a), Real::of(b)) {
        return Ok(match real_pair(x, y) {
            RealPair::Exact(x, y) => exact(x / y),
            RealPair::Inexact(x, y) => Number::Real(x / y),
        });
    }
    Ok(match complex_pair(a, b) {
        ComplexPair::Exact((a, b), (c, d)) => {
            let denominator = &c * &c + &d * &d;
            Complex::exact(
                (&a * &c + &b * &d) / &denominator,
                (&b * &c - &a * &d) / &denominator,
            )
        }
        ComplexPair::Inexact(z, w) => z.div(w).number(),
    })
}

/// `-n`.
pub fn negate(n: &Number) -> Number {
    match Real::of(n) {
        Some(Real::Exact(r)) => exact(-r),
        Some(Real::Inexact(x)) => Number::Real(-x),
        None => match parts(n) {
            Parts::Exact(re, im) => Complex::exact(-re, -im),
            Parts::Inexact(re, im) => Complex::inexact(-re, -im),
        },
    }
}

/// How two reals order: `None` if either is a NaN.
pub fn compare(a: &Number, b: &Number) -> Result<Option<Ordering>> {
    Ok(compare_reals(&real(a)?, &real(b)?))
}

fn compare_reals(x: &Real, y: &Real) -> Option<Ordering> {
    match (x, y) {
        (Real::Exact(x), Real::Exact(y)) => Some(x.cmp(y)),
        (Real::Inexact(x), Real::Inexact(y)) => x.partial_cmp(y),
        (Real::Exact(x), Real::Inexact(y)) => compare_exact_inexact(x, *y),
        (Real::Inexact(x), Real::Exact(y)) => compare_exact_inexact(y, *x).map(Ordering::reverse),
    }
}

/// An exact real against a flonum, exactly.
fn compare_exact_inexact(x: &RBig, y: f64) -> Option<Ordering> {
    if y.is_nan() {
        None
    } else if y == f64::INFINITY {
        Some(Ordering::Less)
    } else if y == f64::NEG_INFINITY {
        Some(Ordering::Greater)
    } else {
        Some(x.cmp(&exact_from_f64(y)?))
    }
}

/// `=`: whether two numbers, complex or not, have the same value.
pub fn num_eq(a: &Number, b: &Number) -> bool {
    let same = |x: Real, y: Real| compare_reals(&x, &y) == Some(Ordering::Equal);
    let split = |p: Parts| match p {
        Parts::Exact(re, im) => (Real::Exact(re), Real::Exact(im)),
        Parts::Inexact(re, im) => (Real::Inexact(re), Real::Inexact(im)),
    };
    let ((a, b), (c, d)) = (split(parts(a)), split(parts(b)));
    same(a, c) && same(b, d)
}

/// Whether `n` is zero.
pub fn is_zero(n: &Number) -> bool {
    match parts(n) {
        Parts::Exact(re, im) => re.is_zero() && im.is_zero(),
        Parts::Inexact(re, im) => re == 0.0 && im == 0.0,
    }
}

/// The sign of a real: `None` for a NaN.
pub fn sign(n: &Number) -> Result<Option<Ordering>> {
    Ok(compare_reals(&real(n)?, &Real::Exact(RBig::ZERO)))
}

/// `abs` of a real.
pub fn abs(n: &Number) -> Result<Number> {
    Ok(match real(n)? {
        Real::Exact(r) if r < RBig::ZERO => exact(-r),
        Real::Inexact(x) => Number::Real(x.abs()),
        r => r.into_number(),
    })
}

/// `exact`: the exact number nearest `n`, which is exactly `n` for any
/// finite flonum.
pub fn to_exact(n: &Number) -> Result<Number> {
    let exact_f64 = |x: f64| exact_from_f64(x).ok_or(ArithError::NoExactValue);
    Ok(match n {
        Number::Real(x) => exact(exact_f64(*x)?),
        Number::Complex(c) => match &c.0 {
            Repr::Inexact { re, im } => Complex::exact(exact_f64(*re)?, exact_f64(*im)?),
            Repr::Exact { .. } => n.clone(),
        },
        _ => n.clone(),
    })
}

/// `inexact`: the flonum nearest `n`, or the complex number whose parts
/// are.
pub fn to_inexact(n: &Number) -> Number {
    match n {
        Number::Integer(i) => Number::Real(i.to_f64()),
        Number::Rational(r) => Number::Real(r.to_f64()),
        Number::Real(_) => n.clone(),
        Number::Complex(_) => parts(n).inexact().number(),
    }
}

/// How `round_real` rounds to an integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceiling,
    /// Toward zero.
    Truncate,
    /// To the nearest integer, ties to even.
    Round,
}

/// `floor`, `ceiling`, `truncate` or `round` of a real: exact for an exact
/// argument, a whole flonum for an inexact one.
pub fn round_real(n: &Number, rounding: Rounding) -> Result<Number> {
    Ok(match real(n)? {
        Real::Exact(r) => Number::Integer(Integer(match rounding {
            Rounding::Floor => r.floor(),
            Rounding::Ceiling => r.ceil(),
            Rounding::Truncate => r.trunc(),
            Rounding::Round => round_even(&r),
        })),
        Real::Inexact(x) => Number::Real(match rounding {
            Rounding::Floor => x.floor(),
            Rounding::Ceiling => x.ceil(),
            Rounding::Truncate => x.trunc(),
            Rounding::Round => x.round_ties_even(),
        }),
    })
}

fn round_even(r: &RBig) -> IBig {
    let floor = r.floor();
    let rest = r - RBig::from(floor.clone());
    let half = RBig::from_parts(IBig::ONE, UBig::from(2u8));
    match rest.cmp(&half) {
        Ordering::Less => floor,
        Ordering::Greater => floor + IBig::ONE,
        Ordering::Equal if (&floor % IBig::from(2)).is_zero() => floor,
        Ordering::Equal => floor + IBig::ONE,
    }
}

/// An integer argument: exact, or a whole flonum.
enum Int {
    Exact(IBig),
    Inexact(f64),
}

fn integer(n: &Number) -> Result<Int> {
    match n {
        Number::Integer(i) => Ok(Int::Exact(i.0.clone())),
        Number::Real(x) if x.is_finite() && x.fract() == 0.0 => Ok(Int::Inexact(*x)),
        _ => Err(ArithError::Domain("an integer")),
    }
}

fn whole_to_ibig(x: f64) -> IBig {
    let (numerator, _) = exact_from_f64(x).expect("a finite flonum").into_parts();
    numerator
}

/// Two integers, both exact or both made inexact.
enum IntPair {
    Exact(IBig, IBig),
    Inexact(f64, f64),
}

fn int_pair(a: &Number, b: &Number) -> Result<IntPair> {
    Ok(match (integer(a)?, integer(b)?) {
        (Int::Exact(x), Int::Exact(y)) => IntPair::Exact(x, y),
        (x, y) => {
            let f = |i: Int| match i {
                Int::Exact(i) => i.to_f64().value(),
                Int::Inexact(x) => x,
            };
            IntPair::Inexact(f(x), f(y))
        }
    })
}

/// Which way integer division rounds its quotient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Division {
    /// `floor/`: the remainder has the divisor's sign.
    Floor,
    /// `truncate/`: the remainder has the dividend's sign.
    Truncate,
}

/// `floor/` or `truncate/`: the quotient and remainder of two integers.
pub fn divide(n: &Number, d: &Number, division: Division) -> Result<(Number, Number)> {
    match int_pair(n, d)? {
        IntPair::Exact(n, d) => {
            if d.is_zero() {
                return Err(ArithError::DivisionByZero);
            }
            let (mut q, mut r) = (&n).div_rem(&d);
            if division == Division::Floor && !r.is_zero() && (r < IBig::ZERO) != (d < IBig::ZERO) {
                q -= IBig::ONE;
                r += &d;
            }
            Ok((Number::Integer(Integer(q)), Number::Integer(Integer(r))))
        }
        IntPair::Inexact(n, d) => {
            if d == 0.0 {
                return Err(ArithError::DivisionByZero);
            }
            let mut r = n % d;
            if division == Division::Floor && r != 0.0 && (r < 0.0) != (d < 0.0) {
                r += d;
            }
            Ok((Number::Real(((n - r) / d).round()), Number::Real(r)))
        }
    }
}

/// `gcd` of two integers: never negative, and 0 for two zeros.
pub fn gcd(a: &Number, b: &Number) -> Result<Number> {
    // dashu panics on two zeros, and zero is the identity anyway.
    let gcd = |x: IBig, y: IBig| -> IBig {
        if x.is_zero() {
            return IBig::from(y.unsigned_abs());
        }
        if y.is_zero() {
            return IBig::from(x.unsigned_abs());
        }
        IBig::from((&x).gcd(&y))
    };
    Ok(match int_pair(a, b)? {
        IntPair::Exact(x, y) => Number::Integer(Integer(gcd(x, y))),
        IntPair::Inexact(x, y) => {
            Number::Real(gcd(whole_to_ibig(x), whole_to_ibig(y)).to_f64().value())
        }
    })
}

/// `lcm` of two integers: never negative.
pub fn lcm(a: &Number, b: &Number) -> Result<Number> {
    let lcm = |x: IBig, y: IBig| -> IBig {
        if x.is_zero() || y.is_zero() {
            return IBig::ZERO;
        }
        let g = IBig::from((&x).gcd(&y));
        IBig::from((&x * &y).unsigned_abs()) / g
    };
    Ok(match int_pair(a, b)? {
        IntPair::Exact(x, y) => Number::Integer(Integer(lcm(x, y))),
        IntPair::Inexact(x, y) => {
            Number::Real(lcm(whole_to_ibig(x), whole_to_ibig(y)).to_f64().value())
        }
    })
}

/// The exact value of a rational argument, and whether it was inexact.
fn rational_value(n: &Number) -> Result<(RBig, bool)> {
    match n {
        Number::Integer(i) => Ok((RBig::from(i.0.clone()), false)),
        Number::Rational(r) => Ok((r.0.clone(), false)),
        Number::Real(x) => exact_from_f64(*x)
            .map(|r| (r, true))
            .ok_or(ArithError::Domain("a finite number")),
        Number::Complex(_) => Err(ArithError::Domain("a rational number")),
    }
}

/// `numerator`, of the number in lowest terms.
pub fn numerator(n: &Number) -> Result<Number> {
    let (r, inexact) = rational_value(n)?;
    let numerator = Number::Integer(Integer(r.numerator().clone()));
    Ok(if inexact {
        to_inexact(&numerator)
    } else {
        numerator
    })
}

/// `denominator`, of the number in lowest terms: always positive.
pub fn denominator(n: &Number) -> Result<Number> {
    let (r, inexact) = rational_value(n)?;
    let denominator = Number::Integer(Integer(IBig::from(r.denominator().clone())));
    Ok(if inexact {
        to_inexact(&denominator)
    } else {
        denominator
    })
}

/// `exact-integer-sqrt`: `s` and `r` with `n = s² + r` and `n < (s + 1)²`.
pub fn exact_integer_sqrt(n: &Number) -> Result<(Number, Number)> {
    match n {
        Number::Integer(i) if !i.is_negative() => {
            let (s, r) = (&i.0).unsigned_abs().sqrt_rem();
            Ok((
                Number::Integer(Integer(IBig::from(s))),
                Number::Integer(Integer(IBig::from(r))),
            ))
        }
        _ => Err(ArithError::Domain("an exact non-negative integer")),
    }
}

/// The exact square root of a non-negative exact rational, if it has one.
fn rational_sqrt(r: &RBig) -> Option<RBig> {
    if *r < RBig::ZERO {
        return None;
    }
    let (n, nr) = (r.numerator()).unsigned_abs().sqrt_rem();
    let (d, dr) = r.denominator().sqrt_rem();
    (nr.is_zero() && dr.is_zero()).then(|| RBig::from_parts(IBig::from(n), d))
}

/// `sqrt`: exact when the argument is exact and its root is too; the
/// principal root otherwise, complex for a negative real.
pub fn sqrt(n: &Number) -> Number {
    match n {
        Number::Integer(_) | Number::Rational(_) => {
            let Real::Exact(r) = Real::of(n).expect("real") else {
                unreachable!()
            };
            if r >= RBig::ZERO {
                rational_sqrt(&r).map_or_else(|| Number::Real(r.to_f64().value().sqrt()), exact)
            } else {
                let r = -r;
                match rational_sqrt(&r) {
                    Some(s) => Complex::exact(RBig::ZERO, s),
                    None => Complex::inexact(0.0, r.to_f64().value().sqrt()),
                }
            }
        }
        Number::Real(x) if *x < 0.0 => Complex::inexact(0.0, (-x).sqrt()),
        Number::Real(x) => Number::Real(x.sqrt()),
        Number::Complex(c) => match &c.0 {
            Repr::Exact { re, im } => {
                exact_complex_sqrt(re, im).unwrap_or_else(|| parts(n).inexact().sqrt().number())
            }
            Repr::Inexact { re, im } => C::new(*re, *im).sqrt().number(),
        },
    }
}

/// The exact principal root of `re + im·i`, if its parts are exact.
fn exact_complex_sqrt(re: &RBig, im: &RBig) -> Option<Number> {
    let magnitude = rational_sqrt(&(re * re + im * im))?;
    let two = RBig::from(IBig::from(2));
    let a = rational_sqrt(&((&magnitude + re) / &two))?;
    let b = rational_sqrt(&((&magnitude - re) / &two))?;
    let b = if *im < RBig::ZERO { -b } else { b };
    Some(Complex::exact(a, b))
}

/// `expt`: exact for an exact base and an exact integer exponent.
pub fn expt(base: &Number, exponent: &Number) -> Result<Number> {
    if let Number::Integer(k) = exponent {
        if k.is_zero() {
            return Ok(if base.is_exact() {
                Number::from(1)
            } else {
                Number::Real(1.0)
            });
        }
        if base.is_exact() {
            return exact_power(base, &k.0);
        }
        if let Some(Real::Inexact(x)) = Real::of(base) {
            return Ok(Number::Real(x.powf(k.to_f64())));
        }
        return Ok(parts(base).inexact().powi(&k.0).number());
    }
    if is_zero(base) {
        let positive = match parts(exponent) {
            Parts::Exact(re, _) => re > RBig::ZERO,
            Parts::Inexact(re, _) => re > 0.0,
        };
        return if positive {
            Ok(if base.is_exact() && exponent.is_exact() {
                Number::from(0)
            } else {
                Number::Real(0.0)
            })
        } else {
            Err(ArithError::DivisionByZero)
        };
    }
    if let (Some(b), Some(x)) = (Real::of(base), Real::of(exponent)) {
        let b = b.to_f64();
        if b >= 0.0 {
            return Ok(Number::Real(b.powf(x.to_f64())));
        }
    }
    let (b, z) = (parts(base).inexact(), parts(exponent).inexact());
    Ok(z.mul(b.ln()).exp().number())
}

/// An exact number to an exact integer power, by repeated squaring.
fn exact_power(base: &Number, k: &IBig) -> Result<Number> {
    if *k < IBig::ZERO {
        if is_zero(base) {
            return Err(ArithError::DivisionByZero);
        }
        return div(&Number::from(1), &exact_power(base, &-k)?);
    }
    let k = k.unsigned_abs();
    let mut result = Number::from(1);
    let mut square = base.clone();
    for i in 0..k.bit_len() {
        if k.bit(i) {
            result = mul(&result, &square);
        }
        if i + 1 < k.bit_len() {
            square = mul(&square, &square);
        }
    }
    Ok(result)
}

/// `exp`: exactly 1 for exact 0.
pub fn exp(z: &Number) -> Number {
    if exact_zero(z) {
        return Number::from(1);
    }
    match Real::of(z) {
        Some(x) => Number::Real(x.to_f64().exp()),
        None => parts(z).inexact().exp().number(),
    }
}

/// The natural logarithm: exactly 0 for exact 1, complex for a negative
/// real.
pub fn log(z: &Number) -> Number {
    if exact_one(z) {
        return Number::from(0);
    }
    match Real::of(z) {
        Some(x) => {
            let x = x.to_f64();
            if x < 0.0 {
                Complex::inexact((-x).ln(), PI)
            } else {
                Number::Real(x.ln())
            }
        }
        None => parts(z).inexact().ln().number(),
    }
}

/// The logarithm of `z` to `base`.
pub fn log_base(z: &Number, base: &Number) -> Result<Number> {
    div(&log(z), &log(base))
}

/// The trigonometric functions of `(scheme inexact)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trig {
    /// `sin`.
    Sin,
    /// `cos`.
    Cos,
    /// `tan`.
    Tan,
    /// `asin`.
    Asin,
    /// `acos`.
    Acos,
    /// `atan` of one argument.
    Atan,
}

/// A trigonometric function: exact where the answer is, at exact 0, and
/// complex where a real argument has no real answer, such as `(asin 2)`.
pub fn trig(f: Trig, z: &Number) -> Number {
    if exact_zero(z) {
        match f {
            Trig::Sin | Trig::Tan | Trig::Asin | Trig::Atan => return Number::from(0),
            Trig::Cos => return Number::from(1),
            Trig::Acos => {}
        }
    }
    if exact_one(z) && f == Trig::Acos {
        return Number::from(0);
    }
    if let Some(x) = Real::of(z) {
        let x = x.to_f64();
        let real = match f {
            Trig::Sin => Some(x.sin()),
            Trig::Cos => Some(x.cos()),
            Trig::Tan => Some(x.tan()),
            Trig::Atan => Some(x.atan()),
            Trig::Asin if (-1.0..=1.0).contains(&x) => Some(x.asin()),
            Trig::Acos if (-1.0..=1.0).contains(&x) => Some(x.acos()),
            Trig::Asin | Trig::Acos => None,
        };
        if let Some(y) = real {
            return Number::Real(y);
        }
    }
    let z = parts(z).inexact();
    match f {
        Trig::Sin => z.sin(),
        Trig::Cos => z.cos(),
        Trig::Tan => z.sin().div(z.cos()),
        Trig::Asin => z.asin(),
        Trig::Acos => C::new(FRAC_PI_2, 0.0).sub(z.asin()),
        Trig::Atan => z.atan(),
    }
    .number()
}

/// `atan` of two reals: the angle of the point `(x, y)`; exactly 0 for an
/// exact 0 and a positive exact `x`.
pub fn atan2(y: &Number, x: &Number) -> Result<Number> {
    let (y, x) = (real(y)?, real(x)?);
    if let (Real::Exact(y), Real::Exact(x)) = (&y, &x)
        && y.is_zero()
        && *x > RBig::ZERO
    {
        return Ok(Number::from(0));
    }
    Ok(Number::Real(y.to_f64().atan2(x.to_f64())))
}

/// `make-rectangular`: real only when the imaginary part is exact zero.
pub fn make_rectangular(re: &Number, im: &Number) -> Result<Number> {
    let (re, im) = (real(re)?, real(im)?);
    Ok(match (re, im) {
        (re, Real::Exact(im)) if im.is_zero() => re.into_number(),
        (Real::Exact(re), Real::Exact(im)) => Complex::exact(re, im),
        (re, im) => Complex::inexact(re.to_f64(), im.to_f64()),
    })
}

/// `make-polar`: real only when the angle is exact zero.
pub fn make_polar(magnitude: &Number, angle: &Number) -> Result<Number> {
    let (m, a) = (real(magnitude)?, real(angle)?);
    if matches!(&a, Real::Exact(a) if a.is_zero()) {
        return Ok(m.into_number());
    }
    let (m, a) = (m.to_f64(), a.to_f64());
    Ok(Complex::inexact(m * a.cos(), m * a.sin()))
}

/// `real-part`.
pub fn real_part(z: &Number) -> Number {
    match z {
        Number::Complex(c) => c.real_part(),
        _ => z.clone(),
    }
}

/// `imag-part`: exact zero for a real.
pub fn imag_part(z: &Number) -> Number {
    match z {
        Number::Complex(c) => c.imag_part(),
        Number::Real(_) | Number::Integer(_) | Number::Rational(_) => Number::from(0),
    }
}

/// `magnitude`: exact when the argument is exact and its magnitude is too.
pub fn magnitude(z: &Number) -> Number {
    match parts(z) {
        Parts::Exact(re, im) if im.is_zero() => exact(if re < RBig::ZERO { -re } else { re }),
        Parts::Exact(re, im) => sqrt(&exact(&re * &re + &im * &im)),
        Parts::Inexact(re, im) => Number::Real(re.hypot(im)),
    }
}

/// `angle`: exactly 0 for a non-negative exact real.
pub fn angle(z: &Number) -> Number {
    match parts(z) {
        Parts::Exact(re, im) if im.is_zero() && re >= RBig::ZERO => Number::from(0),
        Parts::Exact(re, im) => Number::Real(im.to_f64().value().atan2(re.to_f64().value())),
        Parts::Inexact(re, im) => Number::Real(im.atan2(re)),
    }
}

/// Which property of its parts `classify` tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `nan?`: either part is a NaN.
    Nan,
    /// `infinite?`: either part is an infinity.
    Infinite,
    /// `finite?`: both parts are finite.
    Finite,
}

/// `nan?`, `infinite?` and `finite?`.
pub fn classify(z: &Number, class: Class) -> bool {
    let (re, im) = match parts(z) {
        Parts::Exact(..) => return class == Class::Finite,
        Parts::Inexact(re, im) => (re, im),
    };
    match class {
        Class::Nan => re.is_nan() || im.is_nan(),
        Class::Infinite => re.is_infinite() || im.is_infinite(),
        Class::Finite => re.is_finite() && im.is_finite(),
    }
}

/// An inexact complex number, for the operations on complex arguments.
#[derive(Clone, Copy, Debug, PartialEq)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn new(re: f64, im: f64) -> C {
        C { re, im }
    }

    fn number(self) -> Number {
        Complex::inexact(self.re, self.im)
    }

    fn add(self, w: C) -> C {
        C::new(self.re + w.re, self.im + w.im)
    }

    fn sub(self, w: C) -> C {
        C::new(self.re - w.re, self.im - w.im)
    }

    fn mul(self, w: C) -> C {
        C::new(
            self.re * w.re - self.im * w.im,
            self.re * w.im + self.im * w.re,
        )
    }

    /// Smith's algorithm, which avoids overflow in the denominator.
    fn div(self, w: C) -> C {
        if w.re.abs() >= w.im.abs() {
            let r = w.im / w.re;
            let d = w.re + w.im * r;
            C::new((self.re + self.im * r) / d, (self.im - self.re * r) / d)
        } else {
            let r = w.re / w.im;
            let d = w.re * r + w.im;
            C::new((self.re * r + self.im) / d, (self.im * r - self.re) / d)
        }
    }

    fn magnitude(self) -> f64 {
        self.re.hypot(self.im)
    }

    fn exp(self) -> C {
        let m = self.re.exp();
        C::new(m * self.im.cos(), m * self.im.sin())
    }

    fn ln(self) -> C {
        C::new(self.magnitude().ln(), self.im.atan2(self.re))
    }

    fn sqrt(self) -> C {
        if self.re == 0.0 && self.im == 0.0 {
            return C::new(0.0, self.im);
        }
        let a = ((self.magnitude() + self.re.abs()) / 2.0).sqrt();
        if self.re >= 0.0 {
            C::new(a, self.im / (2.0 * a))
        } else {
            C::new(self.im.abs() / (2.0 * a), a.copysign(self.im))
        }
    }

    fn sin(self) -> C {
        C::new(
            self.re.sin() * self.im.cosh(),
            self.re.cos() * self.im.sinh(),
        )
    }

    fn cos(self) -> C {
        C::new(
            self.re.cos() * self.im.cosh(),
            -self.re.sin() * self.im.sinh(),
        )
    }

    /// `asin z = -i log(iz + sqrt(1 - z²))`.
    fn asin(self) -> C {
        let i = C::new(0.0, 1.0);
        let root = C::new(1.0, 0.0).sub(self.mul(self)).sqrt();
        let w = i.mul(self).add(root).ln();
        C::new(w.im, -w.re)
    }

    /// `atan z = (log(1 + iz) - log(1 - iz)) / 2i`.
    fn atan(self) -> C {
        let iz = C::new(-self.im, self.re);
        let one = C::new(1.0, 0.0);
        let w = one.add(iz).ln().sub(one.sub(iz).ln());
        C::new(w.im / 2.0, -w.re / 2.0)
    }

    /// `self` to an integer power, by repeated squaring.
    fn powi(self, k: &IBig) -> C {
        let negative = *k < IBig::ZERO;
        let k = k.unsigned_abs();
        let mut result = C::new(1.0, 0.0);
        let mut square = self;
        for i in 0..k.bit_len() {
            if k.bit(i) {
                result = result.mul(square);
            }
            square = square.mul(square);
        }
        if negative {
            C::new(1.0, 0.0).div(result)
        } else {
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Radix, parse};

    fn n(text: &str) -> Number {
        parse(text, Radix::Decimal).unwrap_or_else(|e| panic!("{text}: {e:?}"))
    }

    fn show(n: Number) -> String {
        n.to_string()
    }

    #[test]
    fn exactness_follows_the_arguments() {
        assert_eq!(show(add(&n("1/2"), &n("1/3"))), "5/6");
        assert_eq!(show(add(&n("1/2"), &n("1/2"))), "1");
        assert_eq!(show(add(&n("1/2"), &n("0.5"))), "1.0");
        assert_eq!(show(mul(&n("2/3"), &n("3/2"))), "1");
        assert_eq!(show(sub(&n("1"), &n("3/2"))), "-1/2");
        assert_eq!(show(div(&n("1"), &n("3")).unwrap()), "1/3");
        assert_eq!(show(div(&n("6"), &n("3")).unwrap()), "2");
        assert_eq!(show(div(&n("1.0"), &n("0.0")).unwrap()), "+inf.0");
        assert_eq!(
            div(&n("1"), &n("0")).unwrap_err(),
            ArithError::DivisionByZero
        );
        assert_eq!(
            div(&n("1.5"), &n("0")).unwrap_err(),
            ArithError::DivisionByZero
        );
        assert_eq!(show(negate(&n("-0.0"))), "0.0");
        assert_eq!(show(negate(&n("1/2"))), "-1/2");
    }

    #[test]
    fn complex_arithmetic() {
        assert_eq!(show(mul(&n("+i"), &n("+i"))), "-1");
        assert_eq!(show(add(&n("1+2i"), &n("1-2i"))), "2");
        assert_eq!(show(mul(&n("1+2i"), &n("3-i"))), "5+5i");
        assert_eq!(show(div(&n("5+5i"), &n("3-i")).unwrap()), "1+2i");
        assert_eq!(show(add(&n("1+2i"), &n("0.5"))), "1.5+2.0i");
        assert_eq!(show(mul(&n("1.0+1.0i"), &n("1.0-1.0i"))), "2.0+0.0i");
        assert!(num_eq(&n("1+2i"), &n("1.0+2.0i")));
        assert!(!num_eq(&n("1+2i"), &n("1+3i")));
        assert_eq!(show(negate(&n("1-2i"))), "-1+2i");
    }

    #[test]
    fn exact_and_inexact_compare_exactly() {
        // 2^53 + 1 is not a flonum; converting it to one would call them equal.
        let big = n("9007199254740993");
        let float = n("9007199254740992.0");
        assert_eq!(compare(&big, &float).unwrap(), Some(Ordering::Greater));
        assert_eq!(
            compare(&n("1/3"), &n("0.3333333333333333")).unwrap(),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare(&n("5"), &n("+inf.0")).unwrap(),
            Some(Ordering::Less)
        );
        assert_eq!(compare(&n("5"), &n("+nan.0")).unwrap(), None);
        assert_eq!(
            compare(&n("1"), &n("+i")).unwrap_err(),
            ArithError::Domain("a real number")
        );
        assert!(num_eq(&n("1/2"), &n("0.5")));
    }

    #[test]
    fn rounding() {
        let r = |text: &str, rounding| show(round_real(&n(text), rounding).unwrap());
        assert_eq!(r("7/2", Rounding::Floor), "3");
        assert_eq!(r("-7/2", Rounding::Floor), "-4");
        assert_eq!(r("-7/2", Rounding::Ceiling), "-3");
        assert_eq!(r("-7/2", Rounding::Truncate), "-3");
        assert_eq!(r("7/2", Rounding::Round), "4");
        assert_eq!(r("5/2", Rounding::Round), "2");
        assert_eq!(r("-5/2", Rounding::Round), "-2");
        assert_eq!(r("2.5", Rounding::Round), "2.0");
        assert_eq!(r("-3.7", Rounding::Truncate), "-3.0");
        assert_eq!(r("4", Rounding::Floor), "4");
    }

    #[test]
    fn integer_division() {
        let d = |a: &str, b: &str, division| {
            let (q, r) = divide(&n(a), &n(b), division).unwrap();
            format!("{q} {r}")
        };
        assert_eq!(d("5", "2", Division::Floor), "2 1");
        assert_eq!(d("-5", "2", Division::Floor), "-3 1");
        assert_eq!(d("5", "-2", Division::Floor), "-3 -1");
        assert_eq!(d("-5", "-2", Division::Floor), "2 -1");
        assert_eq!(d("-5", "2", Division::Truncate), "-2 -1");
        assert_eq!(d("5", "-2", Division::Truncate), "-2 1");
        assert_eq!(d("-5.0", "2", Division::Floor), "-3.0 1.0");
        assert_eq!(d("-5", "2.0", Division::Truncate), "-2.0 -1.0");
        assert_eq!(
            divide(&n("1"), &n("0"), Division::Floor).unwrap_err(),
            ArithError::DivisionByZero
        );
        assert_eq!(
            divide(&n("1/2"), &n("1"), Division::Floor).unwrap_err(),
            ArithError::Domain("an integer")
        );
        assert_eq!(show(gcd(&n("32"), &n("-36")).unwrap()), "4");
        assert_eq!(show(gcd(&n("0"), &n("5")).unwrap()), "5");
        assert_eq!(show(gcd(&n("0"), &n("0")).unwrap()), "0");
        assert_eq!(show(gcd(&n("-6"), &n("0")).unwrap()), "6");
        assert_eq!(show(gcd(&n("0.0"), &n("0.0")).unwrap()), "0.0");
        assert_eq!(show(lcm(&n("32"), &n("-36")).unwrap()), "288");
        assert_eq!(show(lcm(&n("32.0"), &n("-36")).unwrap()), "288.0");
        assert_eq!(show(lcm(&n("0"), &n("5")).unwrap()), "0");
    }

    #[test]
    fn exactness_conversions() {
        assert_eq!(show(to_exact(&n("0.5")).unwrap()), "1/2");
        assert_eq!(show(to_exact(&n("-2.0")).unwrap()), "-2");
        assert_eq!(show(to_exact(&n("0.5+1.5i")).unwrap()), "1/2+3/2i");
        assert_eq!(
            to_exact(&n("+inf.0")).unwrap_err(),
            ArithError::NoExactValue
        );
        assert_eq!(show(to_inexact(&n("1/4"))), "0.25");
        assert_eq!(show(to_inexact(&n("1+2i"))), "1.0+2.0i");
        assert_eq!(show(numerator(&n("6/4")).unwrap()), "3");
        assert_eq!(show(denominator(&n("6/4")).unwrap()), "2");
        assert_eq!(show(denominator(&n("0")).unwrap()), "1");
        assert_eq!(show(denominator(&n("0.5")).unwrap()), "2.0");
        assert_eq!(show(numerator(&n("-0.75")).unwrap()), "-3.0");
    }

    #[test]
    fn roots_and_powers() {
        assert_eq!(show(sqrt(&n("16"))), "4");
        assert_eq!(show(sqrt(&n("9/4"))), "3/2");
        assert_eq!(show(sqrt(&n("-4"))), "0+2i");
        assert_eq!(show(sqrt(&n("2"))), "1.4142135623730951");
        assert_eq!(show(sqrt(&n("-2.0"))), "0.0+1.4142135623730951i");
        assert_eq!(show(sqrt(&n("-3+4i"))), "1+2i");
        assert_eq!(show(sqrt(&n("16.0"))), "4.0");
        let (s, r) = exact_integer_sqrt(&n("17")).unwrap();
        assert_eq!((show(s), show(r)), ("4".into(), "1".into()));
        assert!(exact_integer_sqrt(&n("-1")).is_err());
        assert_eq!(
            show(expt(&n("2"), &n("100")).unwrap()),
            "1267650600228229401496703205376"
        );
        assert_eq!(show(expt(&n("2/3"), &n("3")).unwrap()), "8/27");
        assert_eq!(show(expt(&n("2"), &n("-2")).unwrap()), "1/4");
        assert_eq!(show(expt(&n("+i"), &n("2")).unwrap()), "-1");
        assert_eq!(show(expt(&n("2.0"), &n("3")).unwrap()), "8.0");
        assert_eq!(show(expt(&n("4"), &n("0.5")).unwrap()), "2.0");
        assert_eq!(show(expt(&n("7"), &n("0")).unwrap()), "1");
        assert_eq!(show(expt(&n("0"), &n("2")).unwrap()), "0");
        assert_eq!(
            expt(&n("0"), &n("-1")).unwrap_err(),
            ArithError::DivisionByZero
        );
        // (-8)^(1/3) is the principal cube root, 1+1.732…i.
        let Number::Complex(c) = expt(&n("-8"), &n("1/3")).unwrap() else {
            panic!("a complex root")
        };
        let (Number::Real(re), Number::Real(im)) = (c.real_part(), c.imag_part()) else {
            panic!("inexact parts")
        };
        assert!((re - 1.0).abs() < 1e-12 && (im - 3f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn transcendental_functions() {
        assert_eq!(show(exp(&n("0"))), "1");
        assert_eq!(show(log(&n("1"))), "0");
        assert_eq!(show(exp(&n("1"))), "2.718281828459045");
        assert_eq!(show(log_base(&n("8"), &n("2")).unwrap()), "3.0");
        assert_eq!(show(log(&n("-1"))), "0.0+3.141592653589793i");
        assert_eq!(show(trig(Trig::Sin, &n("0"))), "0");
        assert_eq!(show(trig(Trig::Cos, &n("0"))), "1");
        assert_eq!(show(trig(Trig::Acos, &n("1"))), "0");
        assert_eq!(show(trig(Trig::Atan, &n("1.0"))), "0.7853981633974483");
        assert!(matches!(trig(Trig::Asin, &n("2")), Number::Complex(_)));
        assert_eq!(show(atan2(&n("0"), &n("1")).unwrap()), "0");
        assert_eq!(show(atan2(&n("1"), &n("0")).unwrap()), "1.5707963267948966");
        // e^(iπ) is -1, within rounding.
        let Number::Complex(c) = exp(&mul(&n("+i"), &n("3.141592653589793"))) else {
            panic!("complex")
        };
        let Number::Real(re) = c.real_part() else {
            panic!()
        };
        assert!((re + 1.0).abs() < 1e-15);
    }

    #[test]
    fn complex_construction() {
        assert_eq!(show(make_rectangular(&n("1"), &n("2")).unwrap()), "1+2i");
        assert_eq!(show(make_rectangular(&n("1.5"), &n("0")).unwrap()), "1.5");
        assert_eq!(
            show(make_rectangular(&n("1"), &n("0.0")).unwrap()),
            "1.0+0.0i"
        );
        assert_eq!(show(make_polar(&n("2"), &n("0")).unwrap()), "2");
        assert_eq!(show(real_part(&n("3-4i"))), "3");
        assert_eq!(show(imag_part(&n("3-4i"))), "-4");
        assert_eq!(show(imag_part(&n("2.5"))), "0");
        assert_eq!(show(magnitude(&n("3-4i"))), "5");
        assert_eq!(show(magnitude(&n("-5/2"))), "5/2");
        assert_eq!(show(magnitude(&n("1+i"))), "1.4142135623730951");
        assert_eq!(show(angle(&n("1"))), "0");
        assert_eq!(show(angle(&n("-1"))), "3.141592653589793");
        assert!(classify(&n("+nan.0"), Class::Nan));
        assert!(classify(&n("1+inf.0i"), Class::Infinite));
        assert!(classify(&n("1/2"), Class::Finite));
        assert!(!classify(&n("1.0+nan.0i"), Class::Finite));
    }
}
