//! Vali's run-format bytecode (ADR 0005), specified in `docs/bytecode.md`.
//!
//! - [`Instruction`] and [`Opcode`]: the instructions, and their encoding as
//!   32-bit words.
//! - [`Module`] and [`Function`]: what the VM loads.
//! - [`FunctionBuilder`] and [`ModuleBuilder`]: writing bytecode in Rust,
//!   with labels for branches.
//! - [`validate`]: the checks that make any loaded module safe to run.
//! - [`disassemble`]: a readable listing.
//!
//! The format is private and versioned ([`FORMAT_VERSION`]): it is a cache
//! format, rebuilt from source or a distribution artifact on any mismatch.

mod builder;
mod instruction;
mod module;
mod validate;

pub use builder::{BuildError, FunctionBuilder, Label, ModuleBuilder};
pub use instruction::{
    DecodeError, Instruction, JUMP_RANGE, Opcode, Slot, field_d, field_e_signed, fields,
};
pub use module::{
    Arity, Constant, FORMAT_VERSION, Function, Module, branch_target, disassemble, instructions,
};
pub use validate::{MAX_SLOTS, ValidationError, validate};
