/// The low 3 bits of a value (ADR 0002).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tag {
    /// A 61-bit fixnum, the value shifted left by 3.
    Fixnum = 0b000,
    /// A pointer to a pair.
    Pair = 0b001,
    /// A pointer to a closure.
    Closure = 0b010,
    /// A pointer to a symbol.
    Symbol = 0b011,
    /// A pointer to a string.
    String = 0b100,
    /// A pointer to a boxed flonum.
    Flonum = 0b101,
    /// A pointer to any other heap object, whose type is in its header.
    Other = 0b110,
    /// An immediate: a character, a boolean, the empty list and so on.
    Immediate = 0b111,
}

impl Tag {
    /// How many low bits the tag takes.
    pub const BITS: u32 = 3;
    /// The mask that selects the tag.
    pub const MASK: u64 = 0b111;

    /// The tag of `word`.
    pub const fn of(word: u64) -> Tag {
        match word & Tag::MASK {
            0b000 => Tag::Fixnum,
            0b001 => Tag::Pair,
            0b010 => Tag::Closure,
            0b011 => Tag::Symbol,
            0b100 => Tag::String,
            0b101 => Tag::Flonum,
            0b110 => Tag::Other,
            _ => Tag::Immediate,
        }
    }

    /// Whether values with this tag point into the heap.
    pub const fn is_pointer(self) -> bool {
        !matches!(self, Tag::Fixnum | Tag::Immediate)
    }

    /// `address`, which must be 8-byte aligned, tagged with this tag.
    pub const fn tag_address(self, address: u64) -> u64 {
        debug_assert!(address & Tag::MASK == 0, "heap objects are 8-byte aligned");
        address | self as u64
    }

    /// The address a pointer value points to: the value without its tag.
    pub const fn address_of(word: u64) -> u64 {
        word & !Tag::MASK
    }
}
