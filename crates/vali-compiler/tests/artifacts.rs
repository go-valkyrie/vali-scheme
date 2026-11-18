//! Library artifacts: written after compiling, and loaded in place of
//! compiling (ADRs 0001 #5 and 0012 #5).

use vali_compiler::{
    Artifacts, Identity, LibraryName, LoadError, Loader, Program, Source, compile_program_with,
};
use vali_runtime::{Cx, HeapConfig, NoCollection};

/// Serves libraries from a list of names and texts.
struct Map(Vec<(&'static str, String)>);

impl Loader for Map {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        let wanted = name.to_string();
        Ok(self
            .0
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(n, text)| Source::scheme(Identity::new(&format!("lib:{n}")), text)))
    }
}

/// A library with a procedure, a `syntax-rules` macro that refers to a
/// private helper, and a `syntax-case` macro, whose transformer runs at
/// expansion time.
fn greet() -> Map {
    Map(vec![(
        "(test greet)",
        "(define-library (test greet)
           (import (scheme base))
           (export greet twice count-args)
           (begin
             (define (helper x) (string-append \"hi, \" x))
             (define (greet x) (helper x))
             (define-syntax twice
               (syntax-rules () ((_ e) (begin e e))))
             (define-syntax count-args
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ a ...) (datum->syntax stx (length (syntax->datum #'(a ...))))))))))"
            .to_owned(),
    )])
}

const PROGRAM: &str = "
    (import (scheme base) (test greet))
    (define n 0)
    (twice (set! n (+ n 1)))
    (list (greet \"you\") n (count-args 1 2 3) (let ((helper 5)) helper))";

fn compile(loader: &Map, artifacts: &Artifacts) -> Result<Program, String> {
    compile_program_with(PROGRAM, &Identity::new("main"), loader, artifacts).map_err(|e| e.message)
}

fn run(program: Program) -> String {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    cx.write_string(&result).unwrap()
}

/// Every library's artifact, from compiling the program from source.
fn written(loader: &Map) -> Vec<(Identity, Vec<u8>)> {
    let program = compile(
        loader,
        &Artifacts {
            write: true,
            ..Artifacts::default()
        },
    )
    .unwrap();
    program.artifacts
}

fn reading(artifacts: &[(Identity, Vec<u8>)]) -> Artifacts {
    Artifacts {
        read: artifacts
            .iter()
            .map(|(identity, bytes)| (identity.as_str().to_owned(), bytes.clone()))
            .collect(),
        write: false,
    }
}

#[test]
fn a_program_runs_the_same_on_artifacts() {
    let loader = greet();
    let expected = run(compile(&loader, &Artifacts::default()).unwrap());
    assert_eq!(expected, "(\"hi, you\" 2 3 5)");
    let artifacts = written(&loader);
    let names: Vec<&str> = artifacts.iter().map(|(i, _)| i.as_str()).collect();
    assert!(names.contains(&"lib:(test greet)"), "{names:?}");
    assert!(names.contains(&"vali:scheme/base"), "{names:?}");
    // Every library from its artifact, none expanded.
    assert_eq!(
        run(compile(&loader, &reading(&artifacts)).unwrap()),
        expected
    );
    // Writing again from what was loaded gives the same bytes.
    let again = compile(
        &loader,
        &Artifacts {
            write: true,
            ..reading(&artifacts)
        },
    )
    .unwrap()
    .artifacts;
    assert_eq!(again, artifacts);
}

#[test]
fn an_artifact_is_refused_when_what_it_relied_on_changed() {
    let library = |helper: &str| {
        Map(vec![
            (
                "(test helper)",
                format!(
                    "(define-library (test helper) (import (scheme base))
                       (export {helper}) (begin (define ({helper} x) (string-append x \"!\"))))"
                ),
            ),
            (
                "(test user)",
                "(define-library (test user) (import (scheme base) (test helper))
                   (export shout) (begin (define (shout x) (exclaim x))))"
                    .to_owned(),
            ),
        ])
    };
    let program = "(import (scheme base) (test user)) (shout \"hey\")";
    let compile = |loader: &Map, artifacts: &Artifacts| {
        compile_program_with(program, &Identity::new("main"), loader, artifacts)
            .map_err(|e| e.message)
    };
    let artifacts = compile(
        &library("exclaim"),
        &Artifacts {
            write: true,
            ..Artifacts::default()
        },
    )
    .unwrap()
    .artifacts;
    let user: Vec<_> = artifacts
        .into_iter()
        .filter(|(i, _)| i.as_str() == "lib:(test user)")
        .collect();
    // As it was, (test user) loads from its artifact.
    assert_eq!(
        run(compile(&library("exclaim"), &reading(&user)).unwrap()),
        "\"hey!\""
    );
    // (test helper) changed what it exports: (test user)'s artifact relied
    // on what it was, and is refused.
    let error = compile(&library("yell"), &reading(&user))
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("lib:(test helper) has changed"), "{error}");
}

#[test]
fn damaged_artifacts_are_refused_not_panicked_on() {
    let loader = greet();
    let artifacts = written(&loader);
    let (identity, bytes) = artifacts
        .iter()
        .find(|(i, _)| i.as_str() == "lib:(test greet)")
        .unwrap();
    let with = |bytes: Vec<u8>| {
        let mut set = artifacts.clone();
        set.retain(|(i, _)| i != identity);
        set.push((identity.clone(), bytes));
        compile(&loader, &reading(&set)).map(|_| ())
    };
    for end in [0, 8, bytes.len() / 2, bytes.len() - 1] {
        assert!(with(bytes[..end].to_vec()).is_err(), "{end} bytes");
    }
    // Flipped bits fail cleanly, or still decode to something valid.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..100 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mut flipped = bytes.clone();
        let at = (state as usize) % flipped.len();
        flipped[at] ^= 1 << (state >> 61);
        let _ = with(flipped);
    }
}
