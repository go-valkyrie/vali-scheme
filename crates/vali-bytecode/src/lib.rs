//! Vali's run-format bytecode (ADR 0005), specified in `docs/bytecode.md`.
//!
//! - [`Instruction`] and [`Opcode`]: the instructions, and their encoding as
//!   32-bit words.
//! - [`Module`] and [`Function`]: what the VM loads.
//! - [`FunctionBuilder`] and [`ModuleBuilder`]: writing bytecode in Rust,
//!   with labels for branches.
//! - [`validate`]: the checks that make any loaded module safe to run.
//! - [`convert`] and [`convert_lazy`]: the distribution format's IR to a
//!   module, whole or a function at a time ([`LazyModule`]), and
//!   [`Converted`], which holds either.
//! - [`ownership`]: the pass that places `move` and `drop` in a module's
//!   code as an isolate that counts precisely loads it.
//! - [`disassemble`]: a readable listing.
//!
//! The format is private and versioned ([`FORMAT_VERSION`]): it is made as a
//! library loads, from source or a distribution artifact, and never stored.

mod builder;
mod convert;
mod converted;
mod debug;
mod instruction;
mod module;
pub mod ownership;
mod validate;

pub use builder::{BuildError, FunctionBuilder, Label, ModuleBuilder};
pub use convert::{ConvertError, Converter, LazyModule, convert, convert_lazy};
pub use converted::Converted;
pub use instruction::{
    DecodeError, Instruction, JUMP_RANGE, Opcode, Slot, field_d, field_e_signed, fields,
};
pub use module::{
    Arity, Constant, Export, FORMAT_VERSION, Function, Inlined, Link, Module, Position, Scope,
    Variable, branch_target, disassemble, instructions,
};
#[doc(hidden)]
pub use validate::validations;
pub use validate::{
    MAX_SLOTS, Shape, ValidationError, check_function, check_rewritten, validate, validate_lazy,
    validate_rewritten,
};
