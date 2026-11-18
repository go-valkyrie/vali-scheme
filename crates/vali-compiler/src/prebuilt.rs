//! Vali's own libraries, compiled ahead of time.
//!
//! The `vali-stdlib` crate compiles the standard libraries when it is built
//! and carries their artifacts (ADR 0001 #5). It cannot be a dependency of
//! this crate, which its build script uses, so a program that has it hands
//! the artifacts over with [`install`], once, before compiling anything.
//! From then on, a standard library is loaded from its artifact instead of
//! being expanded, unless a host serves its own version of it, or of a
//! library it was compiled against: then it is compiled from source.
//!
//! The import graph is read from the artifacts too: a standard library
//! with an artifact that fits is never read from its source, only its
//! `depends` section, which names what it imports.

use std::sync::OnceLock;

/// The artifacts: each library's identity, and its bytes.
pub type Prebuilt = &'static [(&'static str, &'static [u8])];

static PREBUILT: OnceLock<Prebuilt> = OnceLock::new();

/// Each installed artifact's `depends` section, as identities, in the
/// order of [`PREBUILT`]: read the first time it is asked for, once per
/// process; `None` if the artifact is not one this Vali can load.
static DEPENDS: OnceLock<Vec<OnceLock<Option<Vec<String>>>>> = OnceLock::new();

/// Makes `artifacts` the prebuilt standard libraries. Only the first call
/// counts; later ones are ignored, so every caller may make it.
pub fn install(artifacts: Prebuilt) {
    let _ = PREBUILT.set(artifacts);
}

/// Whether prebuilt libraries are installed.
pub fn installed() -> bool {
    PREBUILT.get().is_some()
}

/// The place of the standard library `identity` among the installed
/// artifacts, and its artifact.
fn find(identity: &str) -> Option<(usize, &'static [u8])> {
    PREBUILT
        .get()?
        .iter()
        .enumerate()
        .find(|(_, (i, _))| *i == identity)
        .map(|(n, (_, bytes))| (n, *bytes))
}

/// The artifact of the standard library `identity`, if one is installed.
pub(crate) fn artifact(identity: &str) -> Option<&'static [u8]> {
    find(identity).map(|(_, bytes)| bytes)
}

/// The identities of the libraries the standard library `identity` was
/// compiled against, from its artifact, if one is installed and this
/// build of Vali can load it.
pub(crate) fn depends(identity: &str) -> Option<&'static [String]> {
    let (n, bytes) = find(identity)?;
    let all = DEPENDS.get_or_init(|| {
        let count = PREBUILT.get().map_or(0, |p| p.len());
        (0..count).map(|_| OnceLock::new()).collect()
    });
    all.get(n)?
        .get_or_init(|| {
            crate::expand::decode_library_artifact(bytes)
                .ok()
                .map(|(_, needed)| {
                    needed
                        .into_iter()
                        .map(|(dependency, _)| dependency)
                        .collect()
                })
        })
        .as_deref()
}
