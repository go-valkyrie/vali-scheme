//! What Vali's GDB and LLDB extensions read (ADR 0004 #6): a description of
//! each function, and of the memory strategy's object layout, in C layout,
//! so the extensions can walk Scheme frames and print Scheme values without
//! knowing how Rust lays out Vali's own types.
//!
//! The extensions, in `tools/debugger/`, find the innermost Scheme frame
//! through the interpreter's `fp` and `pc` locals, from DWARF. From a
//! frame, slot 0 is its closure, whose first word points to its function's
//! `Code`, which starts with a [`DebugFunction`]; that points to its
//! strategy's [`DebugLayout`]. Changing anything here changes
//! [`DEBUG_VERSION`], which the extensions check.

use vali_abi::{Layout, Tag, TypeCode};
use vali_bytecode::Position;

/// The version of these structures, which the extensions check. Bumped
/// whenever any of them, or what they describe, changes.
pub const DEBUG_VERSION: u32 = 1;

/// A string the extensions read: a pointer to UTF-8 bytes and their length.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DebugStr {
    /// The bytes.
    pub ptr: *const u8,
    /// How many.
    pub len: usize,
}

// SAFETY: a view of immutable bytes, kept alive by whatever holds the
// view, and only ever read.
unsafe impl Send for DebugStr {}
// SAFETY: as above.
unsafe impl Sync for DebugStr {}

impl DebugStr {
    /// A view of `s`, which must outlive it.
    pub(crate) fn of(s: &str) -> DebugStr {
        DebugStr {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// A strategy's object layout: what the extensions need to print values.
/// Offsets are in bytes from a tagged value, as `vali_abi::Layout` gives
/// them.
#[repr(C)]
#[derive(Debug)]
pub struct DebugLayout {
    /// [`DEBUG_VERSION`].
    pub version: u32,
    /// How many low bits a tag takes (ADR 0002).
    pub tag_bits: u32,
    /// A pair's `car` and `cdr`.
    pub car: i32,
    pub cdr: i32,
    /// A flonum's `f64`.
    pub flonum: i32,
    /// A closure's pointer to its `Code`, which starts with a
    /// [`DebugFunction`].
    pub closure_code: i32,
    /// A symbol's name, a string.
    pub symbol_name: i32,
    /// A string's bytevector of UTF-8.
    pub string_bytes: i32,
    /// A bytevector's length; its bytes follow that word.
    pub bytevector_length: i32,
    /// The header of an object tagged "other", whose low byte is its type
    /// code.
    pub other_header: i32,
    /// A vector's length, then its elements, one word each.
    pub vector_length: i32,
    /// A record's type descriptor; its fields follow, one word each.
    pub record_type: i32,
}

impl DebugLayout {
    /// The description of `layout`.
    pub(crate) const fn of(layout: Layout) -> DebugLayout {
        DebugLayout {
            version: DEBUG_VERSION,
            tag_bits: Tag::BITS,
            car: layout.car(),
            cdr: layout.cdr(),
            flonum: layout.flonum(),
            closure_code: layout.closure_code(),
            symbol_name: layout.symbol_name(),
            string_bytes: layout.string_bytes(),
            bytevector_length: layout.length(TypeCode::Bytevector),
            other_header: -(Tag::Other as i32),
            vector_length: layout.length(TypeCode::Vector),
            record_type: layout.record_type(),
        }
    }
}

/// A function, as the extensions see it: the first field of every `Code`.
#[repr(C)]
#[derive(Debug)]
pub struct DebugFunction {
    /// [`DEBUG_VERSION`].
    pub version: u32,
    /// How many slots its frames have; 0 for a procedure implemented in
    /// Rust, which has no Scheme frame.
    pub slots: u32,
    /// Its strategy's layout.
    pub layout: *const DebugLayout,
    /// Its name.
    pub name: DebugStr,
    /// Its line table, sorted by instruction (ADR 0014 #9), and how many
    /// entries it has.
    pub positions: *const Position,
    pub positions_len: usize,
    /// The sources its positions name, by index.
    pub sources: *const DebugStr,
    pub sources_len: usize,
}

impl DebugFunction {
    /// A function with no Scheme frames or positions: one implemented in
    /// Rust, or one the VM handles itself.
    pub(crate) fn native(layout: &'static DebugLayout, name: &str) -> DebugFunction {
        DebugFunction {
            version: DEBUG_VERSION,
            slots: 0,
            layout,
            name: DebugStr::of(name),
            positions: std::ptr::null(),
            positions_len: 0,
            sources: std::ptr::null(),
            sources_len: 0,
        }
    }
}
