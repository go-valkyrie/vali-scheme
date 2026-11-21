//! Flonum functions for SRFI 144: the C99 math library's operations on
//! `f64`, as IEEE 754 and C99 define them for `-0.0`, infinities and NaNs.
//!
//! Where Rust's standard library defers to the platform's C library (so
//! the last bit can differ between systems), the functions here use the
//! `libm` crate, which gives the same results everywhere. Plain IEEE
//! operations (`+`, `sqrt`, `floor`) come from the language.

use crate::Integer;
use crate::arith::{ArithError, Result};

/// What `ilogb` returns for zero: C99's `FP_ILOGB0`.
pub const INTEGER_EXPONENT_ZERO: i32 = i32::MIN;
/// What `ilogb` returns for a NaN: C99's `FP_ILOGBNAN`.
pub const INTEGER_EXPONENT_NAN: i32 = i32::MIN;

macro_rules! libm_unary {
    ($($(#[$doc:meta])* $name:ident = $f:path;)*) => {$(
        $(#[$doc])*
        pub fn $name(x: f64) -> f64 {
            $f(x)
        }
    )*};
}

libm_unary! {
    /// 2 to the `x`.
    exp2 = libm::exp2;
    /// e to the `x`, minus 1, accurately for small `x`.
    expm1 = libm::expm1;
    /// The natural logarithm of `x` plus 1, accurately for small `x`.
    log1p = libm::log1p;
    /// The base-2 logarithm.
    log2 = libm::log2;
    /// The base-10 logarithm.
    log10 = libm::log10;
    /// The cube root.
    cbrt = libm::cbrt;
    /// Hyperbolic sine.
    sinh = libm::sinh;
    /// Hyperbolic cosine.
    cosh = libm::cosh;
    /// Hyperbolic tangent.
    tanh = libm::tanh;
    /// Inverse hyperbolic sine.
    asinh = libm::asinh;
    /// Inverse hyperbolic cosine.
    acosh = libm::acosh;
    /// Inverse hyperbolic tangent.
    atanh = libm::atanh;
    /// The error function.
    erf = libm::erf;
    /// The complementary error function.
    erfc = libm::erfc;
    /// The gamma function.
    gamma = libm::tgamma;
}

/// `sqrt(x² + y²)` without intermediate overflow.
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// `x * y + z` with a single rounding.
pub fn fma(x: f64, y: f64, z: f64) -> f64 {
    libm::fma(x, y, z)
}

/// `log |Γ(x)|` and the sign of Γ(x), as 1.0 or -1.0.
pub fn log_gamma(x: f64) -> (f64, f64) {
    let (value, sign) = libm::lgamma_r(x);
    (value, if sign < 0 { -1.0 } else { 1.0 })
}

/// The largest order `libm` computes by recurrence. Past it the answer
/// for a small argument is known without computing, and for a large one
/// the recurrence takes time in proportion to the order, so it is refused.
pub const MAX_BESSEL_ORDER: i64 = 1 << 20;

/// What a Bessel function of an order past [`MAX_BESSEL_ORDER`] is.
enum HugeOrder {
    /// The order is in range: ask `libm`.
    No,
    /// The order is past it, and `negative` and `odd` describe it.
    Yes { negative: bool, odd: bool },
}

fn huge_order(n: &Integer) -> HugeOrder {
    match n.to_i64() {
        Some(k) if k.abs() <= MAX_BESSEL_ORDER => HugeOrder::No,
        _ => HugeOrder::Yes {
            negative: n.is_negative(),
            odd: n.limbs().1.first().is_some_and(|w| w & 1 == 1),
        },
    }
}

/// Whether `x` is finite, not zero, and no more than half of the huge
/// order `n` in size: where J underflows to zero and Y overflows.
fn in_decay_region(n: &Integer, x: f64) -> bool {
    x.is_finite() && x != 0.0 && x.abs() <= n.to_f64().abs() / 2.0
}

/// The Bessel function of the first kind of order `n`.
///
/// An order past [`MAX_BESSEL_ORDER`] has `|x|` at most half of it
/// (where J_n underflows, with the sign of (-1)^n for negative `x` and
/// for negative `n`) its answer without computing; with a larger finite
/// argument the result is [`ArithError::TooLarge`]. Zero, infinite and NaN
/// arguments need no recurrence and always get an answer.
pub fn bessel_first(n: &Integer, x: f64) -> Result<f64> {
    match huge_order(n) {
        HugeOrder::No => Ok(libm::jn(n.to_i64().unwrap_or(0) as i32, x)),
        HugeOrder::Yes { negative, odd } => {
            if x.is_nan() || x == 0.0 || x.is_infinite() {
                Ok(libm::jn(stand_in(negative, odd), x))
            } else if in_decay_region(n, x) {
                // (-1)^n twice over: once for a negative order, once for a
                // negative argument.
                Ok(if odd && (x < 0.0) != negative {
                    -0.0
                } else {
                    0.0
                })
            } else {
                Err(ArithError::TooLarge)
            }
        }
    }
}

/// The Bessel function of the second kind of order `n`, with the same
/// bounds as [`bessel_first`]: in the decay region it is infinite, and a
/// negative argument is NaN.
pub fn bessel_second(n: &Integer, x: f64) -> Result<f64> {
    match huge_order(n) {
        HugeOrder::No => Ok(libm::yn(n.to_i64().unwrap_or(0) as i32, x)),
        HugeOrder::Yes { negative, odd } => {
            if x.is_nan() || x == 0.0 || x.is_infinite() {
                Ok(libm::yn(stand_in(negative, odd), x))
            } else if x < 0.0 {
                Ok(f64::NAN)
            } else if in_decay_region(n, x) {
                // Y_{-n} = (-1)^n Y_n.
                Ok(if odd && negative {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                })
            } else {
                Err(ArithError::TooLarge)
            }
        }
    }
}

/// An order of the same sign and parity as a huge one, for the arguments
/// that take `libm` no time whatever the order.
fn stand_in(negative: bool, odd: bool) -> i32 {
    let n = if odd { i32::MAX } else { i32::MAX - 1 };
    if negative { -n } else { n }
}

/// An order as an `i32`, clamped.
fn order(n: &Integer) -> i32 {
    n.to_i64()
        .map_or(if n.is_negative() { i32::MIN } else { i32::MAX }, |n| {
            n.clamp(i32::MIN as i64, i32::MAX as i64) as i32
        })
}

/// The flonum next to `x` in the direction of `y`.
pub fn adjacent(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        f64::NAN
    } else if x == y {
        x
    } else if x < y {
        x.next_up()
    } else {
        x.next_down()
    }
}

/// `x * 2^n`, `n` an exact integer of any size.
pub fn make_flonum(x: f64, n: &Integer) -> f64 {
    libm::scalbn(x, order(n))
}

/// The integral and fractional parts of `x`, both with `x`'s sign.
pub fn integer_fraction(x: f64) -> (f64, f64) {
    if x.is_infinite() {
        return (x, 0.0f64.copysign(x));
    }
    let integral = x.trunc();
    (integral, if x.is_nan() { x } else { x - integral })
}

/// `logb`: the exponent of `x` as a flonum; -infinity for zero and
/// infinity for infinities.
pub fn exponent(x: f64) -> f64 {
    if x.is_nan() {
        x
    } else if x == 0.0 {
        f64::NEG_INFINITY
    } else if x.is_infinite() {
        f64::INFINITY
    } else {
        f64::from(libm::ilogb(x))
    }
}

/// `ilogb`: [`exponent`] as an exact integer.
pub fn integer_exponent(x: f64) -> i32 {
    libm::ilogb(x)
}

/// A fraction `f` with `0.5 <= |f| < 1` and an exponent `n` with
/// `x = f * 2^n`; zeros, infinities and NaNs are their own fractions with
/// exponent 0.
pub fn normalized_fraction_exponent(x: f64) -> (f64, i32) {
    libm::frexp(x)
}

/// 1 if the sign bit of `x` is set, even for `-0.0`, else 0.
pub fn sign_bit(x: f64) -> i64 {
    i64::from(x.is_sign_negative())
}

/// Whether `x` is finite and not zero or subnormal.
pub fn is_normalized(x: f64) -> bool {
    x.is_normal()
}

/// Whether `x` is subnormal.
pub fn is_denormalized(x: f64) -> bool {
    x.classify() == std::num::FpCategory::Subnormal
}

/// The numerator of `x` as a fraction in lowest terms, as a flonum.
pub fn numerator(x: f64) -> f64 {
    numerator_denominator(x).0
}

/// The (positive) denominator of `x` as a fraction in lowest terms, as a
/// flonum: infinity for a subnormal's, which is a power of two past the
/// range.
pub fn denominator(x: f64) -> f64 {
    numerator_denominator(x).1
}

fn numerator_denominator(x: f64) -> (f64, f64) {
    if x.is_nan() {
        return (x, x);
    }
    if x.is_infinite() || x == 0.0 || x.fract() == 0.0 {
        return (x, 1.0);
    }
    // A finite non-integer is m * 2^e with m odd and e < 0.
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mantissa, exponent) = if exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), exponent - 1075)
    };
    let zeros = mantissa.trailing_zeros() as i32;
    let mantissa = mantissa >> zeros;
    let exponent = exponent + zeros;
    let numerator = (mantissa as f64).copysign(x);
    (numerator, libm::scalbn(1.0, -exponent))
}

/// `(a - b)` if positive, else 0 (C99 `fdim`); NaN if either is NaN.
pub fn positive_difference(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        f64::NAN
    } else if x > y {
        x - y
    } else {
        0.0
    }
}

/// The quotient `x / y` truncated towards zero.
pub fn quotient(x: f64, y: f64) -> f64 {
    (x / y).trunc()
}

/// The truncating remainder of `x / y`, C's `fmod`.
pub fn remainder(x: f64, y: f64) -> f64 {
    x % y
}

/// The remainder of `x / y` with the quotient rounded to nearest, and the
/// low bits of that quotient (at least three of them, with its sign).
pub fn remainder_quotient(x: f64, y: f64) -> (f64, i32) {
    libm::remquo(x, y)
}

/// `x` rounded to the nearest integer, ties to even.
pub fn round(x: f64) -> f64 {
    x.round_ties_even()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: f64, b: f64) -> bool {
        crate::flonum_eqv(a, b)
    }

    #[test]
    fn integer_and_fraction() {
        assert_eq!(integer_fraction(3.75), (3.0, 0.75));
        let (i, f) = integer_fraction(-3.75);
        assert_eq!((i, f), (-3.0, -0.75));
        let (i, f) = integer_fraction(-0.5);
        assert!(same(i, -0.0) && f == -0.5);
        assert_eq!(integer_fraction(f64::INFINITY), (f64::INFINITY, 0.0));
        let (i, f) = integer_fraction(f64::NEG_INFINITY);
        assert!(i == f64::NEG_INFINITY && same(f, -0.0));
        assert!(integer_fraction(f64::NAN).0.is_nan());
    }

    #[test]
    fn exponents() {
        assert_eq!(exponent(8.0), 3.0);
        assert_eq!(exponent(0.1), -4.0);
        assert_eq!(exponent(0.0), f64::NEG_INFINITY);
        assert_eq!(exponent(f64::NEG_INFINITY), f64::INFINITY);
        assert!(exponent(f64::NAN).is_nan());
        assert_eq!(integer_exponent(8.0), 3);
        assert_eq!(integer_exponent(0.0), INTEGER_EXPONENT_ZERO);
        assert_eq!(integer_exponent(f64::NAN), INTEGER_EXPONENT_NAN);
        assert_eq!(integer_exponent(f64::INFINITY), i32::MAX);
        assert_eq!(integer_exponent(f64::from_bits(1)), -1074);
        assert_eq!(normalized_fraction_exponent(8.0), (0.5, 4));
        assert_eq!(normalized_fraction_exponent(-3.0), (-0.75, 2));
        assert_eq!(normalized_fraction_exponent(0.0), (0.0, 0));
    }

    #[test]
    fn scaling() {
        assert_eq!(make_flonum(1.5, &Integer::from(4)), 24.0);
        assert_eq!(make_flonum(1.0, &Integer::from(i64::MAX)), f64::INFINITY);
        assert_eq!(make_flonum(1.0, &Integer::from(i64::MIN)), 0.0);
        assert_eq!(make_flonum(1.0, &Integer::from(-1074)), f64::from_bits(1));
        let huge = Integer::from(i128::MAX);
        assert_eq!(make_flonum(1.0, &huge), f64::INFINITY);
        assert_eq!(make_flonum(1.0, &-&huge), 0.0);
    }

    #[test]
    fn neighbours_and_signs() {
        assert_eq!(adjacent(1.0, 2.0), 1.0 + f64::EPSILON);
        assert_eq!(adjacent(1.0, 0.0), 1.0 - f64::EPSILON / 2.0);
        assert_eq!(adjacent(1.0, 1.0), 1.0);
        assert_eq!(adjacent(0.0, 1.0), f64::from_bits(1));
        assert!(adjacent(1.0, f64::NAN).is_nan());
        assert_eq!(sign_bit(-0.0), 1);
        assert_eq!(sign_bit(0.0), 0);
        assert_eq!(sign_bit(-2.0), 1);
    }

    #[test]
    fn classification() {
        assert!(is_normalized(1.0));
        assert!(!is_normalized(0.0));
        assert!(!is_normalized(f64::MIN_POSITIVE / 2.0));
        assert!(is_denormalized(f64::MIN_POSITIVE / 2.0));
        assert!(!is_denormalized(0.0));
        assert!(!is_denormalized(f64::INFINITY));
    }

    #[test]
    fn fractions() {
        assert_eq!((numerator(0.5), denominator(0.5)), (1.0, 2.0));
        assert_eq!((numerator(-0.75), denominator(-0.75)), (-3.0, 4.0));
        assert_eq!((numerator(6.0), denominator(6.0)), (6.0, 1.0));
        assert_eq!(
            (numerator(0.1), denominator(0.1)),
            (3602879701896397.0, 36028797018963968.0)
        );
        assert!(same(numerator(-0.0), -0.0) && denominator(-0.0) == 1.0);
        assert_eq!(
            (numerator(f64::INFINITY), denominator(f64::INFINITY)),
            (f64::INFINITY, 1.0)
        );
        assert!(numerator(f64::NAN).is_nan() && denominator(f64::NAN).is_nan());
        // A subnormal's denominator is past the flonum range.
        assert_eq!(denominator(f64::from_bits(1)), f64::INFINITY);
        assert_eq!(numerator(f64::from_bits(1)), 1.0);
    }

    #[test]
    fn division() {
        assert_eq!(quotient(7.0, 2.0), 3.0);
        assert_eq!(quotient(-7.0, 2.0), -3.0);
        assert_eq!(remainder(-7.0, 2.0), -1.0);
        assert_eq!(remainder_quotient(7.0, 2.0), (-1.0, 4));
        assert_eq!(positive_difference(5.0, 3.0), 2.0);
        assert_eq!(positive_difference(3.0, 5.0), 0.0);
        assert!(positive_difference(f64::NAN, 5.0).is_nan());
        assert_eq!(round(2.5), 2.0);
        assert_eq!(round(3.5), 4.0);
        assert_eq!(round(-2.5), -2.0);
    }

    #[test]
    fn huge_bessel_orders() {
        // Just past the bound, where libm can still check the closed form.
        let base = MAX_BESSEL_ORDER + 1;
        for n in [base, base + 1, -base, -base - 1] {
            for x in [
                1.0,
                -1.0,
                1000.0,
                -1000.0,
                0.0,
                -0.0,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NAN,
            ] {
                let order = Integer::from(n);
                let (j, y) = (
                    bessel_first(&order, x).unwrap(),
                    bessel_second(&order, x).unwrap(),
                );
                let (lj, ly) = (libm::jn(n as i32, x), libm::yn(n as i32, x));
                assert!(same(j, lj), "J {n} {x}: {j} vs {lj}");
                assert!(same(y, ly), "Y {n} {x}: {y} vs {ly}");
            }
        }
    }

    #[test]
    fn enormous_bessel_orders_do_not_recur() {
        let start = std::time::Instant::now();
        let big = Integer::from(1u8) * Integer::from(u64::MAX) * Integer::from(u64::MAX);
        for n in [
            Integer::from(i64::MAX),
            big.clone(),
            -&big,
            Integer::from(1i64 << 31),
        ] {
            assert_eq!(bessel_first(&n, 1.0).unwrap(), 0.0);
            let odd = n.limbs().1[0] & 1 == 1;
            let expected = if odd && n.is_negative() {
                f64::INFINITY
            } else {
                f64::NEG_INFINITY
            };
            assert_eq!(bessel_second(&n, 1.0).unwrap(), expected);
            assert!(bessel_second(&n, -1.0).unwrap().is_nan());
            assert!(bessel_first(&n, f64::NAN).unwrap().is_nan());
            assert_eq!(bessel_first(&n, f64::INFINITY).unwrap(), 0.0);
            assert_eq!(bessel_first(&n, 0.0).unwrap(), 0.0);
            // Past half the order the function oscillates.
            assert_eq!(bessel_first(&n, 1e300), Err(ArithError::TooLarge));
            assert_eq!(bessel_second(&n, 1e300), Err(ArithError::TooLarge));
        }
        assert!(start.elapsed().as_secs() < 5);
    }

    #[test]
    fn special_functions() {
        assert!((gamma(5.0) - 24.0).abs() < 1e-12);
        assert!((gamma(0.5) - std::f64::consts::PI.sqrt()).abs() < 1e-14);
        let (v, s) = log_gamma(-0.5);
        assert!((v - gamma(-0.5).abs().ln()).abs() < 1e-12 && s == -1.0);
        assert_eq!(log_gamma(3.0).1, 1.0);
        assert!((erf(1.0) - 0.8427007929497149).abs() < 1e-15);
        assert!((erfc(1.0) - 0.15729920705028513).abs() < 1e-15);
        assert!((bessel_first(&Integer::from(0), 1.0).unwrap() - 0.7651976865579666).abs() < 1e-15);
        assert!(
            (bessel_second(&Integer::from(1), 1.0).unwrap() + 0.7812128213002887).abs() < 1e-14
        );
        assert!((expm1(1e-10) - 1.00000000005e-10).abs() < 1e-24);
        assert!((log1p(1e-10) - 9.9999999995e-11).abs() < 1e-24);
        assert_eq!(hypot(3.0, 4.0), 5.0);
        assert_eq!(fma(2.0, 3.0, 1.0), 7.0);
        assert_eq!(cbrt(27.0), 3.0);
    }
}
