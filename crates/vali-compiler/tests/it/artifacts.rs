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

/// Run-format bytecode is validated where it loads, once per module, and
/// not again when the compiler converts it.
#[test]
fn a_module_is_validated_once_per_load() {
    let library = |name: &'static str| {
        (
            name,
            format!(
                "(define-library {name}
                   (import (scheme base))
                   (export f)
                   (begin (define (f) 1)))"
            ),
        )
    };
    let loader = Map(vec![library("(test one)"), library("(test two)")]);
    let compile = |source: &str, artifacts: &Artifacts| {
        let before = vali_bytecode::validations();
        let program =
            compile_program_with(source, &Identity::new("main"), &loader, artifacts).unwrap();
        (program, vali_bytecode::validations() - before)
    };
    let own = "(import (scheme base) (prefix (test one) a:) (prefix (test two) b:)) (a:f)";
    let (_, baseline) = compile(
        "(import (scheme base) (prefix (test one) a:)) (a:f)",
        &Artifacts::default(),
    );
    // Compiling from source loads some modules of its own, which are
    // validated as they do; converting one more library adds no validation
    // to that.
    let (program, from_source) = compile(
        own,
        &Artifacts {
            write: true,
            ..Artifacts::default()
        },
    );
    assert_eq!(from_source, baseline);
    let (from_artifacts, _) = compile(own, &reading(&program.artifacts));
    // Loading validates each library and the body, once each.
    for program in [program, from_artifacts] {
        let modules = program.libraries.len() + 1;
        let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
        let before = vali_bytecode::validations();
        vali_compiler::load(&mut cx, program).unwrap();
        assert_eq!(vali_bytecode::validations() - before, modules);
    }
}

/// A library made from source and the same one loaded from its artifact
/// have one fingerprint, taken from the IR and not the converted code, so
/// a program compiled one way links to libraries loaded the other.
#[test]
fn a_library_has_one_fingerprint_from_source_or_artifact() {
    let loader = greet();
    let from_source = compile(&loader, &Artifacts::default()).unwrap();
    let artifacts = written(&loader);
    let from_artifacts = compile(&loader, &reading(&artifacts)).unwrap();
    assert_eq!(from_source.libraries.len(), from_artifacts.libraries.len());
    for ((identity, a), (_, b)) in from_source.libraries.iter().zip(&from_artifacts.libraries) {
        // A native library, which has no IR, has none.
        let native = identity.as_str() == "vali:vali/ports";
        assert_eq!(a.digest.is_none(), native, "{identity:?}");
        assert_eq!(b.digest.is_none(), native, "{identity:?}");
        assert_eq!(a.fingerprint(), b.fingerprint(), "{identity:?}");
    }
    // One isolate takes both: the second finds each library loaded and
    // the same.
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    vali_compiler::load(&mut cx, from_source).unwrap();
    vali_compiler::load(&mut cx, from_artifacts).unwrap();
    // A changed library is another.
    let changed = Map(vec![(
        "(test greet)",
        "(define-library (test greet)
           (import (scheme base))
           (export greet twice count-args)
           (begin
             (define (greet x) x)
             (define-syntax twice (syntax-rules () ((_ e) (begin e e))))
             (define-syntax count-args (syntax-rules () ((_ a ...) 0)))))"
            .to_owned(),
    )]);
    let other = compile(&changed, &Artifacts::default()).unwrap();
    let greet_of = |p: &Program| {
        p.libraries
            .iter()
            .find(|(i, _)| i.as_str() == "lib:(test greet)")
            .map(|(_, m)| m.fingerprint())
    };
    assert_ne!(greet_of(&other), None);
    assert_ne!(
        greet_of(&other),
        greet_of(&compile(&loader, &Artifacts::default()).unwrap())
    );
}

/// A library whose constants and macros are deeper or longer than a test
/// thread's 2 MiB stack could recurse through: it compiles, is written as
/// an artifact, is read back and runs.
#[test]
fn deep_and_long_libraries_compile_and_round_trip() {
    let deep = |open: &str, close: &str, n: usize| format!("{}{}", open.repeat(n), close.repeat(n));
    let library = format!(
        "(define-library (test deep)
           (import (scheme base))
           (export deep-list deep-vector long-list nest-lists)
           (begin
             (define deep-list (quote {}))
             (define deep-vector (quote {}))
             (define long-list (quote ({})))
             (define-syntax nest-lists
               (syntax-rules () ((_) (quote {}))))))",
        deep("(", ")", 100_000),
        deep("#(", ")", 100_000),
        "1 ".repeat(300_000),
        deep("(", ")", 500),
    );
    let loader = Map(vec![("(test deep)", library)]);
    let program = "(import (scheme base) (test deep))
                   (list (length deep-list) (vector-length deep-vector)
                         (length long-list) (length (nest-lists)))";
    let compile = |artifacts: &Artifacts| {
        compile_program_with(program, &Identity::new("main"), &loader, artifacts)
            .map_err(|e| e.message)
            .unwrap()
    };
    eprintln!("STEP start {}", vali_bytecode::validations());
    let written = compile(&Artifacts {
        write: true,
        ..Artifacts::default()
    })
    .artifacts;
    let expected = "(1 1 300000 1)";
    assert_eq!(run(compile(&Artifacts::default())), expected);
    assert_eq!(run(compile(&reading(&written))), expected);
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

/// A library with one exported procedure, compiled with `body` as its
/// result.
fn answer(body: &str) -> Map {
    Map(vec![(
        "(test answer)",
        format!(
            "(define-library (test answer) (import (scheme base)) (export answer)
               (begin (define (answer) {body})))"
        ),
    )])
}

const ANSWER_PROGRAM: &str = "(import (scheme base) (test answer)) (answer)";

fn compile_answer(loader: &Map, artifacts: &Artifacts) -> Result<Program, String> {
    compile_program_with(ANSWER_PROGRAM, &Identity::new("main"), loader, artifacts)
        .map_err(|e| e.message)
}

/// The artifact of `identity` among `artifacts`, as they were written.
fn only(artifacts: Vec<(Identity, Vec<u8>)>, identity: &str) -> Vec<(Identity, Vec<u8>)> {
    artifacts
        .into_iter()
        .filter(|(i, _)| i.as_str() == identity)
        .collect()
}

fn writing() -> Artifacts {
    Artifacts {
        write: true,
        ..Artifacts::default()
    }
}

#[test]
fn an_artifact_is_refused_when_its_source_changed() {
    let old = compile_answer(&answer("'old"), &writing())
        .unwrap()
        .artifacts;
    let old = only(old, "lib:(test answer)");
    assert_eq!(
        run(compile_answer(&answer("'old"), &reading(&old)).unwrap()),
        "old"
    );
    // Same interface, new body: the old artifact must not run as 'old.
    let error = compile_answer(&answer("'new"), &reading(&old))
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("its source has changed"), "{error}");
}

/// Serves a library that includes a file, and the file.
struct Including {
    file: &'static str,
}

impl Loader for Including {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((name.to_string() == "(test answer)").then(|| {
            Source::scheme(
                Identity::new("lib:(test answer)"),
                "(define-library (test answer) (import (scheme base)) (export answer)
                   (include \"answer.scm\"))",
            )
        }))
    }

    fn include(&self, path: &str, _from: &Identity) -> Result<Option<String>, LoadError> {
        Ok((path == "answer.scm").then(|| self.file.to_owned()))
    }
}

#[test]
fn an_artifact_is_refused_when_a_file_it_includes_changed() {
    let compile = |file: &'static str, artifacts: &Artifacts| {
        compile_program_with(
            ANSWER_PROGRAM,
            &Identity::new("main"),
            &Including { file },
            artifacts,
        )
        .map_err(|e| e.message)
    };
    let old = compile("(define (answer) 'old)", &writing())
        .unwrap()
        .artifacts;
    let old = only(old, "lib:(test answer)");
    assert_eq!(
        run(compile("(define (answer) 'old)", &reading(&old)).unwrap()),
        "old"
    );
    let error = compile("(define (answer) 'new)", &reading(&old))
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("its source has changed"), "{error}");
}

/// The artifact of `(test answer)`, with its sections changed by `edit`.
fn tampered(edit: impl FnOnce(&mut vali_dist::Artifact)) -> Vec<(Identity, Vec<u8>)> {
    let old = compile_answer(&answer("'old"), &writing())
        .unwrap()
        .artifacts;
    let mut old = only(old, "lib:(test answer)");
    let known = ["build", "source", "library", "syntax", "invoke", "depends"];
    let mut artifact = vali_dist::Artifact::decode(&old[0].1, &known).unwrap();
    edit(&mut artifact);
    old[0].1 = artifact.encode();
    old
}

#[test]
fn an_artifact_from_another_build_is_refused() {
    let mut seen = String::new();
    let _ = tampered(|a| seen = String::from_utf8(a.section("build").unwrap().to_vec()).unwrap());
    // It names the formats and the ABI, and the compiler's own sources.
    assert!(
        seen.contains(&format!("format {}", vali_dist::FORMAT_VERSION)),
        "{seen}"
    );
    assert!(seen.contains(" abi "), "{seen}");
    assert!(seen.contains(" build "), "{seen}");
    for other in [
        seen.replace(" build ", " build 0"),
        seen.replace("abi ", "abi 9"),
        seen.replace("format ", "format 9"),
    ] {
        let artifacts = tampered(|a| a.insert("build", true, other.into_bytes()));
        let error = compile_answer(&answer("'old"), &reading(&artifacts))
            .map(|_| ())
            .unwrap_err();
        assert!(error.contains("another build of Vali"), "{error}");
    }
}

#[test]
fn an_artifact_whose_records_disagree_is_refused() {
    // No dependency records, though it imports (scheme base).
    let artifacts = tampered(|a| a.insert("depends", true, vec![0, 0]));
    let error = compile_answer(&answer("'old"), &reading(&artifacts))
        .map(|_| ())
        .unwrap_err();
    assert!(error.contains("different libraries"), "{error}");
}

/// A helper library, whose procedure a macro in `(test macro)` calls while
/// expanding, and one `(test plain)` calls only when it runs.
fn expansion(helper: &str) -> Map {
    Map(vec![
        (
            "(test helper)",
            format!(
                "(define-library (test helper) (import (scheme base)) (export five)
                   (begin (define (five) {helper})))"
            ),
        ),
        (
            "(test macro)",
            "(define-library (test macro) (import (scheme base) (test helper))
               (export five-now)
               (begin
                 (define-syntax five-now
                   (lambda (stx)
                     (syntax-case stx ()
                       ((_) (datum->syntax stx (five))))))))"
                .to_owned(),
        ),
        (
            "(test plain)",
            "(define-library (test plain) (import (scheme base) (test helper))
               (export five-later)
               (begin (define (five-later) (five))))"
                .to_owned(),
        ),
    ])
}

#[test]
fn an_artifact_is_refused_when_code_that_ran_at_expansion_changed() {
    let program = "(import (scheme base) (test macro) (test plain)) (list (five-now) (five-later))";
    let compile = |loader: &Map, artifacts: &Artifacts| {
        compile_program_with(program, &Identity::new("main"), loader, artifacts)
            .map_err(|e| e.message)
    };
    let written = compile(&expansion("5"), &writing()).unwrap().artifacts;
    for library in ["lib:(test macro)", "lib:(test plain)"] {
        let one = only(written.clone(), library);
        assert_eq!(
            run(compile(&expansion("5"), &reading(&one)).unwrap()),
            "(5 5)"
        );
    }
    // The helper's implementation changed, not its exports, so what an
    // importer observes of it is the same.
    let macro_artifact = only(written.clone(), "lib:(test macro)");
    let error = compile(&expansion("6"), &reading(&macro_artifact))
        .map(|_| ())
        .unwrap_err();
    assert!(
        error.contains("whose code ran while it was expanded, has changed"),
        "{error}"
    );
    // A library that only calls it when it runs is still good.
    let plain = only(written, "lib:(test plain)");
    assert_eq!(
        run(compile(&expansion("6"), &reading(&plain)).unwrap()),
        "(6 6)"
    );
}

/// A user library's artifacts, written and then read: the program must give
/// the same answer both ways.
fn round_trips(library: &str, name: &str, program: &str, expected: &str) {
    let loader = Map(vec![(
        match name {
            "(test probe)" => "(test probe)",
            _ => unreachable!("one library here"),
        },
        library.to_owned(),
    )]);
    let compile = |artifacts: &Artifacts| {
        compile_program_with(program, &Identity::new("main"), &loader, artifacts)
            .map_err(|e| e.message)
    };
    let mut written = compile(&writing()).unwrap();
    let artifacts = std::mem::take(&mut written.artifacts);
    assert_eq!(run(written), expected);
    let set = only(artifacts, &format!("lib:{name}"));
    assert_eq!(set.len(), 1);
    let read = compile(&reading(&set)).unwrap_or_else(|e| panic!("{name} from its artifact: {e}"));
    assert_eq!(run(read), expected);
}

#[test]
fn a_user_library_with_a_record_type_loads_from_its_artifact() {
    // `define-record-type` expands into calls of internal primitives in the
    // library's own code: at phase 0 here, and in a transformer below.
    round_trips(
        "(define-library (test probe) (import (scheme base)) (export px)
           (begin
             (define-record-type point (make-point x) point? (x point-x set-point-x!))
             (define (px) (let ((p (make-point 7))) (set-point-x! p 8) (list (point? p) (point-x p))))))",
        "(test probe)",
        "(import (scheme base) (test probe)) (px)",
        "(#t 8)",
    );
    round_trips(
        "(define-library (test probe) (import (scheme base)) (export probe-m)
           (begin
             (define-syntax probe-m
               (lambda (stx)
                 (define-record-type pt (mk-pt a) pt? (a pt-a))
                 (datum->syntax stx (pt-a (mk-pt 5)))))))",
        "(test probe)",
        "(import (scheme base) (test probe)) (probe-m)",
        "5",
    );
}

/// The `%` names in the bodies of the macros in `lib/`, other than what
/// each file defines itself: what a user's code can link by using them.
fn primitives_macros_emit() -> std::collections::BTreeSet<String> {
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "sld") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    files(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lib"),
        &mut paths,
    );
    let is_name_char = |c: char| !c.is_whitespace() && !"()'`,\"".contains(c);
    let names = |s: &str| -> Vec<String> {
        s.split(|c: char| !is_name_char(c))
            .filter(|t| t.starts_with('%') && t.len() > 1)
            .map(str::to_owned)
            .collect()
    };
    let texts: Vec<String> = paths
        .iter()
        .map(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|l| l.split(';').next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    // What any library in `lib/` defines, as a procedure or a macro: those
    // names are Scheme's, linked as variables of their libraries, not
    // primitives.
    let defined: std::collections::BTreeSet<String> = texts
        .iter()
        .flat_map(|text| {
            ["(define ", "(define-syntax "]
                .into_iter()
                .flat_map(|form| text.match_indices(form))
                .flat_map(|(at, m)| {
                    let rest = text[at + m.len()..].trim_start_matches('(');
                    names(rest.split(|c: char| !is_name_char(c)).next().unwrap_or(""))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let mut found = std::collections::BTreeSet::new();
    for text in &texts {
        for (at, _) in text.match_indices("(define-syntax ") {
            // The form, to its closing parenthesis.
            let mut depth = 0;
            let mut end = text.len();
            for (i, c) in text[at..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = at + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for name in names(&text[at..end]) {
                if !defined.contains(&name) {
                    found.insert(name);
                }
            }
        }
    }
    found
}

#[test]
fn vali_s_macros_emit_no_internal_primitive_that_user_libraries_may_not_link() {
    let allowed: std::collections::BTreeSet<String> = vali_compiler::MACRO_PRIMITIVES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let found = primitives_macros_emit();
    assert!(
        found.is_superset(&allowed),
        "MACRO_PRIMITIVES names primitives no macro emits: {:?}",
        allowed.difference(&found).collect::<Vec<_>>()
    );
    assert_eq!(
        found.difference(&allowed).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "macros in lib/ emit primitives that are not in MACRO_PRIMITIVES"
    );
}
