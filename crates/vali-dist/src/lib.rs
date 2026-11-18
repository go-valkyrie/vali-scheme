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
//!
//! These types are the distribution format's own. Run-format bytecode is a
//! separate, private format (ADR 0005), so the two can change apart.

mod ir;
mod primitive;
mod print;
mod validate;

pub use ir::{
    Arity, Constant, ConstantId, FunId, Function, Global, GlobalId, GlobalLink, JoinId, Module,
    Operand, Position, Term, TermId, Value, Var,
};
pub use primitive::{Effects, Primitive};
pub use validate::{ValidationError, validate};
