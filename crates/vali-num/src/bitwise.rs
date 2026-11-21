//! Bitwise operations on exact integers of any size (SRFI 151).
//!
//! Every integer stands for a semi-infinite two's-complement bit string:
//! a non-negative one has finitely many 1 bits and a negative one finitely
//! many 0 bits. Bit 0 is the least significant.
//!
//! An operation that would build a result of more than
//! [`MAX_BITS`] bits fails with [`ArithError::TooLarge`] instead
//! of allocating it, as `expt` does: `(arithmetic-shift 1 (expt 10 12))`
//! is an error, not a terabyte.

use dashu_int::IBig;
use dashu_int::ops::{BitTest, UnsignedAbs};

use crate::Integer;
use crate::arith::{ArithError, MAX_EXACT_EXPT_BITS, Result};

/// The most bits a result of a shift, a bit-field operation or a bit copy
/// may have.
pub const MAX_BITS: u64 = MAX_EXACT_EXPT_BITS;

/// The bit count `n` as a `usize` no larger than [`MAX_BITS`], if it is
/// one.
fn bounded(n: &Integer) -> Option<usize> {
    n.to_u64()
        .filter(|&n| n <= MAX_BITS)
        .and_then(|n| usize::try_from(n).ok())
}

/// A non-negative integer as a `usize`, or `usize::MAX` if it is larger.
fn saturating(n: &Integer) -> usize {
    n.to_u64()
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(usize::MAX)
}

fn check_index(n: &Integer) -> Result<()> {
    if n.is_negative() {
        Err(ArithError::Domain("a non-negative exact integer"))
    } else {
        Ok(())
    }
}

/// `bitwise-not`: `-i - 1`.
pub fn not(i: &Integer) -> Integer {
    Integer(!&i.0)
}

/// `bitwise-and` of two integers.
pub fn and(a: &Integer, b: &Integer) -> Integer {
    Integer(&a.0 & &b.0)
}

/// `bitwise-ior` of two integers.
pub fn ior(a: &Integer, b: &Integer) -> Integer {
    Integer(&a.0 | &b.0)
}

/// `bitwise-xor` of two integers.
pub fn xor(a: &Integer, b: &Integer) -> Integer {
    Integer(&a.0 ^ &b.0)
}

/// `arithmetic-shift`: `i` times 2 to the `count`, rounded towards
/// negative infinity.
///
/// A shift left whose result would pass [`MAX_BITS`] bits is
/// [`ArithError::TooLarge`]; a shift right by more bits than `i` has is
/// 0, or -1 for a negative `i`, however large the count.
pub fn arithmetic_shift(i: &Integer, count: &Integer) -> Result<Integer> {
    if count.is_negative() {
        let by = saturating(&-count);
        if by as u64 > i.0.bit_len() as u64 {
            return Ok(Integer::from(if i.is_negative() { -1 } else { 0 }));
        }
        return Ok(Integer(&i.0 >> by));
    }
    if i.is_zero() {
        return Ok(Integer::ZERO);
    }
    let by = bounded(count).ok_or(ArithError::TooLarge)?;
    if (i.0.bit_len() + by) as u64 > MAX_BITS {
        return Err(ArithError::TooLarge);
    }
    Ok(Integer(&i.0 << by))
}

/// `bit-count`: the number of 1 bits of a non-negative integer, or of 0
/// bits of a negative one.
pub fn bit_count(i: &Integer) -> u64 {
    let magnitude = if i.is_negative() { !&i.0 } else { i.0.clone() };
    magnitude.unsigned_abs().count_ones() as u64
}

/// `integer-length`: the bits needed to write `i` in two's complement,
/// not counting the sign.
pub fn integer_length(i: &Integer) -> u64 {
    if i.is_negative() {
        (!&i.0).bit_len() as u64
    } else {
        i.0.bit_len() as u64
    }
}

/// `bit-set?`: whether bit `index` of `i` is 1. Every bit past the last of
/// a negative integer is.
pub fn bit_set(index: &Integer, i: &Integer) -> Result<bool> {
    check_index(index)?;
    Ok(match index.to_u64().and_then(|n| usize::try_from(n).ok()) {
        Some(n) => i.0.bit(n),
        None => i.is_negative(),
    })
}

/// `first-set-bit`: the index of the lowest 1 bit, or -1 for zero.
pub fn first_set_bit(i: &Integer) -> i64 {
    i.0.trailing_zeros().map_or(-1, |n| n as i64)
}

/// `copy-bit`: `i` with bit `index` set to `value`.
pub fn copy_bit(index: &Integer, i: &Integer, value: bool) -> Result<Integer> {
    check_index(index)?;
    if bit_set(index, i)? == value {
        return Ok(i.clone());
    }
    // Bit `n` makes a result of `n + 1` bits.
    let n = bounded(index)
        .filter(|&n| (n as u64) < MAX_BITS)
        .ok_or(ArithError::TooLarge)?;
    let bit = IBig::ONE << n;
    Ok(Integer(if value { &i.0 | &bit } else { &i.0 & !bit }))
}

/// `bit-field`: bits `start` up to `end` of `i`, shifted down to bit 0.
///
/// Fields past the top of a non-negative integer read as zeros, so
/// `(bit-field 6 2 999)` is 1; the field of a negative integer that
/// passes [`MAX_BITS`] bits is [`ArithError::TooLarge`].
pub fn bit_field(i: &Integer, start: &Integer, end: &Integer) -> Result<Integer> {
    check_index(start)?;
    check_index(end)?;
    if end < start {
        return Err(ArithError::Domain(
            "a field whose end is not before its start",
        ));
    }
    let width = end - start;
    let shifted = arithmetic_shift(i, &-start)?;
    if !shifted.is_negative()
        && (width
            .to_u64()
            .is_none_or(|w| w >= shifted.0.bit_len() as u64))
    {
        return Ok(shifted);
    }
    let width = bounded(&width).ok_or(ArithError::TooLarge)?;
    let mask: IBig = (IBig::ONE << width) - IBig::ONE;
    Ok(Integer(&shifted.0 & mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Integer {
        Integer(s.parse::<IBig>().unwrap())
    }

    fn i(x: i64) -> Integer {
        Integer::from(x)
    }

    fn pow2(k: usize) -> Integer {
        Integer(IBig::ONE << k)
    }

    #[test]
    fn logic_on_negatives_is_twos_complement() {
        assert_eq!(and(&i(-1), &i(37)), i(37));
        assert_eq!(and(&i(-2), &i(7)), i(6));
        assert_eq!(ior(&i(-8), &i(3)), i(-5));
        assert_eq!(xor(&i(-1), &i(5)), i(-6));
        assert_eq!(not(&i(10)), i(-11));
        assert_eq!(not(&i(-37)), i(36));
        assert_eq!(not(&i(0)), i(-1));
        assert_eq!(not(&i(-1)), i(0));
    }

    #[test]
    fn logic_on_bignums() {
        let big = pow2(100);
        let neg = -&big;
        assert_eq!(and(&neg, &big), big);
        assert_eq!(ior(&neg, &i(1)), &neg + &i(1));
        assert_eq!(xor(&big, &big), i(0));
        assert_eq!(and(&not(&big), &big), i(0));
        assert_eq!(not(&not(&neg)), neg);
        // Across the fixnum boundary.
        let edge = i((1 << 60) - 1);
        assert_eq!(ior(&edge, &i(1 << 60)), i((1 << 61) - 1));
        assert_eq!(and(&i(-(1 << 60) - 1), &i(-1)), i(-(1 << 60) - 1));
    }

    #[test]
    fn shifts() {
        assert_eq!(arithmetic_shift(&i(8), &i(2)).unwrap(), i(32));
        assert_eq!(arithmetic_shift(&i(4), &i(0)).unwrap(), i(4));
        assert_eq!(arithmetic_shift(&i(8), &i(-1)).unwrap(), i(4));
        assert_eq!(arithmetic_shift(&i(-1), &i(-5)).unwrap(), i(-1));
        assert_eq!(arithmetic_shift(&i(-5), &i(-1)).unwrap(), i(-3));
        assert_eq!(arithmetic_shift(&i(-4), &i(-1)).unwrap(), i(-2));
        assert_eq!(
            arithmetic_shift(&n("-100000000000000000000000000000000"), &i(-100)).unwrap(),
            i(-79)
        );
        assert_eq!(arithmetic_shift(&i(1), &i(100)).unwrap(), pow2(100));
        assert_eq!(arithmetic_shift(&i(-1), &i(100)).unwrap(), -pow2(100));
        assert_eq!(
            arithmetic_shift(&i(0), &n("99999999999999999999999")).unwrap(),
            i(0)
        );
    }

    #[test]
    fn huge_shifts_are_bounded() {
        let huge = n("100000000000000000000000000000");
        assert_eq!(arithmetic_shift(&i(1), &huge), Err(ArithError::TooLarge));
        assert_eq!(
            arithmetic_shift(&i(1), &i(1 << 40)),
            Err(ArithError::TooLarge)
        );
        assert_eq!(
            arithmetic_shift(&i(1), &i(MAX_BITS as i64)),
            Err(ArithError::TooLarge)
        );
        assert!(arithmetic_shift(&i(1), &i(MAX_BITS as i64 - 1)).is_ok());
        // Right by a huge count never allocates.
        assert_eq!(arithmetic_shift(&i(12345), &-&huge).unwrap(), i(0));
        assert_eq!(arithmetic_shift(&i(-12345), &-&huge).unwrap(), i(-1));
        assert_eq!(arithmetic_shift(&-pow2(200), &i(i64::MIN)).unwrap(), i(-1));
    }

    #[test]
    fn counts_and_lengths() {
        assert_eq!(bit_count(&i(0)), 0);
        assert_eq!(bit_count(&i(-1)), 0);
        assert_eq!(bit_count(&i(7)), 3);
        assert_eq!(bit_count(&i(13)), 3);
        assert_eq!(bit_count(&i(-13)), 2);
        assert_eq!(bit_count(&i(30)), 4);
        assert_eq!(bit_count(&i(-30)), 4);
        assert_eq!(bit_count(&pow2(100)), 1);
        assert_eq!(bit_count(&-pow2(100)), 100);
        assert_eq!(bit_count(&-(&pow2(100) + &i(1))), 1);
        assert_eq!(integer_length(&i(0)), 0);
        assert_eq!(integer_length(&i(1)), 1);
        assert_eq!(integer_length(&i(-1)), 0);
        assert_eq!(integer_length(&i(7)), 3);
        assert_eq!(integer_length(&i(-7)), 3);
        assert_eq!(integer_length(&i(8)), 4);
        assert_eq!(integer_length(&i(-8)), 3);
        assert_eq!(integer_length(&pow2(100)), 101);
        assert_eq!(integer_length(&-pow2(100)), 100);
        assert_eq!(integer_length(&i(1 << 60)), 61);
        assert_eq!(integer_length(&i(-(1 << 60))), 60);
    }

    #[test]
    fn single_bits() {
        assert!(!bit_set(&i(1), &i(1)).unwrap());
        assert!(bit_set(&i(0), &i(1)).unwrap());
        assert!(bit_set(&i(3), &i(10)).unwrap());
        assert!(bit_set(&i(1_000_000), &i(-1)).unwrap());
        assert!(!bit_set(&i(1_000_000), &i(5)).unwrap());
        assert!(bit_set(&n("99999999999999999999999"), &i(-1)).unwrap());
        assert!(!bit_set(&n("99999999999999999999999"), &i(1)).unwrap());
        assert!(bit_set(&i(-1), &i(1)).is_err());
        assert_eq!(first_set_bit(&i(1)), 0);
        assert_eq!(first_set_bit(&i(2)), 1);
        assert_eq!(first_set_bit(&i(0)), -1);
        assert_eq!(first_set_bit(&i(40)), 3);
        assert_eq!(first_set_bit(&i(-28)), 2);
        assert_eq!(first_set_bit(&pow2(99)), 99);
        assert_eq!(first_set_bit(&-pow2(99)), 99);
        assert_eq!(first_set_bit(&i(-1)), 0);
        assert_eq!(copy_bit(&i(0), &i(0), true).unwrap(), i(1));
        assert_eq!(copy_bit(&i(2), &i(0), true).unwrap(), i(4));
        assert_eq!(copy_bit(&i(2), &i(15), false).unwrap(), i(11));
        assert_eq!(copy_bit(&i(2), &i(-1), false).unwrap(), i(-5));
        assert_eq!(copy_bit(&i(100), &i(0), true).unwrap(), pow2(100));
        assert_eq!(copy_bit(&i(1 << 40), &i(0), false).unwrap(), i(0));
        assert_eq!(
            copy_bit(&i(1 << 40), &i(0), true),
            Err(ArithError::TooLarge)
        );
        assert_eq!(
            copy_bit(&i(1 << 40), &i(-1), false),
            Err(ArithError::TooLarge)
        );
    }

    #[test]
    fn results_stay_within_the_cap() {
        let cap = MAX_BITS as i64;
        let bits = |x: &Integer| integer_length(x);
        // A copy that sets bit n has n + 1 bits.
        let set = copy_bit(&i(cap - 1), &i(0), true).unwrap();
        assert_eq!(bits(&set), MAX_BITS);
        assert_eq!(copy_bit(&i(cap), &i(0), true), Err(ArithError::TooLarge));
        assert_eq!(
            copy_bit(&i(cap + 1), &i(0), true),
            Err(ArithError::TooLarge)
        );
        // Clearing a bit of a negative integer is as large.
        let clear = copy_bit(&i(cap - 1), &i(-1), false).unwrap();
        assert_eq!(bits(&clear), MAX_BITS);
        assert_eq!(copy_bit(&i(cap), &i(-1), false), Err(ArithError::TooLarge));
        assert_eq!(
            copy_bit(&i(cap + 1), &i(-1), false),
            Err(ArithError::TooLarge)
        );
        // A bit that is already right needs no new bits.
        assert_eq!(copy_bit(&i(cap), &i(0), false).unwrap(), i(0));
        // Shifts: the same cap.
        let shifted = arithmetic_shift(&i(1), &i(cap - 1)).unwrap();
        assert_eq!(bits(&shifted), MAX_BITS);
        assert_eq!(arithmetic_shift(&i(1), &i(cap)), Err(ArithError::TooLarge));
        assert_eq!(
            arithmetic_shift(&i(1), &i(cap + 1)),
            Err(ArithError::TooLarge)
        );
        assert_eq!(
            arithmetic_shift(&i(3), &i(cap - 1)),
            Err(ArithError::TooLarge)
        );
        // A field of a negative integer of exactly the cap's width fits.
        let field = bit_field(&i(-1), &i(0), &i(cap)).unwrap();
        assert_eq!(bits(&field), MAX_BITS);
        assert_eq!(
            bit_field(&i(-1), &i(0), &i(cap + 1)),
            Err(ArithError::TooLarge)
        );
        assert_eq!(bit_field(&i(-1), &i(5), &i(cap + 5)).unwrap(), field);
        assert_eq!(
            bit_field(&i(-1), &i(5), &i(cap + 6)),
            Err(ArithError::TooLarge)
        );
    }

    #[test]
    fn fields() {
        assert_eq!(
            bit_field(&i(0b1101101010), &i(0), &i(4)).unwrap(),
            i(0b1010)
        );
        assert_eq!(
            bit_field(&i(0b1101101010), &i(3), &i(9)).unwrap(),
            i(0b101101)
        );
        assert_eq!(bit_field(&i(6), &i(0), &i(1)).unwrap(), i(0));
        assert_eq!(bit_field(&i(6), &i(1), &i(3)).unwrap(), i(3));
        assert_eq!(bit_field(&i(6), &i(2), &i(999)).unwrap(), i(1));
        assert_eq!(bit_field(&pow2(128), &i(128), &i(129)).unwrap(), i(1));
        assert_eq!(bit_field(&i(-1), &i(0), &i(8)).unwrap(), i(255));
        assert_eq!(bit_field(&i(-2), &i(1), &i(4)).unwrap(), i(7));
        assert_eq!(bit_field(&i(5), &i(3), &i(3)).unwrap(), i(0));
        assert!(bit_field(&i(5), &i(3), &i(2)).is_err());
        assert!(bit_field(&i(5), &i(-1), &i(2)).is_err());
        assert_eq!(
            bit_field(&i(-1), &i(0), &i(1 << 40)),
            Err(ArithError::TooLarge)
        );
        // A huge field of a non-negative integer is just the integer.
        assert_eq!(
            bit_field(&i(77), &i(0), &n("99999999999999999999999999")).unwrap(),
            i(77)
        );
        assert_eq!(
            bit_field(
                &i(77),
                &n("99999999999999999999999999"),
                &n("999999999999999999999999999")
            )
            .unwrap(),
            i(0)
        );
    }
}
