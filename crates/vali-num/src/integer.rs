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
