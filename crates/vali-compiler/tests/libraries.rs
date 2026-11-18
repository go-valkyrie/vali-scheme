//! Libraries: defining, importing, linking, and loading them.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use vali_compiler::{
    AsyncLoader, Identity, LibraryName, LoadError, Loader, Source, compile_program,
    compile_program_async,
};
use vali_runtime::{Cx, Error, HeapConfig, NoCollection};

/// Serves libraries from a list of names and texts.
struct Map(Vec<(&'static str, &'static str)>);

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
            .map(|(n, text)| Source::scheme(Identity::new(&format!("lib:{n}")), *text)))
    }
}

/// Compiles and runs `source` with `loader`, returning what it writes.
fn run_with(source: &str, loader: &impl Loader) -> Result<String, String> {
    let program = compile_program(source, &Identity::new("main"), loader).map_err(|e| e.message)?;
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().map_err(|e| e.to_string())?;
    let id = vali_compiler::load(&mut cx, program).map_err(|e| e.to_string())?;
    let result = cx.run(id).map_err(|e: Error| e.to_string())?;
    cx.write_string(&result).map_err(|e| e.to_string())
}

fn run(source: &str) -> Result<String, String> {
    run_with(source, &Map(Vec::new()))
}

fn check(cases: &[(&str, &str)]) {
    for (source, expected) in cases {
        match run(source) {
            Ok(result) => assert_eq!(&result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
}

/// What every test library imports.
const BASE: &str = "(scheme base)";

#[test]
fn a_program_imports_its_own_library() {
    check(&[(
        &format!(
            "(define-library (greet)
               (import {BASE})
               (export greeting (rename shout loud))
               (begin
                 (define greeting \"hello\")
                 (define (shout s) (string-append s \"!\"))))
             (import {BASE} (greet))
             (loud greeting)"
        ),
        "\"hello!\"",
    )]);
}

#[test]
fn import_sets() {
    let library = format!(
        "(define-library (nums)
           (import {BASE})
           (export one two three)
           (begin (define one 1) (define two 2) (define three 3)))"
    );
    check(&[
        (
            &format!("{library} (import {BASE} (only (nums) one two)) (+ one two)"),
            "3",
        ),
        (
            &format!("{library} (import {BASE} (prefix (nums) n:)) (list n:one n:three)"),
            "(1 3)",
        ),
        (
            &format!("{library} (import {BASE} (rename (nums) (one uno))) uno"),
            "1",
        ),
        (
            &format!("{library} (import {BASE} (except (nums) one)) (+ two three)"),
            "5",
        ),
    ]);
    let error = run(&format!("{library} (import {BASE} (only (nums) one)) two")).unwrap_err();
    assert!(
        error.contains("two is not bound: is it imported?"),
        "{error}"
    );
    let error = run(&format!("{library} (import {BASE} (only (nums) four)) 1")).unwrap_err();
    assert!(error.contains("(nums) does not export four"), "{error}");
}

#[test]
fn an_import_sees_its_librarys_assignments() {
    check(&[(
        &format!(
            "(define-library (counter)
               (import {BASE})
               (export count bump!)
               (begin
                 (define count 0)
                 (define (bump!) (set! count (+ count 1)))))
             (import {BASE} (counter))
             (bump!) (bump!)
             count"
        ),
        "2",
    )]);
}

#[test]
fn imports_cannot_be_assigned_or_redefined() {
    let library = format!("(define-library (c) (import {BASE}) (export x) (begin (define x 1)))");
    let error = run(&format!("{library} (import {BASE} (c)) (set! x 2)")).unwrap_err();
    assert!(
        error.contains("x is imported, and cannot be assigned"),
        "{error}"
    );
    let error = run(&format!("{library} (import {BASE} (c)) (define x 2)")).unwrap_err();
    assert!(
        error.contains("x is imported, and cannot be redefined"),
        "{error}"
    );
    let error = run(&format!("(import {BASE}) (set! car 1)")).unwrap_err();
    assert!(
        error.contains("car is imported, and cannot be assigned"),
        "{error}"
    );
}

#[test]
fn exported_macros_are_hygienic() {
    // The macro uses a helper the library does not export, and the user's
    // own `helper` doesn't capture it.
    check(&[(
        &format!(
            "(define-library (twice)
               (import {BASE})
               (export twice)
               (begin
                 (define (helper f x) (f (f x)))
                 (define-syntax twice
                   (syntax-rules () ((_ f x) (helper f x))))))
             (import {BASE} (twice))
             (define (helper . args) 'wrong)
             (twice (lambda (n) (* n 3)) 2)"
        ),
        "18",
    )]);
}

#[test]
fn exported_procedural_macros() {
    // The regression: a library exports a macro that computes its
    // expansion.
    check(&[(
        &format!(
            "(define-library (regression exported-macros)
               (export count-args)
               (import {BASE})
               (begin
                 (define-syntax count-args
                   (lambda (stx)
                     (syntax-case stx ()
                       ((_ arg ...)
                        (datum->syntax stx (length (syntax->datum #'(arg ...))))))))))
             (import {BASE} (regression exported-macros))
             (list (count-args a b c) (count-args))"
        ),
        "(3 0)",
    )]);
}

#[test]
fn transformers_use_imported_procedures_at_phase_1() {
    check(&[(
        &format!(
            "(define-library (helpers)
               (import {BASE})
               (export triple)
               (begin (define (triple n) (* 3 n))))
             (import {BASE} (helpers))
             (define-syntax tripled
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ n) (datum->syntax stx (triple (syntax->datum #'n)))))))
             (list (tripled 5) (triple 2))"
        ),
        "(15 6)",
    )]);
}

#[test]
fn each_phase_has_its_own_instance() {
    // The macro bumps the counter at phase 1 while expanding; the program's
    // run-time instance starts from zero regardless (ADR 0001 #2).
    check(&[(
        &format!(
            "(define-library (counter)
               (import {BASE})
               (export count bump!)
               (begin
                 (define count 0)
                 (define (bump!) (set! count (+ count 1)) count)))
             (import {BASE} (counter))
             (define-syntax expansion-count
               (lambda (stx) (datum->syntax stx (bump!))))
             (list (expansion-count) (expansion-count) count)"
        ),
        "(1 2 0)",
    )]);
}

#[test]
fn a_loader_serves_libraries() {
    let loader = Map(vec![
        (
            "(math square)",
            "(define-library (math square)
               (import (except (scheme base) square))
               (export square)
               (begin (define (square x) (* x x))))",
        ),
        (
            "(math sum-squares)",
            "(define-library (math sum-squares)
               (import (except (scheme base) square) (math square))
               (export sum-squares)
               (begin (define (sum-squares a b) (+ (square a) (square b)))))",
        ),
    ]);
    let result = run_with(
        "(import (scheme base) (math sum-squares)) (sum-squares 3 4)",
        &loader,
    );
    assert_eq!(result.unwrap(), "25");
    let error = run_with("(import (math cube)) 1", &loader).unwrap_err();
    assert_eq!(
        error,
        "library (math cube) not found, imported by the program"
    );
}

/// Only Vali's own libraries may import `(vali primitive)`: not a program,
/// a library a host serves, or an `eval` environment, and a host cannot
/// pass a library off as Vali's own by naming its identity so.
#[test]
fn only_valis_own_libraries_reach_the_primitives() {
    let internal = "(vali primitive) is internal to Vali";
    let error = run("(import (vali primitive)) (%raise 1)").unwrap_err();
    assert!(error.contains(internal), "{error}");
    let loader = Map(vec![(
        "(sneaky)",
        "(define-library (sneaky)
           (import (scheme base) (only (vali primitive) %raise))
           (export %raise))",
    )]);
    let error = run_with("(import (sneaky)) (%raise 1)", &loader).unwrap_err();
    assert!(error.contains(internal), "{error}");
    let error = run("(import (scheme base) (scheme eval))
                     (eval '(%raise 1) (environment '(only (vali primitive) %raise)))")
    .unwrap_err();
    assert!(error.contains(internal), "{error}");
    // A program with no imports sees (scheme base), and nothing else.
    let error = run("(%raise 1)").unwrap_err();
    assert!(error.contains("%raise is not bound"), "{error}");
}

/// Serves libraries asynchronously, waiting once per load.
struct Slow(Map);

impl AsyncLoader for Slow {
    fn load(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = Result<Option<Source>, LoadError>> + Send {
        let result = Loader::load(&self.0, name, importer);
        let mut waited = false;
        std::future::poll_fn(move |cx| {
            if waited {
                Poll::Ready(result.clone())
            } else {
                waited = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
    }
}

#[test]
fn an_async_loader_serves_libraries() {
    let loader = Slow(Map(vec![(
        "(answer)",
        "(define-library (answer) (import (vali core)) (export answer) (begin (define answer 42)))",
    )]));
    let main = Identity::new("main");
    let mut future = pin!(compile_program_async(
        "(import (answer)) answer",
        &main,
        &loader
    ));
    let program = loop {
        if let Poll::Ready(result) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            break result.unwrap();
        }
    };
    assert_eq!(program.libraries.len(), 1);
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "42");
}

#[test]
fn standard_libraries() {
    check(&[
        // (scheme cxr) defines its procedures in Scheme, so it is loaded
        // and linked at run time.
        (
            "(import (scheme base) (scheme cxr)) (list (caddr '(1 2 3)) (cdddr '(1 2 3 4)))",
            "(3 (4))",
        ),
        (
            "(import (scheme base) (scheme char)) (list (char-upcase #\\a) (string-upcase \"hi\"))",
            "(#\\A \"HI\")",
        ),
        // The derived forms come from (scheme base).
        (
            "(import (scheme base)) (let* ((a 1) (b (+ a 1))) (cond ((> b 1) 'big) (else 'small)))",
            "big",
        ),
    ]);
    // A program that imports (scheme base) explicitly sees only what it
    // imports, and cannot redefine it.
    let error = run("(import (scheme base)) (char-upcase #\\a)").unwrap_err();
    assert!(
        error.contains("char-upcase is not bound: is it imported?"),
        "{error}"
    );
    let error = run("(import (scheme base)) (define (car x) x)").unwrap_err();
    assert!(
        error.contains("car is imported, and cannot be redefined"),
        "{error}"
    );
    // error raises.
    let error = run("(error \"bad thing:\" 'x 42)").unwrap_err();
    assert!(error.contains("bad thing: x 42"), "{error}");
}
