//! Programs compiled with the prebuilt standard libraries installed.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Program, Source, compile_program};
use vali_runtime::{Cx, HeapConfig, NoCollection};

/// Serves `(scheme process-context)` itself, as a host granting it would,
/// and nothing else.
struct Host;

impl Loader for Host {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(
            (*name == LibraryName::new(["scheme", "process-context"])).then(|| {
                Source::scheme(
                    Identity::new("host:process-context"),
                    "(define-library (scheme process-context)
                       (import (scheme base))
                       (export get-environment-variable)
                       (begin (define (get-environment-variable name) \"granted\")))",
                )
            }),
        )
    }
}

fn compile(source: &str) -> Program {
    vali_stdlib::install();
    compile_program(source, &Identity::new("main"), &Host).unwrap_or_else(|e| panic!("{e}"))
}

fn run(program: Program) -> String {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    cx.write_string(&result).unwrap()
}

fn loaded(program: &Program) -> Vec<&str> {
    program.loaded.iter().map(Identity::as_str).collect()
}

#[test]
fn standard_libraries_load_from_their_artifacts() {
    let program = compile(
        "(import (scheme base) (scheme write) (srfi 1) (scheme case-lambda))
         (define f (case-lambda ((x) x) ((x y) (+ x y))))
         (list (fold + 0 '(1 2 3)) (f 1 2) (let-values (((a b) (values 1 2))) (+ a b)))",
    );
    let loaded = loaded(&program);
    for library in [
        "vali:scheme/base",
        "vali:scheme/write",
        "vali:srfi/1",
        "vali:scheme/case-lambda",
    ] {
        assert!(loaded.contains(&library), "{library} in {loaded:?}");
    }
    assert_eq!(run(program), "(6 3 3)");
}

#[test]
fn a_library_a_host_serves_is_compiled_from_its_source() {
    let program = compile(
        "(import (scheme base) (scheme process-context))
         (get-environment-variable \"HOME\")",
    );
    let loaded = loaded(&program);
    assert!(loaded.contains(&"vali:scheme/base"), "{loaded:?}");
    assert!(!loaded.contains(&"host:process-context"), "{loaded:?}");
    assert_eq!(run(program), "\"granted\"");
}
