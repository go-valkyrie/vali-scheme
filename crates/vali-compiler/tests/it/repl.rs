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

/// An input's local bindings are dropped with it, and nothing a later
/// input can reach goes with them.
#[test]
fn what_later_inputs_reach_survives_forgetting_locals() {
    check(&[
        // Top-level definitions, whatever they expand through.
        ("(define (inc x) (let ((y 1)) (+ x y)))", Ok(&[])),
        (
            "(define-record-type point (make-point x y) point? (x px) (y py))",
            Ok(&[]),
        ),
        ("(define-values (a b) (values 1 2))", Ok(&[])),
        (
            "(define-syntax def-pair
               (syntax-rules ()
                 ((_ n m v) (begin (define tmp v) (define n tmp) (define m tmp)))))",
            Ok(&[]),
        ),
        ("(def-pair p q 7)", Ok(&[])),
        // A macro whose helper is a top-level definition another macro
        // introduced: only that macro's output can name it.
        (
            "(define-syntax def-helper
               (syntax-rules ()
                 ((_ name) (begin (define (helper) 'helped)
                                  (define-syntax name
                                    (syntax-rules () ((_) (helper))))))))",
            Ok(&[]),
        ),
        ("(def-helper call-helper)", Ok(&[])),
        // Locals of other inputs come and go in between.
        (
            "(let loop ((i 0)) (if (< i 3) (loop (+ i 1)) i))",
            Ok(&["3"]),
        ),
        ("(inc 1)", Ok(&["2"])),
        ("(px (make-point 3 4))", Ok(&["3"])),
        ("(list a b p q)", Ok(&["(1 2 7 7)"])),
        ("(call-helper)", Ok(&["helped"])),
        // A macro defined in one input, used in a later one, with locals.
        (
            "(define-syntax my-or2
               (syntax-rules () ((_ a b) (let ((t a)) (if t t b)))))",
            Ok(&[]),
        ),
        ("(let ((t 5)) (my-or2 #f t))", Ok(&["5"])),
        (
            "(let-syntax ((m (syntax-rules () ((_ e) (let ((z 2)) (* z e)))))) (m 4))",
            Ok(&["8"]),
        ),
        (
            "(let ((z 10)) (let-syntax ((m (syntax-rules () ((_ e) (* z e))))) (m 4)))",
            Ok(&["40"]),
        ),
        // Shadowing a REPL global locally, then using the global later.
        ("(define g 'global)", Ok(&[])),
        ("(let ((g 'local)) g)", Ok(&["local"])),
        ("g", Ok(&["global"])),
        ("(define (use-g) g)", Ok(&[])),
        ("(let ((g 1) (h 2)) (+ g h))", Ok(&["3"])),
        ("(use-g)", Ok(&["global"])),
        // Inputs that fail to compile or run leave nothing behind.
        ("(let ((w 1)) (car w))", Err("car: expected a pair")),
        ("(let ((w 1)) (if))", Err("if: expected a test")),
        ("(let ((w 1)) w)", Ok(&["1"])),
        ("w", Err("unbound variable: w")),
    ]);
}

/// The macro `m`'s transformer is wrapped in a `let-syntax`, and its output
/// names that `let-syntax`'s macro `h`, which is bound under a binder's
/// scope: dropping an input's local macros would leave a later `(m 5)`
/// with an unbound `h`. Only local variables go.
const WRAPPED: &str = "(define-syntax m
  (let-syntax ((h (syntax-rules () ((_ x) (quote (helped x))))))
    (lambda (stx) (syntax-case stx () ((_ x) (syntax (h x)))))))";

#[test]
fn a_top_level_macro_keeps_the_local_macros_its_output_names() {
    check(&[
        (WRAPPED, Ok(&[])),
        ("(m 5)", Ok(&["(helped 5)"])),
        ("(let ((x 1)) (m x))", Ok(&["(helped x)"])),
        ("(m 6)", Ok(&["(helped 6)"])),
    ]);
}

/// Serves `(test helped)`, whose exported macro is built like `WRAPPED`.
struct Helped;

impl vali_compiler::Loader for Helped {
    fn load(
        &self,
        name: &vali_compiler::LibraryName,
        _importer: Option<&vali_compiler::Identity>,
    ) -> Result<Option<vali_compiler::Source>, vali_compiler::LoadError> {
        Ok((name.to_string() == "(test helped)").then(|| {
            vali_compiler::Source::scheme(
                vali_compiler::Identity::new("lib:test/helped"),
                format!(
                    "(define-library (test helped)
                       (import (scheme base))
                       (export m)
                       (begin {WRAPPED}))"
                ),
            )
        }))
    }
}

/// A library first imported in an input brings its macros' local helpers
/// into the interaction environment's table, under binder scopes made while
/// that input compiled.
#[test]
fn a_library_first_imported_in_an_input_keeps_its_local_macros() {
    let loader = std::sync::Arc::new(Helped);
    let mut repl =
        Repl::<DeferredHeadered>::new(&HeapConfig::default(), Some(loader)).expect("a REPL");
    let mut eval = |input: &str| repl.eval(input).map_err(|e| e.to_string());
    assert_eq!(eval("(import (test helped))"), Ok(vec![]));
    assert_eq!(eval("(m 5)"), Ok(vec!["(helped 5)".to_owned()]));
    assert_eq!(
        eval("(let ((x 2)) (m x))"),
        Ok(vec!["(helped x)".to_owned()])
    );
}
