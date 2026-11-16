//! Vali's reader and source representation (ADR 0014).
//!
//! One Logos lexer and one error-tolerant parser emit events, which two
//! sinks consume: a lossless Rowan tree for tools, and a datum builder for
//! running code. This crate also holds spans, source identities, scope sets
//! and the syntax-object data structures.
