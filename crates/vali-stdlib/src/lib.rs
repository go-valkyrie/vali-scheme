//! Vali's standard libraries, compiled when this crate is built.
//!
//! Its build script compiles every standard library written in Scheme with
//! `vali-compiler` and embeds their artifacts (ADR 0001 #5): their
//! interfaces, syntax exports and code as distribution code. [`install`]
//! hands them to the compiler, which then loads each from its artifact
//! instead of expanding it, so starting up skips expansion. Changing the
//! compiler or a library's source rebuilds them.

include!(concat!(env!("OUT_DIR"), "/artifacts.rs"));

/// Makes the compiler use these artifacts for Vali's own libraries. Call
/// it once before compiling; calling it again does nothing.
pub fn install() {
    vali_compiler::prebuilt::install(ARTIFACTS);
}
