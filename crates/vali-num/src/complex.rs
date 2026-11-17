use dashu_ratio::RBig;

use crate::rational::from_rbig;
use crate::{Number, Radix, flonum_eqv, print};

/// A complex number whose value is not real.
///
/// Its parts are either both exact or both inexact; R7RS never mixes them.
/// An exact complex number always has a non-zero imaginary part. An inexact
/// one may have a zero imaginary part, and is still not real.
#[derive(Clone, Debug)]
pub struct Complex(pub(crate) Repr);

#[derive(Clone, Debug)]
pub(crate) enum Repr {
    Exact { re: RBig, im: RBig },
    Inexact { re: f64, im: f64 },
}

impl Complex {
    /// `re + im·i` from exact parts. Returns a real [`Number`] when `im` is
    /// zero, as Scheme's `make-rectangular` does for exact arguments.
    pub(crate) fn exact(re: RBig, im: RBig) -> Number {
        if im == RBig::ZERO {
            from_rbig(re)
        } else {
            Number::Complex(Complex(Repr::Exact { re, im }))
        }
    }

    /// `re + im·i` from inexact parts. Always complex, even when `im` is
    /// zero.
    pub(crate) fn inexact(re: f64, im: f64) -> Number {
        Number::Complex(Complex(Repr::Inexact { re, im }))
    }

    /// The real part.
    pub fn real_part(&self) -> Number {
        match &self.0 {
            Repr::Exact { re, .. } => from_rbig(re.clone()),
            Repr::Inexact { re, .. } => Number::Real(*re),
        }
    }

    /// The imaginary part.
    pub fn imag_part(&self) -> Number {
        match &self.0 {
            Repr::Exact { im, .. } => from_rbig(im.clone()),
            Repr::Inexact { im, .. } => Number::Real(*im),
        }
    }

    /// Whether both parts are exact.
    pub fn is_exact(&self) -> bool {
        matches!(self.0, Repr::Exact { .. })
    }

    /// Scheme's `eqv?` on complex numbers: the same exactness, and parts that
    /// are `eqv?`.
    pub(crate) fn eqv(&self, other: &Complex) -> bool {
        match (&self.0, &other.0) {
            (Repr::Exact { re: a, im: b }, Repr::Exact { re: c, im: d }) => a == c && b == d,
            (Repr::Inexact { re: a, im: b }, Repr::Inexact { re: c, im: d }) => {
                flonum_eqv(*a, *c) && flonum_eqv(*b, *d)
            }
            _ => false,
        }
    }

    /// The number in `radix`, or `None` if it is inexact and `radix` is not
    /// 10.
    pub fn to_string_radix(&self, radix: Radix) -> Option<String> {
        let re = print::to_string_radix(&self.real_part(), radix)?;
        let im = print::to_string_radix(&self.imag_part(), radix)?;
        // The imaginary part always needs a sign; +inf.0, -inf.0, +nan.0 and
        // negative numbers already carry one.
        let sign = if im.starts_with(['+', '-']) { "" } else { "+" };
        Some(format!("{re}{sign}{im}i"))
    }
}

impl std::fmt::Display for Complex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self
            .to_string_radix(Radix::Decimal)
            .expect("every number prints in radix 10");
        f.write_str(&text)
    }
}
