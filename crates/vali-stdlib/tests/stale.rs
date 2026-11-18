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

#[test]
fn a_damaged_prebuilt_artifact_is_compiled_from_source_instead() {
    // (scheme write) damaged; (scheme base) as built.
    let base = vali_stdlib::ARTIFACTS
        .iter()
        .find(|(identity, _)| *identity == "vali:scheme/base")
        .copied()
        .expect("(scheme base) is prebuilt");
    let artifacts: &'static [(&'static str, &'static [u8])] = Box::leak(Box::new([
        base,
        ("vali:scheme/write", &b"\0VALI\0A\nnot really"[..]),
    ]));
    vali_compiler::prebuilt::install(artifacts);
    let program = compile_program(
        "(import (scheme base) (scheme write))
         (let ((p (open-output-string))) (write 'ok p) (get-output-string p))",
        &Identity::new("main"),
        &NoLibraries,
    )
    .unwrap();
    let loaded: Vec<&str> = program.loaded.iter().map(Identity::as_str).collect();
    assert_eq!(loaded, ["vali:scheme/base"]);
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "\"ok\"");
}
