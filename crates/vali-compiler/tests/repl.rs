//! The REPL: the interaction environment persisting from one input to the
//! next.

use vali_compiler::Repl;
use vali_runtime::{DeferredHeadered, HeapConfig, PreciseSideCounts, Strategy};

/// Runs `inputs` in order in a fresh REPL under strategy `S`, returning
/// what each printed, or its error.
fn session<S: Strategy>(inputs: &[&str]) -> Vec<Result<Vec<String>, String>> {
    let mut repl = Repl::<S>::new(&HeapConfig::default(), None).expect("a REPL");
    inputs
        .iter()
        .map(|input| repl.eval(input).map_err(|e| e.to_string()))
        .collect()
}

/// Checks a session under two strategies: each input's expected output,
/// or `Err` with part of its error message.
fn check(steps: &[(&str, Result<&[&str], &str>)]) {
    let inputs: Vec<&str> = steps.iter().map(|(input, _)| *input).collect();
    for results in [
        session::<DeferredHeadered>(&inputs),
        session::<PreciseSideCounts>(&inputs),
    ] {
        for ((input, expected), result) in steps.iter().zip(results) {
            match (expected, result) {
                (Ok(expected), Ok(shown)) => assert_eq!(&shown, expected, "{input}"),
                (Err(part), Err(message)) => {
                    assert!(message.contains(part), "{input}: {message}")
                }
                (_, result) => panic!("{input}: expected {expected:?}, got {result:?}"),
            }
        }
    }
}

#[test]
fn definitions_persist() {
    check(&[
        ("(define x 5)", Ok(&[])),
        ("(* x 2)", Ok(&["10"])),
        // Defining again replaces.
        ("(define x 7)", Ok(&[])),
        ("x", Ok(&["7"])),
        ("(set! x 9)", Ok(&[])),
        ("x", Ok(&["9"])),
        // A procedure can name one defined later.
        ("(define (f) (g))", Ok(&[])),
        ("(define (g) 'g)", Ok(&[])),
        ("(f)", Ok(&["g"])),
        // An error leaves what was defined.
        ("(car 1)", Err("car: expected a pair")),
        ("x", Ok(&["9"])),
        ("undefined-thing", Err("unbound variable: undefined-thing")),
    ]);
}

#[test]
fn redefining_imported_names() {
    check(&[
        ("(define (car x) 'mine)", Ok(&[])),
        ("(car '(1 2))", Ok(&["mine"])),
        // The REPL's car is passed where asked for...
        ("(map car '((1) (2)))", Ok(&["(mine mine)"])),
        // ...but the libraries keep their own.
        ("(cadr '(1 2))", Ok(&["2"])),
    ]);
}

#[test]
fn macros_persist() {
    check(&[
        (
            "(define-syntax twice (syntax-rules () ((_ e) (begin e e))))",
            Ok(&[]),
        ),
        ("(let ((n 0)) (twice (set! n (+ n 1))) n)", Ok(&["2"])),
        // A name defined as a macro can be defined again as a variable.
        ("(define twice 3)", Ok(&[])),
        ("twice", Ok(&["3"])),
    ]);
}

/// A library compiled after the REPL defined a macro keeps its own macros
/// apart from the REPL's: an input's unit is dropped once it is loaded, and
/// the library may get its number.
#[test]
fn macros_of_libraries_imported_later() {
    check(&[
        (
            "(define-syntax mine (syntax-rules () ((_) 'mine)))",
            Ok(&[]),
        ),
        ("(import (srfi 64))", Ok(&[])),
        ("(test-begin \"t\")", Ok(&[])),
        ("(test-assert (= 1 1))", Ok(&[])),
        ("(mine)", Ok(&["mine"])),
    ]);
}

#[test]
fn values_imports_and_several_data() {
    check(&[
        ("(values 1 2)", Ok(&["1", "2"])),
        ("1 \"two\" 'three", Ok(&["1", "\"two\"", "three"])),
        ("(iota 3)", Err("iota")),
        ("(import (srfi 1))", Ok(&[])),
        ("(iota 3)", Ok(&["(0 1 2)"])),
        // The REPL's own definitions shadow what it imports.
        ("(define (iota n) 'mine)", Ok(&[])),
        ("(iota 3)", Ok(&["mine"])),
    ]);
}

#[test]
fn incomplete_input() {
    for text in ["(define (f)", "\"abc", "'", "(f #|", "(a (b"] {
        assert!(Repl::<DeferredHeadered>::is_incomplete(text), "{text}");
    }
    for text in ["(f)", "(f))", "", "x"] {
        assert!(!Repl::<DeferredHeadered>::is_incomplete(text), "{text}");
    }
}

/// An input that fails to compile leaves nothing behind that a later
/// input trips over, whatever the later one expands to.
#[test]
fn inputs_after_a_failed_one() {
    check(&[
        ("(if)", Err("if: expected a test")),
        (
            "(let loop ((i 0)) (if (< i 3) (loop (+ i 1)) i))",
            Ok(&["3"]),
        ),
        (
            "(define-record-type point (make-point x) point? (x point-x))",
            Ok(&[]),
        ),
        ("(point-x (make-point 5))", Ok(&["5"])),
        ("((lambda (a b) (+ a b)) 1 2)", Ok(&["3"])),
    ]);
}
