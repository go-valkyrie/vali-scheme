//! A prebuilt artifact that does not fit is set aside, and its library
//! compiled from source. A test binary of its own, since the prebuilt
//! libraries are installed once per process.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source, compile_program};
use vali_runtime::{Cx, HeapConfig, NoCollection};

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

fn prebuilt(identity: &str) -> &'static [u8] {
    vali_stdlib::ARTIFACTS
        .iter()
        .find(|(i, _)| *i == identity)
        .map(|(_, bytes)| *bytes)
        .unwrap_or_else(|| panic!("{identity} is prebuilt"))
}

/// `bytes`, an artifact, as if compiled against another version of each
/// library it imports: it reads well, but does not fit.
fn compiled_against_another(bytes: &[u8]) -> Vec<u8> {
    let sections = ["build", "source", "library", "syntax", "invoke", "depends"];
    let mut artifact = vali_dist::Artifact::decode(bytes, &sections).expect("an artifact");
    let mut depends = vali_dist::Reader::new(artifact.section("depends").expect("depends"));
    let mut changed = vali_dist::Writer::new();
    // What it imports, then what ran while it was expanded.
    for list in 0..2 {
        let n = depends.count(33).expect("a count");
        changed.usize(n);
        for _ in 0..n {
            changed.str(depends.str().expect("an identity"));
            let mut hash: [u8; 32] = depends.raw(32).expect("a hash").try_into().unwrap();
            if list == 0 {
                hash[0] ^= 1;
            }
            changed.raw(&hash);
        }
    }
    artifact.insert("depends", true, changed.finish());
    artifact.encode()
}

#[test]
fn a_prebuilt_artifact_that_does_not_fit_is_compiled_from_source_instead() {
    // (scheme base) as built; (scheme write) damaged, so the import graph
    // reads its source; (scheme cxr) readable, so the graph takes its
    // imports from it, but compiled against another (scheme base).
    let artifacts: &'static [(&'static str, &'static [u8])] = Box::leak(Box::new([
        ("vali:scheme/base", prebuilt("vali:scheme/base")),
        ("vali:scheme/write", &b"\0VALI\0A\nnot really"[..]),
        (
            "vali:scheme/cxr",
            Box::leak(compiled_against_another(prebuilt("vali:scheme/cxr")).into_boxed_slice()),
        ),
    ]));
    vali_compiler::prebuilt::install(artifacts);
    let program = compile_program(
        "(import (scheme base) (scheme write) (scheme cxr))
         (let ((p (open-output-string))) (write (caddr '(a b ok)) p) (get-output-string p))",
        &Identity::new("main"),
        &NoLibraries,
    )
    .unwrap();
    let loaded: Vec<&str> = program.loaded.iter().map(Identity::as_str).collect();
    assert_eq!(loaded, ["vali:scheme/base"]);
    let read: Vec<&str> = program.read.iter().map(Identity::as_str).collect();
    assert_eq!(read, ["vali:scheme/write"]);
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "\"ok\"");
}
