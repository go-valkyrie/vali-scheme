//! Gives each build of the compiler a stamp of its own (ADR 0001 #8).
//!
//! An artifact is only reused by the build that wrote it, because what
//! the compiler writes depends on its source, and on the sources of the
//! crates beneath it: the reader, the optimiser, the format, the runtime
//! that expansion-time code runs on. The stamp is a hash of those crates'
//! source files and of the Scheme libraries Vali carries, so it differs
//! between builds whose output may differ, and is the same for identical
//! inputs on any machine: paths are hashed relative to the workspace and
//! in sorted order.

use std::path::{Path, PathBuf};

use vali_dist::Hasher;

/// The crates whose code or data decides what the compiler writes. A
/// crate that is not beside this one, as when the compiler is built from a
/// registry, is pinned by its version instead.
const CRATES: &[&str] = &[
    "vali-abi",
    "vali-bytecode",
    "vali-compiler",
    "vali-dist",
    "vali-macros",
    "vali-num",
    "vali-opt",
    "vali-runtime",
    "vali-syntax",
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let here = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let crates = here.parent().expect("the compiler is in a directory");
    let mut hasher = Hasher::new();
    for name in CRATES {
        let root = crates.join(name);
        let mut found = Vec::new();
        for part in ["src", "lib"] {
            files(&root.join(part), &mut found);
        }
        found.push(root.join("Cargo.toml"));
        found.sort();
        for path in found {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let relative = path.strip_prefix(crates).expect("under the crates");
            hasher.field(relative.to_string_lossy().replace('\\', "/").as_bytes());
            hasher.field(&bytes);
        }
        println!("cargo::rerun-if-changed=../{name}/src");
        println!("cargo::rerun-if-changed=../{name}/Cargo.toml");
    }
    println!("cargo::rerun-if-changed=lib");
    println!("cargo::rerun-if-changed=build.rs");
    println!(
        "cargo::rustc-env=VALI_COMPILER_SOURCE_HASH={}",
        &hasher.finish().hex()[..32]
    );
}
