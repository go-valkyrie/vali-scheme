//! Vali's distribution format (ADR 0012): the IR libraries are compiled
//! into and shipped as (ADR 0017).
//!
//! The IR is A-normal form with join points: scoped SSA. Every
//! intermediate value is bound once, by a `let`, a function's parameters
//! or a join point's; operands are variables or constants; control flow
//! nests, and a name is visible only inside what binds it, so checking
//! that every use follows its definition is one walk ([`validate`]).
//! Return points are not named: a non-tail call's continuation is what
//! follows its `let`, and becomes a frame when the IR is converted to
//! run-format bytecode.
//!
//! - [`Module`], [`Function`], [`Term`] and [`Value`]: the IR, held in
//!   arenas and referred to by index, so passes never recurse on nesting.
//! - [`Primitive`] and [`Effects`]: the operations on values the format
//!   defines, and what each may do.
//! - [`validate`]: the checks that make a module well formed.
//! - The [`Display`](std::fmt::Display) of a [`Module`]: a readable form.
//! - [`encode`] and [`decode`]: a module's bytes (ADR 0017 #8), built on
//!   [`Writer`] and [`Reader`], which other sections' encodings use too.
//! - [`Artifact`]: the container that holds a compiled library's sections.
//!
//! These types are the distribution format's own. Run-format bytecode is a
//! separate, private format (ADR 0005), so the two can change apart.

mod artifact;
mod bounds;
mod bytes;
mod encode;
mod hash;
mod ir;
mod primitive;
mod print;

mod validate;

pub use artifact::{Artifact, FORMAT_VERSION, KNOWN_FEATURES, MAGIC, Section};
pub use bytes::{DecodeError, Reader, Writer};
pub use encode::{decode, encode};
pub use hash::{Digest, Hasher, fingerprint};

pub use bounds::{Bounds, bounds};
pub use ir::{
    Arity, Constant, ConstantId, FunId, Function, Global, GlobalId, GlobalLink, Inlined, JoinId,
    Module, Operand, Position, Term, TermId, Value, Var,
};
pub use primitive::{Effects, Primitive};
pub use validate::{
    MAX_CODE_WORDS, MAX_CONSTANTS, MAX_COUNT, MAX_POOL, MAX_SLOT, ValidationError, validate,
};
