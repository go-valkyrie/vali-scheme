//! Vali's ABI facts (ADRs 0002 and 0006).
//!
//! Everything here is a constant or a pure function on 64-bit words, with no
//! heap and no runtime, so the runtime, the VM and the code generator all
//! read the same facts:
//!
//! - [`Tag`]: the low 3 bits of every value, and [`fixnum`] encoding.
//! - [`immediate`]: characters, booleans, the empty list and the other
//!   values with tag `111`.
//! - [`Header`] and [`TypeCode`]: the header word, split between the memory
//!   strategy's half and the runtime's type code and data.
//! - [`Layout`]: where each field of each kind of object sits, given which
//!   objects the strategy gives a header.
//!
//! Changing anything here changes the ABI, so [`ABI_VERSION`] changes with it.

#![no_std]

pub mod fixnum;
mod header;
pub mod immediate;
mod layout;
mod tag;

pub use header::{Header, TypeCode, data};
pub use layout::{Layout, StrategyLayout};
pub use tag::Tag;

/// The version of everything in this crate. Native code and run-format
/// bytecode built for one version are rebuilt for another; distribution
/// artifacts are not affected (ADR 0006). The memory strategy is part of the
/// full ABI version too, through [`StrategyLayout`].
pub const ABI_VERSION: u32 = 1;

/// The size of a word, and of every value, in bytes.
pub const WORD_BYTES: usize = 8;
