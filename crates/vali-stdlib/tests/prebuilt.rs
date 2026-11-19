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
fn the_import_graph_is_read_from_the_artifacts() {
    let program = compile(
        "(import (scheme base) (scheme write) (srfi 1) (scheme case-lambda) (scheme eval))
         (fold + 0 '(1 2 3))",
    );
    assert!(program.read.is_empty(), "read {:?}", program.read);
    assert_eq!(run(program), "6");
    // A library a host serves is read from its source, and only it.
    let program = compile(
        "(import (scheme base) (scheme process-context))
         (get-environment-variable \"HOME\")",
    );
    let read: Vec<&str> = program.read.iter().map(Identity::as_str).collect();
    assert_eq!(read, ["host:process-context"]);
}

/// The prebuilt standard libraries still win with a cache: they are
/// loaded from their artifacts and never stored in it, and only the
/// library a host served is.
#[test]
fn the_cache_holds_what_a_host_served_not_the_standard_libraries() {
    vali_stdlib::install();
    let dir = std::env::temp_dir().join(format!("vali-stdlib-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = vali_compiler::Cache::new(&dir);
    let source = "(import (scheme base) (srfi 1) (scheme process-context))
         (list (fold + 0 '(1 2 3)) (get-environment-variable \"HOME\"))";
    let cold = vali_compiler::compile_program_cached(source, &Identity::new("main"), &Host, &cache)
        .unwrap_or_else(|e| panic!("{e}"));
    let stored: Vec<&str> = cold.cache.stored.iter().map(Identity::as_str).collect();
    assert_eq!(stored, ["host:process-context"]);
    assert!(loaded(&cold).contains(&"vali:srfi/1"));
    assert_eq!(run(cold), "(6 \"granted\")");
    let warm = vali_compiler::compile_program_cached(source, &Identity::new("main"), &Host, &cache)
        .unwrap_or_else(|e| panic!("{e}"));
    let hits: Vec<&str> = warm.cache.hits.iter().map(Identity::as_str).collect();
    assert_eq!(hits, ["host:process-context"]);
    assert!(warm.cache.stored.is_empty());
    let loaded = loaded(&warm);
    assert!(loaded.contains(&"vali:scheme/base") && loaded.contains(&"vali:srfi/1"));
    assert_eq!(run(warm), "(6 \"granted\")");
    assert_eq!(cache.stats().unwrap().entries, 1);
    let _ = std::fs::remove_dir_all(&dir);
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
