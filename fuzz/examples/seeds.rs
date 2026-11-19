//! Writes the fuzz targets' seed corpora from vali-stdlib's prebuilt
//! artifacts, and from the artifacts of a small user library:
//!
//!     cargo +nightly run --example seeds --no-default-features
//!
//! `corpus/ir/` gets each artifact's `invoke` section, an IR module.
//! `corpus/library/` gets, for each artifact and each of its sections, the
//! mode and library number (the top bit is set for the user library), the
//! section's number and the section's bytes, which is the input the
//! `library` target reads. Nothing is committed: the artifacts change with
//! the compiler and the format.

use std::fs;
use std::path::Path;

use vali_dist::Artifact;
use vali_fuzz::{SECTIONS, user_library_artifacts};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let ir = root.join("ir");
    let library = root.join("library");
    for dir in [&ir, &library] {
        fs::create_dir_all(dir).expect("creating a corpus directory");
    }
    let stdlib: Vec<(String, Vec<u8>)> = vali_stdlib::ARTIFACTS
        .iter()
        .map(|(i, b)| ((*i).to_owned(), b.to_vec()))
        .collect();
    let (mut irs, mut libraries) = (0, 0);
    for (mode, tag, artifacts) in [
        (0u8, "std", stdlib),
        (0x80, "user", user_library_artifacts()),
    ] {
        for (n, (identity, bytes)) in artifacts.iter().enumerate() {
            let artifact = Artifact::decode(bytes, SECTIONS)
                .unwrap_or_else(|e| panic!("decoding the artifact of {identity}: {e}"));
            for (s, name) in SECTIONS.iter().enumerate() {
                let Some(section) = artifact.section(name) else {
                    continue;
                };
                let mut input = vec![mode | n as u8, s as u8];
                input.extend_from_slice(section);
                fs::write(library.join(format!("{tag}-{n}-{name}")), input)
                    .expect("writing a seed");
                libraries += 1;
                if *name == "invoke" {
                    fs::write(ir.join(format!("{tag}-{n}")), section).expect("writing a seed");
                    irs += 1;
                }
            }
        }
    }
    println!(
        "{irs} IR seeds, {libraries} library seeds, in {}",
        root.display()
    );
}
