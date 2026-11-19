//! Vali's run-format bytecode (ADR 0005), specified in `docs/bytecode.md`.
//!
//! - [`Instruction`] and [`Opcode`]: the instructions, and their encoding as
//!   32-bit words.
//! - [`Module`] and [`Function`]: what the VM loads.
//! - [`FunctionBuilder`] and [`ModuleBuilder`]: writing bytecode in Rust,
//!   with labels for branches.
//! - [`validate`]: the checks that make any loaded module safe to run.
//! - [`ownership`]: the pass that places `move` and `drop` in a module's
//!   code as an isolate that counts precisely loads it.
//! - [`disassemble`]: a readable listing.
//!
//! The format is private and versioned ([`FORMAT_VERSION`]): it is a cache
//! format, rebuilt from source or a distribution artifact on any mismatch.

mod builder;
mod convert;
mod instruction;
mod module;
pub mod ownership;
mod validate;

pub use builder::{BuildError, FunctionBuilder, Label, ModuleBuilder};
pub use convert::{ConvertError, convert};
pub use instruction::{
    DecodeError, Instruction, JUMP_RANGE, Opcode, Slot, field_d, field_e_signed, fields,
};
pub use module::{
    Arity, Constant, Export, FORMAT_VERSION, Function, Inlined, Link, Module, Position, Scope,
    Variable, branch_target, disassemble, instructions,
};
#[doc(hidden)]
pub use validate::validations;
pub use validate::{MAX_SLOTS, ValidationError, validate, validate_rewritten};
