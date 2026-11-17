use dashu_int::IBig;

use crate::Radix;

/// An exact integer of unlimited size.
///
/// Values that fit in two machine words are stored inline, without
/// allocating.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Integer(pub(crate) IBig);

impl Integer {
    /// Zero.
    pub const ZERO: Integer = Integer(IBig::ZERO);

    /// Whether the integer is zero.
    pub fn is_zero(&self) -> bool {
        self.0 == IBig::ZERO
    }

    /// Whether the integer is less than zero.
    pub fn is_negative(&self) -> bool {
        self.0 < IBig::ZERO
    }

    /// The value as an `i64`, if it fits.
    pub fn to_i64(&self) -> Option<i64> {
        i64::try_from(&self.0).ok()
    }

    /// The value as a `u64`, if it fits.
    pub fn to_u64(&self) -> Option<u64> {
        u64::try_from(&self.0).ok()
    }

    /// The value as an `i128`, if it fits.
    pub fn to_i128(&self) -> Option<i128> {
        i128::try_from(&self.0).ok()
    }

    /// The nearest `f64`, rounding ties to even, as Scheme's `inexact` does.
    pub fn to_f64(&self) -> f64 {
        self.0.to_f64().value()
    }

    /// The digits in `radix`, with a leading `-` if negative, as Scheme's
    /// `number->string` writes them.
    pub fn to_string_radix(&self, radix: Radix) -> String {
        format!("{}", self.0.in_radix(radix.value() as u8))
    }
}

impl std::fmt::Display for Integer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

macro_rules! from_primitive {
    ($($t:ty),*) => {$(
        impl From<$t> for Integer {
            fn from(n: $t) -> Self {
                Integer(IBig::from(n))
            }
        }
    )*};
}
from_primitive!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

/// Access to an integer's representation, for runtimes that store integers
/// in their own heap.
#[cfg(target_pointer_width = "64")]
impl Integer {
    /// The integer whose magnitude is `limbs`, least significant first, and
    /// which is negative if `negative` is set.
    pub fn from_limbs(negative: bool, limbs: &[u64]) -> Integer {
        let sign = if negative {
            dashu_int::Sign::Negative
        } else {
            dashu_int::Sign::Positive
        };
        Integer(IBig::from_parts(sign, dashu_int::UBig::from_words(limbs)))
    }

    /// Whether the integer is negative, and its magnitude's limbs, least
    /// significant first, with no high zero limbs.
    pub fn limbs(&self) -> (bool, &[u64]) {
        let (sign, words) = self.0.as_sign_words();
        (sign == dashu_int::Sign::Negative, words)
    }
}

macro_rules! arithmetic {
    ($($trait:ident $method:ident),*) => {$(
        impl std::ops::$trait for &Integer {
            type Output = Integer;

            fn $method(self, other: &Integer) -> Integer {
                Integer(std::ops::$trait::$method(&self.0, &other.0))
            }
        }

        impl std::ops::$trait for Integer {
            type Output = Integer;

            fn $method(self, other: Integer) -> Integer {
                Integer(std::ops::$trait::$method(self.0, other.0))
            }
        }
    )*};
}

arithmetic!(Add add, Sub sub, Mul mul, Div div, Rem rem);
