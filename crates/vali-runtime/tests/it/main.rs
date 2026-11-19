//! vali-runtime's integration tests, one module per area. They build as
//! one binary so the runtime and its dependencies are linked once, not
//! once per file. Select an area with its module: `--test it vm::`.
//!
//! `tests/debugger.rs` stays a binary of its own: it needs the
//! `native-frames` feature and runs itself under a debugger.

mod handles;
mod isolate;
mod linking;
mod objects;
mod ownership;
mod primitives;
mod printer;
mod read;
mod rust_objects;
mod stack;
mod syntax;
mod vm;
