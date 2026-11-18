//! What an isolate has done, counted exactly, for benchmarks that compare
//! compilers and optimisations without the noise of timing.
//!
//! Counting costs an increment per instruction, so it is on only with the
//! `counters` feature; [`Cx::counters`](crate::Cx::counters) gives `None`
//! without it.

/// Counts since the isolate was made, or last reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Bytecode instructions executed.
    pub instructions: u64,
    /// Bytecode procedures entered, tail calls included.
    pub calls: u64,
    /// Procedures implemented in Rust called.
    pub native_calls: u64,
    /// Heap objects allocated.
    pub allocations: u64,
    /// Words those objects took.
    pub words: u64,
    /// The most slots a frame entered had.
    pub largest_frame: u64,
}
