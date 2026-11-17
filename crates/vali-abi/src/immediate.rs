//! Immediates (ADR 0002 #9): tag `111`, a 5-bit subtag above it, and a
//! 56-bit payload above that.

use crate::Tag;

/// How far the subtag is shifted.
pub const SUBTAG_SHIFT: u32 = Tag::BITS;
/// How many bits the subtag has.
pub const SUBTAG_BITS: u32 = 5;
/// How far the payload is shifted.
pub const PAYLOAD_SHIFT: u32 = SUBTAG_SHIFT + SUBTAG_BITS;
/// The largest payload.
pub const PAYLOAD_MAX: u64 = (1 << (64 - PAYLOAD_SHIFT)) - 1;

/// The kinds of immediate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Subtag {
    /// A character; the payload is its Unicode scalar value.
    Character = 0,
    /// `#f` (payload 0) or `#t` (payload 1).
    Boolean = 1,
    /// The empty list.
    EmptyList = 2,
    /// The end-of-file object.
    Eof = 3,
    /// The unspecified value.
    Unspecified = 4,
    /// The default object: an omitted optional argument.
    Default = 5,
    /// An internal marker, which never reaches Scheme code.
    Marker = 6,
}

impl Subtag {
    /// The subtag with number `n`, if it is assigned.
    pub const fn from_number(n: u8) -> Option<Subtag> {
        Some(match n {
            0 => Subtag::Character,
            1 => Subtag::Boolean,
            2 => Subtag::EmptyList,
            3 => Subtag::Eof,
            4 => Subtag::Unspecified,
            5 => Subtag::Default,
            6 => Subtag::Marker,
            _ => return None,
        })
    }
}

/// The internal markers (subtag [`Subtag::Marker`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Marker {
    /// A variable that has no value yet.
    Unbound = 0,
    /// A `letrec` binding read before it was initialised.
    Uninitialised = 1,
    /// A deleted hash-table entry.
    Tombstone = 2,
}

/// The immediate with `subtag` and `payload`. `payload` must fit in 56 bits.
pub const fn make(subtag: Subtag, payload: u64) -> u64 {
    debug_assert!(payload <= PAYLOAD_MAX);
    payload << PAYLOAD_SHIFT | (subtag as u64) << SUBTAG_SHIFT | Tag::Immediate as u64
}

/// `#f`.
pub const FALSE: u64 = make(Subtag::Boolean, 0);
/// `#t`.
pub const TRUE: u64 = make(Subtag::Boolean, 1);
/// The empty list.
pub const EMPTY_LIST: u64 = make(Subtag::EmptyList, 0);
/// The end-of-file object.
pub const EOF: u64 = make(Subtag::Eof, 0);
/// The unspecified value.
pub const UNSPECIFIED: u64 = make(Subtag::Unspecified, 0);
/// The default object.
pub const DEFAULT: u64 = make(Subtag::Default, 0);

/// The subtag of `word`, if it is an immediate with an assigned subtag.
pub const fn subtag(word: u64) -> Option<Subtag> {
    if word & Tag::MASK != Tag::Immediate as u64 {
        return None;
    }
    Subtag::from_number(((word >> SUBTAG_SHIFT) & ((1 << SUBTAG_BITS) - 1)) as u8)
}

/// The payload of an immediate.
pub const fn payload(word: u64) -> u64 {
    word >> PAYLOAD_SHIFT
}

/// The boolean for `b`.
pub const fn boolean(b: bool) -> u64 {
    if b { TRUE } else { FALSE }
}

/// Whether `word` is `#t` or `#f`: one mask and one compare.
pub const fn is_boolean(word: u64) -> bool {
    word & !(1 << PAYLOAD_SHIFT) == FALSE
}

/// The immediate for `c`.
pub const fn character(c: char) -> u64 {
    make(Subtag::Character, c as u64)
}

/// The character `word` holds, if it is a character.
pub const fn to_character(word: u64) -> Option<char> {
    if word & ((1 << PAYLOAD_SHIFT) - 1) != make(Subtag::Character, 0) {
        return None;
    }
    let scalar = payload(word);
    if scalar > char::MAX as u64 {
        return None;
    }
    char::from_u32(scalar as u32)
}

/// The immediate for `marker`.
pub const fn marker(marker: Marker) -> u64 {
    make(Subtag::Marker, marker as u64)
}
