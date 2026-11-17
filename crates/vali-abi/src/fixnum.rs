//! Fixnums: 61-bit two's-complement integers, shifted left over a zero tag.
//! Adding or subtracting two fixnum words gives the fixnum word of the
//! result, so arithmetic fast paths work on the words directly.

use crate::Tag;

/// How many bits a fixnum has.
pub const BITS: u32 = 64 - Tag::BITS;
/// The smallest fixnum, −2⁶⁰.
pub const MIN: i64 = -(1 << (BITS - 1));
/// The largest fixnum, 2⁶⁰ − 1.
pub const MAX: i64 = (1 << (BITS - 1)) - 1;

/// The fixnum word for `n`, if `n` fits.
pub const fn encode(n: i64) -> Option<u64> {
    if n < MIN || n > MAX {
        None
    } else {
        Some((n << Tag::BITS) as u64)
    }
}

/// The integer a fixnum word holds. Meaningless if `word` is not a fixnum.
pub const fn decode(word: u64) -> i64 {
    (word as i64) >> Tag::BITS
}

/// Whether `word` is a fixnum.
pub const fn is_fixnum(word: u64) -> bool {
    word & Tag::MASK == Tag::Fixnum as u64
}
