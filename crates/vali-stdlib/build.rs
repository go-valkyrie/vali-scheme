//! Compiles Vali's standard libraries and writes their artifacts, and a
//! table of them for the crate to embed.
//!
//! One program imports every library, each under a prefix of its own so
//! their exports cannot clash, and is compiled with every library's
//! artifact written: all from one build, so each artifact's record of what
//! it imported matches the artifacts beside it.

use std::fmt::Write as _;
use std::path::PathBuf;

use vali_compiler::{
    Artifacts, Identity, LibraryName, LoadError, Loader, Source, compile_program_with,
    standard_libraries,
};

/// Serves nothing: every library compiled here is Vali's own.
struct NoLibraries;

impl Loader for NoLibraries {
    fn load(
        &self,
        _name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let mut program = String::from("(import");
    for (n, name) in standard_libraries().iter().enumerate() {
        write!(program, " (prefix {name} p{n}:)").expect("writing to a string");
    }
    program.push_str(")\n#t\n");
    let compiled = compile_program_with(
        &program,
        &Identity::new("vali-stdlib"),
        &NoLibraries,
        &Artifacts {
            write: true,
            ..Artifacts::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiling the standard libraries: {e}"));
    let mut table = String::from(
        "/// Each standard library's identity and artifact.\n\
         pub static ARTIFACTS: &[(&str, &[u8])] = &[\n",
    );
    for (n, (identity, bytes)) in compiled.artifacts.iter().enumerate() {
        let file = format!("{n}.vali");
        std::fs::write(out.join(&file), bytes).expect("writing an artifact");
        writeln!(
            table,
            "    ({:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{file}\"))),",
            identity.as_str()
        )
        .expect("writing to a string");
    }
    table.push_str("];\n");
    std::fs::write(out.join("artifacts.rs"), table).expect("writing the table");
    println!("cargo::rerun-if-changed=build.rs");
}
