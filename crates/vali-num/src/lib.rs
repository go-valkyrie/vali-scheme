//! The Scheme numeric tower for Vali: exact integers of unlimited size,
//! exact rationals, inexact reals and complex numbers (ADR 0007).
//!
//! The crate covers representation, parsing and printing of Scheme's
//! numeric syntax, the `eqv?` comparison, and the generic arithmetic of
//! R7RS in [`arith`]. The `std::ops` interface follows.
//!
//! The big-number types wrap `dashu`, but never expose it: [`Integer`],
//! [`Rational`] and [`Complex`] are opaque, so the dependency can change
//! without changing this crate's API.

pub mod arith;
pub mod bitwise;
mod complex;
mod integer;
mod parse;
mod print;
mod rational;

pub use complex::Complex;
pub use integer::Integer;
pub use parse::{MAX_EXACT_DECIMAL_EXPONENT, ParseError, ParseErrorKind, parse};
pub use rational::Rational;

/// One value of the numeric tower.
///
/// Every value is in its canonical form: a [`Rational`] is never an integer,
/// and an exact [`Complex`] never has a zero imaginary part. Inexact complex
/// numbers keep a zero imaginary part, as R7RS requires (`(real? 1.0+0.0i)`
/// is `#f`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Number {
    /// An exact integer.
    Integer(Integer),
    /// An exact rational that is not an integer, in lowest terms.
    Rational(Rational),
    /// An inexact real: an IEEE 754 binary64 value.
    Real(f64),
    /// A complex number with a non-real value, exact or inexact.
    Complex(Complex),
}

/// The radix of a numeral, as in R7RS's `#b`, `#o`, `#d` and `#x` prefixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Radix {
    /// Base 2 (`#b`).
    Binary = 2,
    /// Base 8 (`#o`).
    Octal = 8,
    /// Base 10 (`#d`), the default.
    Decimal = 10,
    /// Base 16 (`#x`).
    Hexadecimal = 16,
}

impl Radix {
    /// The radix as a number.
    pub fn value(self) -> u32 {
        self as u32
    }
}

impl Number {
    /// `numerator / denominator` as an exact number in lowest terms: a
    /// [`Number::Integer`] when the denominator divides the numerator,
    /// otherwise a [`Number::Rational`].
    ///
    /// Returns `None` when `denominator` is zero.
    pub fn from_ratio(numerator: Integer, denominator: Integer) -> Option<Number> {
        rational::from_ratio(numerator, denominator)
    }

    /// Whether the number is exact.
    pub fn is_exact(&self) -> bool {
        match self {
            Number::Integer(_) | Number::Rational(_) => true,
            Number::Real(_) => false,
            Number::Complex(c) => c.is_exact(),
        }
    }

    /// R7RS `integer?`: true for exact integers and for integral finite
    /// flonums such as `2.0`.
    pub fn is_integer(&self) -> bool {
        match self {
            Number::Integer(_) => true,
            Number::Real(x) => x.is_finite() && x.fract() == 0.0,
            Number::Rational(_) | Number::Complex(_) => false,
        }
    }

    /// R7RS `rational?`: true for exact rationals and finite flonums.
    pub fn is_rational(&self) -> bool {
        match self {
            Number::Integer(_) | Number::Rational(_) => true,
            Number::Real(x) => x.is_finite(),
            Number::Complex(_) => false,
        }
    }

    /// R7RS `real?`: false for every [`Complex`], including `1.0+0.0i`.
    pub fn is_real(&self) -> bool {
        !matches!(self, Number::Complex(_))
    }

    /// Scheme's `eqv?`: the same exactness and the same value.
    ///
    /// Flonums compare by their bits after every NaN is reduced to one
    /// canonical NaN, as Java's `Double.equals` does: all NaNs are `eqv?` to
    /// each other, and `0.0` and `-0.0` are not. Comparing raw bits would
    /// make the answer depend on the CPU, since x86 and ARM produce NaNs with
    /// different bit patterns.
    pub fn eqv(&self, other: &Number) -> bool {
        match (self, other) {
            (Number::Integer(a), Number::Integer(b)) => a == b,
            (Number::Rational(a), Number::Rational(b)) => a == b,
            (Number::Real(a), Number::Real(b)) => flonum_eqv(*a, *b),
            (Number::Complex(a), Number::Complex(b)) => a.eqv(b),
            _ => false,
        }
    }
}

/// `eqv?` on flonums: bitwise, with every NaN treated as the same value.
pub(crate) fn flonum_eqv(a: f64, b: f64) -> bool {
    canonical_bits(a) == canonical_bits(b)
}

fn canonical_bits(x: f64) -> u64 {
    if x.is_nan() {
        f64::NAN.to_bits()
    } else {
        x.to_bits()
    }
}

impl From<Integer> for Number {
    fn from(n: Integer) -> Self {
        Number::Integer(n)
    }
}

impl From<Rational> for Number {
    fn from(r: Rational) -> Self {
        Number::Rational(r)
    }
}

impl From<Complex> for Number {
    fn from(c: Complex) -> Self {
        Number::Complex(c)
    }
}

impl From<f64> for Number {
    fn from(x: f64) -> Self {
        Number::Real(x)
    }
}

macro_rules! from_primitive_integer {
    ($($t:ty),*) => {$(
        impl From<$t> for Number {
            fn from(n: $t) -> Self {
                Number::Integer(Integer::from(n))
            }
        }
    )*};
}
from_primitive_integer!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);
