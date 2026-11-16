use dashu_int::{IBig, UBig};
use dashu_ratio::RBig;

use crate::{Integer, Number, Radix};

/// An exact rational that is not an integer, always in lowest terms with a
/// positive denominator greater than one.
///
/// Use [`Number::from_ratio`] to build one; it returns a [`Number`] because
/// the result may turn out to be an integer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rational(pub(crate) RBig);

impl Rational {
    /// The numerator, carrying the sign.
    pub fn numerator(&self) -> Integer {
        Integer(self.0.numerator().clone())
    }

    /// The denominator, always greater than one.
    pub fn denominator(&self) -> Integer {
        Integer(IBig::from(self.0.denominator().clone()))
    }

    /// The nearest `f64`, rounding ties to even, as Scheme's `inexact` does.
    pub fn to_f64(&self) -> f64 {
        self.0.to_f64().value()
    }

    /// `numerator/denominator` in `radix`, as Scheme's `number->string`
    /// writes it.
    pub fn to_string_radix(&self, radix: Radix) -> String {
        format!(
            "{}/{}",
            self.numerator().to_string_radix(radix),
            self.denominator().to_string_radix(radix)
        )
    }
}

impl std::fmt::Display for Rational {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.0.numerator(), self.0.denominator())
    }
}

/// `numerator / denominator` in lowest terms, or `None` for a zero
/// denominator.
pub(crate) fn from_ratio(numerator: Integer, denominator: Integer) -> Option<Number> {
    if denominator.is_zero() {
        return None;
    }
    Some(from_rbig(RBig::from_parts_signed(
        numerator.0,
        denominator.0,
    )))
}

/// Wraps a normalised `RBig` as the right kind of exact number.
pub(crate) fn from_rbig(r: RBig) -> Number {
    if r.denominator() == &UBig::ONE {
        let (numerator, _) = r.into_parts();
        Number::Integer(Integer(numerator))
    } else {
        Number::Rational(Rational(r))
    }
}

/// The exact value of a finite `f64`, as Scheme's `exact` computes it.
/// Returns `None` for infinities and NaN, which have no exact value.
pub(crate) fn exact_from_f64(x: f64) -> Option<RBig> {
    if !x.is_finite() {
        return None;
    }
    let bits = x.to_bits();
    let negative = bits >> 63 == 1;
    let biased_exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1u64 << 52) - 1);
    // Subnormals have no implicit leading bit and the minimum exponent.
    let (mantissa, exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased_exponent - 1075)
    };
    let mut numerator = IBig::from(mantissa);
    if negative {
        numerator = -numerator;
    }
    Some(if exponent >= 0 {
        RBig::from(numerator << exponent as usize)
    } else {
        RBig::from_parts(numerator, UBig::ONE << (-exponent) as usize)
    })
}
