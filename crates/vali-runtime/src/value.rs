use std::fmt;

use vali_abi::immediate::{self, Marker, Subtag};
use vali_abi::{Tag, fixnum};

/// A raw Scheme value: one tagged word (ADR 0002).
///
/// A `Value` is `Copy` and safe to hold, but it stays meaningful only until
/// the next allocation (ADR 0003), so reading the heap through one is
/// `unsafe`. Code outside the runtime holds values through handles instead.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Value(u64);

impl Value {
    /// `#f`.
    pub const FALSE: Value = Value(immediate::FALSE);
    /// `#t`.
    pub const TRUE: Value = Value(immediate::TRUE);
    /// The empty list.
    pub const EMPTY_LIST: Value = Value(immediate::EMPTY_LIST);
    /// The end-of-file object.
    pub const EOF: Value = Value(immediate::EOF);
    /// The unspecified value.
    pub const UNSPECIFIED: Value = Value(immediate::UNSPECIFIED);
    /// The default object.
    pub const DEFAULT: Value = Value(immediate::DEFAULT);
    /// The marker for a variable with no value yet.
    pub const UNBOUND: Value = Value(immediate::marker(Marker::Unbound));

    /// The value whose word is `word`.
    ///
    /// # Safety
    ///
    /// `word` must be a valid value's word: an immediate, or a pointer to
    /// a live object of the isolate it is used in, with that object's tag.
    /// The VM and every primitive trust a value without checking it.
    pub const unsafe fn from_raw(word: u64) -> Value {
        Value(word)
    }

    /// The value's word.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The value's tag.
    pub const fn tag(self) -> Tag {
        Tag::of(self.0)
    }

    /// The fixnum `n`, if it fits in 61 bits.
    pub const fn fixnum(n: i64) -> Option<Value> {
        match fixnum::encode(n) {
            Some(word) => Some(Value(word)),
            None => None,
        }
    }

    /// The integer, if this is a fixnum.
    pub const fn as_fixnum(self) -> Option<i64> {
        if fixnum::is_fixnum(self.0) {
            Some(fixnum::decode(self.0))
        } else {
            None
        }
    }

    /// The boolean `b`.
    pub const fn boolean(b: bool) -> Value {
        Value(immediate::boolean(b))
    }

    /// The boolean, if this is `#t` or `#f`.
    pub const fn as_boolean(self) -> Option<bool> {
        if immediate::is_boolean(self.0) {
            Some(self.0 == immediate::TRUE)
        } else {
            None
        }
    }

    /// Whether Scheme treats this as true: anything but `#f`.
    pub const fn is_true(self) -> bool {
        self.0 != immediate::FALSE
    }

    /// The character `c`.
    pub const fn character(c: char) -> Value {
        Value(immediate::character(c))
    }

    /// The character, if this is one.
    pub const fn as_character(self) -> Option<char> {
        immediate::to_character(self.0)
    }

    /// Whether this points into the heap.
    pub const fn is_pointer(self) -> bool {
        self.tag().is_pointer()
    }

    /// Whether this is a pair.
    pub const fn is_pair(self) -> bool {
        matches!(self.tag(), Tag::Pair)
    }

    /// The address this points to. Meaningless unless [`Value::is_pointer`].
    pub const fn address(self) -> u64 {
        Tag::address_of(self.0)
    }

    /// A pointer value: `address`, which must be 8-byte aligned, tagged with
    /// `tag`.
    ///
    /// # Safety
    ///
    /// `address` must be a live object of the isolate the value is used
    /// in, whose type has `tag`, as for [`Value::from_raw`].
    pub const unsafe fn from_address(tag: Tag, address: u64) -> Value {
        Value(tag.tag_address(address))
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(n) = self.as_fixnum() {
            return write!(f, "{n}");
        }
        if let Some(b) = self.as_boolean() {
            return f.write_str(if b { "#t" } else { "#f" });
        }
        if let Some(c) = self.as_character() {
            return write!(f, "#\\{c}");
        }
        match immediate::subtag(self.0) {
            Some(Subtag::EmptyList) => return f.write_str("()"),
            Some(Subtag::Eof) => return f.write_str("#<eof>"),
            Some(Subtag::Unspecified) => return f.write_str("#<unspecified>"),
            Some(Subtag::Default) => return f.write_str("#<default>"),
            Some(Subtag::Marker) => {
                return write!(f, "#<marker {}>", immediate::payload(self.0));
            }
            _ => {}
        }
        write!(f, "#<{:?} {:#x}>", self.tag(), self.address())
    }
}
