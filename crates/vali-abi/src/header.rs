//! The header word (ADR 0002 #7):
//!
//! ```text
//!  63                       32 31                  8 7          0
//! [ strategy's half (32 bits) | runtime data (24)   | type code  ]
//! ```

use crate::Tag;

/// The type of a heap object, in the low byte of its header. Every type
/// code is odd, so bit 0 of a header is always 1, while a forwarding
/// pointer, being 8-byte aligned, has it clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum TypeCode {
    /// A pair, when its strategy gives pairs a header.
    Pair = 1,
    /// A flonum, when its strategy gives flonums a header.
    Flonum = 3,
    /// A closure; the data is its free-variable count.
    Closure = 5,
    /// A symbol; the data is one of the [`data::symbol`] kinds.
    Symbol = 7,
    /// A string; the data holds [`data::IMMUTABLE`] and [`data::ASCII`].
    String = 9,
    /// A vector.
    Vector = 11,
    /// A bytevector.
    Bytevector = 13,
    /// A box: an assigned variable that a closure captures.
    Box = 15,
    /// An exact integer outside the fixnum range.
    Bignum = 17,
    /// An exact non-integer rational.
    Ratnum = 19,
    /// An exact complex number.
    ExactComplex = 21,
    /// An inexact complex number.
    InexactComplex = 23,
    /// A record: its type descriptor, then its fields.
    Record = 25,
    /// A record type descriptor.
    RecordType = 27,
    /// An SRFI 88 keyword.
    Keyword = 29,
    /// Compiled code, bytecode or native, with its debug information.
    Code = 31,
    /// A captured continuation.
    Continuation = 33,
    /// A segment of a task's stack.
    StackSegment = 35,
    /// A pair whose `car` is held weakly.
    WeakPair = 37,
    /// An ephemeron.
    Ephemeron = 39,
    /// A port.
    Port = 41,
    /// A Rust object exposed to Scheme.
    RustObject = 43,
    /// A syntax object: an index into its context's table of them (ADR
    /// 0015 #13).
    Syntax = 45,
}

impl TypeCode {
    /// Every type code, in numeric order.
    pub const ALL: [TypeCode; 23] = [
        TypeCode::Pair,
        TypeCode::Flonum,
        TypeCode::Closure,
        TypeCode::Symbol,
        TypeCode::String,
        TypeCode::Vector,
        TypeCode::Bytevector,
        TypeCode::Box,
        TypeCode::Bignum,
        TypeCode::Ratnum,
        TypeCode::ExactComplex,
        TypeCode::InexactComplex,
        TypeCode::Record,
        TypeCode::RecordType,
        TypeCode::Keyword,
        TypeCode::Code,
        TypeCode::Continuation,
        TypeCode::StackSegment,
        TypeCode::WeakPair,
        TypeCode::Ephemeron,
        TypeCode::Port,
        TypeCode::RustObject,
        TypeCode::Syntax,
    ];

    /// The type code numbered `n`, if there is one.
    pub const fn from_u8(n: u8) -> Option<TypeCode> {
        if n & 1 == 0 || (n as usize) / 2 >= TypeCode::ALL.len() {
            return None;
        }
        Some(TypeCode::ALL[(n as usize) / 2])
    }

    /// The tag of a pointer to an object of this type.
    pub const fn tag(self) -> Tag {
        match self {
            TypeCode::Pair => Tag::Pair,
            TypeCode::Flonum => Tag::Flonum,
            TypeCode::Closure => Tag::Closure,
            TypeCode::Symbol => Tag::Symbol,
            TypeCode::String => Tag::String,
            _ => Tag::Other,
        }
    }
}

/// The runtime data stored in headers of particular types.
pub mod data {
    /// A string, vector or bytevector that cannot be modified, such as a
    /// literal.
    pub const IMMUTABLE: u32 = 1 << 0;
    /// A string whose characters are all ASCII, so character indices are
    /// byte indices.
    pub const ASCII: u32 = 1 << 1;

    /// The kinds of symbol.
    pub mod symbol {
        /// An interned symbol: equal names are the same symbol.
        pub const INTERNED: u32 = 0;
        /// A symbol made by `string->uninterned-symbol`.
        pub const UNINTERNED: u32 = 1;
        /// A symbol made by `generate-symbol` or for a temporary.
        pub const GENERATED: u32 = 2;
    }
}

/// A header word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Header(pub u64);

impl Header {
    /// How far the runtime data is shifted.
    pub const DATA_SHIFT: u32 = 8;
    /// How many bits the runtime data has.
    pub const DATA_BITS: u32 = 24;
    /// The largest runtime data, which also bounds a closure's free
    /// variables and a record's fields.
    pub const DATA_MAX: u32 = (1 << Header::DATA_BITS) - 1;
    /// How far the strategy's half is shifted.
    pub const STRATEGY_SHIFT: u32 = 32;

    /// A header for `type_code` with `data`, and an empty strategy's half.
    /// `None` if `data` does not fit in 24 bits.
    pub const fn new(type_code: TypeCode, data: u32) -> Option<Header> {
        if data > Header::DATA_MAX {
            return None;
        }
        Some(Header(
            (data as u64) << Header::DATA_SHIFT | type_code as u64,
        ))
    }

    /// Whether `word`, read where a header belongs, is a header rather than
    /// a forwarding pointer.
    pub const fn is_header(word: u64) -> bool {
        word & 1 == 1
    }

    /// The object's type. `None` only for a corrupt header.
    pub const fn type_code(self) -> Option<TypeCode> {
        TypeCode::from_u8(self.0 as u8)
    }

    /// The runtime data.
    pub const fn data(self) -> u32 {
        (self.0 >> Header::DATA_SHIFT) as u32 & Header::DATA_MAX
    }

    /// This header with its runtime data replaced. `None` if `data` does not
    /// fit in 24 bits.
    pub const fn with_data(self, data: u32) -> Option<Header> {
        if data > Header::DATA_MAX {
            return None;
        }
        let mask = (Header::DATA_MAX as u64) << Header::DATA_SHIFT;
        Some(Header(self.0 & !mask | (data as u64) << Header::DATA_SHIFT))
    }

    /// The strategy's half.
    pub const fn strategy_half(self) -> u32 {
        (self.0 >> Header::STRATEGY_SHIFT) as u32
    }

    /// This header with the strategy's half replaced.
    pub const fn with_strategy_half(self, half: u32) -> Header {
        Header(self.0 & 0xFFFF_FFFF | (half as u64) << Header::STRATEGY_SHIFT)
    }
}
