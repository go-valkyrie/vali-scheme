//! vali-compiler's integration tests, one module per area. They build as
//! one binary so the compiler and its dependencies are linked once, not
//! once per file. Select an area with its module: `--test it libraries::`.

mod artifacts;
mod bridges;
mod compile;
mod libraries;
mod limits;
mod lower;
mod repl;
