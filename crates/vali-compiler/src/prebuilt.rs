//! Vali's own libraries, compiled ahead of time.
//!
//! The `vali-stdlib` crate compiles the standard libraries when it is built
//! and carries their artifacts (ADR 0001 #5). It cannot be a dependency of
//! this crate, which its build script uses, so a program that has it hands
//! the artifacts over with [`install`], once, before compiling anything.
//! From then on, a standard library is loaded from its artifact instead of
//! being expanded, unless a host serves its own version of it, or of a
//! library it was compiled against: then it is compiled from source.

use std::sync::OnceLock;

/// The artifacts: each library's identity, and its bytes.
pub type Prebuilt = &'static [(&'static str, &'static [u8])];

static PREBUILT: OnceLock<Prebuilt> = OnceLock::new();

/// Makes `artifacts` the prebuilt standard libraries. Only the first call
/// counts; later ones are ignored, so every caller may make it.
pub fn install(artifacts: Prebuilt) {
    let _ = PREBUILT.set(artifacts);
}

/// Whether prebuilt libraries are installed.
pub fn installed() -> bool {
    PREBUILT.get().is_some()
}

/// The artifact of the standard library `identity`, if one is installed.
pub(crate) fn artifact(identity: &str) -> Option<&'static [u8]> {
    PREBUILT
        .get()?
        .iter()
        .find(|(i, _)| *i == identity)
        .map(|(_, bytes)| *bytes)
}
