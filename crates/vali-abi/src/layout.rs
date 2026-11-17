//! Where fields sit in heap objects (ADR 0002 #8, #10 and #12).
//!
//! Offsets are in bytes from a *tagged* pointer, so code adds them to the
//! value as it is, and the tag costs nothing.

use crate::{TypeCode, WORD_BYTES};

/// What a memory strategy decides about object layout (ADR 0003): whether
/// pairs and flonums carry a header. Every other object always has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StrategyLayout {
    /// Whether pairs carry a header.
    pub pair_header: bool,
    /// Whether flonums carry a header.
    pub flonum_header: bool,
}

impl StrategyLayout {
    /// Pairs and flonums without a header, as tracing keeps them.
    pub const COMPACT: StrategyLayout = StrategyLayout {
        pair_header: false,
        flonum_header: false,
    };
    /// Every object with a header, as counting with counts in the header
    /// needs.
    pub const HEADERED: StrategyLayout = StrategyLayout {
        pair_header: true,
        flonum_header: true,
    };
}

/// Object layouts under one [`StrategyLayout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Layout {
    strategy: StrategyLayout,
}

const W: i32 = WORD_BYTES as i32;

impl Layout {
    /// The layouts under `strategy`.
    pub const fn new(strategy: StrategyLayout) -> Layout {
        Layout { strategy }
    }

    /// What the strategy decided.
    pub const fn strategy(self) -> StrategyLayout {
        self.strategy
    }

    /// Whether objects of type `t` carry a header.
    pub const fn has_header(self, t: TypeCode) -> bool {
        match t {
            TypeCode::Pair => self.strategy.pair_header,
            TypeCode::Flonum => self.strategy.flonum_header,
            _ => true,
        }
    }

    /// How many words a header takes in objects of type `t`: 0 or 1.
    pub const fn header_words(self, t: TypeCode) -> usize {
        self.has_header(t) as usize
    }

    /// The offset of the header, if objects of type `t` have one.
    pub const fn header(self, t: TypeCode) -> Option<i32> {
        if self.has_header(t) {
            Some(-(t.tag() as i32))
        } else {
            None
        }
    }

    /// The offset of the `index`th word after the header.
    pub const fn field(self, t: TypeCode, index: usize) -> i32 {
        (self.header_words(t) + index) as i32 * W - t.tag() as i32
    }

    /// The size in words of an object of type `t` with `fields` words after
    /// its header.
    pub const fn words(self, t: TypeCode, fields: usize) -> usize {
        self.header_words(t) + fields
    }

    /// A pair's `car`.
    pub const fn car(self) -> i32 {
        self.field(TypeCode::Pair, 0)
    }

    /// A pair's `cdr`.
    pub const fn cdr(self) -> i32 {
        self.field(TypeCode::Pair, 1)
    }

    /// A flonum's `f64`.
    pub const fn flonum(self) -> i32 {
        self.field(TypeCode::Flonum, 0)
    }

    /// A closure's code pointer.
    pub const fn closure_code(self) -> i32 {
        self.field(TypeCode::Closure, 0)
    }

    /// A closure's `index`th free variable.
    pub const fn closure_free(self, index: usize) -> i32 {
        self.field(TypeCode::Closure, 1 + index)
    }

    /// A symbol's name.
    pub const fn symbol_name(self) -> i32 {
        self.field(TypeCode::Symbol, 0)
    }

    /// A symbol's precomputed hash.
    pub const fn symbol_hash(self) -> i32 {
        self.field(TypeCode::Symbol, 1)
    }

    /// A string's length in bytes.
    pub const fn string_byte_length(self) -> i32 {
        self.field(TypeCode::String, 0)
    }

    /// A string's length in characters.
    pub const fn string_char_count(self) -> i32 {
        self.field(TypeCode::String, 1)
    }

    /// The pointer to a string's UTF-8 bytes.
    pub const fn string_bytes(self) -> i32 {
        self.field(TypeCode::String, 2)
    }

    /// The pointer to a string's index hints, null until built.
    pub const fn string_hints(self) -> i32 {
        self.field(TypeCode::String, 3)
    }

    /// The length word of a vector, bytevector or other variable-length
    /// object.
    pub const fn length(self, t: TypeCode) -> i32 {
        self.field(t, 0)
    }

    /// A vector's `index`th element.
    pub const fn vector_element(self, index: usize) -> i32 {
        self.field(TypeCode::Vector, 1 + index)
    }

    /// A record's type descriptor.
    pub const fn record_type(self) -> i32 {
        self.field(TypeCode::Record, 0)
    }

    /// A record's `index`th field.
    pub const fn record_field(self, index: usize) -> i32 {
        self.field(TypeCode::Record, 1 + index)
    }
}
