//! Writes the fuzz targets' seed corpora from vali-stdlib's prebuilt
//! artifacts:
//!
//!     cargo +nightly run --example seeds
//!
//! `corpus/ir/` gets each artifact's `invoke` section, an IR module.
//! `corpus/library/` gets, for each artifact and each of its sections, the
//! library's number, the section's number and the section's bytes, which is
//! the input the `library` target reads. Nothing is committed: the
//! artifacts change with the compiler and the format.

use std::fs;
use std::path::Path;

use vali_dist::Artifact;

/// The sections a library's artifact has, as the `library` target numbers
/// them.
const SECTIONS: &[&str] = &["build", "source", "library", "syntax", "invoke", "depends"];

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let ir = root.join("ir");
    let library = root.join("library");
    for dir in [&ir, &library] {
        fs::create_dir_all(dir).expect("creating a corpus directory");
    }
    let (mut irs, mut libraries) = (0, 0);
    for (n, (identity, bytes)) in vali_stdlib::ARTIFACTS.iter().enumerate() {
        let artifact = Artifact::decode(bytes, SECTIONS)
            .unwrap_or_else(|e| panic!("decoding the artifact of {identity}: {e}"));
        for (s, name) in SECTIONS.iter().enumerate() {
            let Some(section) = artifact.section(name) else {
                continue;
            };
            let mut input = vec![n as u8, s as u8];
            input.extend_from_slice(section);
            fs::write(library.join(format!("{n}-{name}")), input).expect("writing a seed");
            libraries += 1;
            if *name == "invoke" {
                fs::write(ir.join(format!("{n}")), section).expect("writing a seed");
                irs += 1;
            }
        }
    }
    println!(
        "{irs} IR seeds, {libraries} library seeds, in {}",
        root.display()
    );
}
