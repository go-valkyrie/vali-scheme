//! Compile time must grow in proportion to the size of the source: nested
//! binding forms, long quasiquotes and chains of libraries that re-export
//! each other each took time, and memory, quadratic or worse in their
//! size.
//!
//! Each test compiles a program at one size and at four times it. A linear
//! compiler takes about four times as long, a quadratic one sixteen, so a
//! ratio under seven passes the first and fails the second. Timing is noisy
//! on a loaded machine, so each size is the best of three runs, and a ratio
//! that fails is measured once more before the test does.

use std::time::{Duration, Instant};

use vali_compiler::{
    Identity, LibraryName, LoadError, Loader, Source, compile_internal_program, compile_program,
};

/// Only the standard libraries.
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

/// How much longer four times the size may take: linear is four, quadratic
/// sixteen.
const LIMIT: f64 = 7.0;

/// The best of three runs of `work`.
fn best_of_three(mut work: impl FnMut()) -> Duration {
    (0..3)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed()
        })
        .min()
        .expect("three runs")
}

/// Checks that `work` at size `4 * n` takes less than seven times what it
/// takes at `n`.
fn grows_linearly(what: &str, n: usize, work: impl Fn(usize)) {
    let measure = || {
        let small = best_of_three(|| work(n));
        let large = best_of_three(|| work(4 * n));
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.002);
        (ratio, small, large)
    };
    let (mut ratio, mut small, mut large) = measure();
    if ratio >= LIMIT {
        (ratio, small, large) = measure();
    }
    assert!(
        ratio < LIMIT,
        "{what}: {n} took {small:?} and {} took {large:?}, {ratio:.1} times as long",
        4 * n
    );
}

/// Compiles `source` as a program of its own, which imports `(scheme base)`.
fn compile_alone(source: &str) {
    compile_internal_program(source, &Identity::new("scaling"), &NoLibraries)
        .unwrap_or_else(|e| panic!("{}", e.message));
}

/// `n` levels of `open` before `inner`, each closed by `close`.
fn nest(n: usize, open: impl Fn(usize) -> String, inner: &str, close: &str) -> String {
    let mut source = String::from("(import (scheme base)) ");
    for i in 0..n {
        source.push_str(&open(i));
    }
    source.push_str(inner);
    source.push_str(&close.repeat(n));
    source
}

#[test]
fn nested_lets_compile_in_linear_time() {
    grows_linearly("nested lets", 4_000, |n| {
        compile_alone(&nest(n, |i| format!("(let ((x{i} {i})) "), "x0", ")"));
    });
}

#[test]
fn nested_lets_rebinding_one_name_compile_in_linear_time() {
    grows_linearly("nested lets of one name", 4_000, |n| {
        compile_alone(&nest(n, |_| "(let ((x 1)) ".to_owned(), "x", ")"));
    });
}

#[test]
fn nested_lambdas_compile_in_linear_time() {
    grows_linearly("nested lambdas", 4_000, |n| {
        compile_alone(&nest(n, |i| format!("((lambda (x{i}) "), "x0", ") 1)"));
    });
}

#[test]
fn nested_let_syntax_compiles_in_linear_time() {
    grows_linearly("nested let-syntax", 4_000, |n| {
        compile_alone(&nest(
            n,
            |i| format!("(let-syntax ((m{i} (syntax-rules () ((_ e) e)))) "),
            "1",
            ")",
        ));
    });
}

#[test]
fn nested_quasiquotes_compile_in_linear_time() {
    grows_linearly("nested quasiquotes", 1_000, |n| {
        let source = format!(
            "(import (scheme base)) (define y 1) {}",
            nest(n, |_| "`(1 ,@(list ".to_owned(), "y", "))")
                .trim_start_matches("(import (scheme base)) ")
        );
        compile_alone(&source);
    });
}

#[test]
fn a_long_quasiquote_compiles_in_linear_time() {
    grows_linearly("a long quasiquote", 5_000, |n| {
        let flat = "1 ".repeat(n);
        let mixed = "1 ,y ".repeat(n / 2);
        compile_alone(&format!(
            "(import (scheme base)) (define y 1) (list `({flat}) `({mixed}))"
        ));
    });
}

/// A chain of `n` libraries `(chain l0)` to `(chain l{n-1})`, each defining
/// a `syntax-rules` macro and a procedure and importing the one before it.
/// With `reexport`, each exports all of that one's exports as well as its
/// own.
struct Chain {
    reexport: bool,
}

impl Loader for Chain {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        let name = name.to_string();
        let Some(i) = name
            .strip_prefix("(chain l")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|i| i.parse::<usize>().ok())
        else {
            return Ok(None);
        };
        let exports: String = if self.reexport {
            (0..=i).map(|j| format!("m{j} p{j} ")).collect()
        } else {
            format!("m{i} p{i}")
        };
        let previous = if i > 0 {
            format!("(chain l{})", i - 1)
        } else {
            String::new()
        };
        let text = format!(
            "(define-library (chain l{i})
               (export {exports})
               (import (scheme base) {previous})
               (begin
                 (define-syntax m{i} (syntax-rules () ((_ x) (+ x {i}))))
                 (define (p{i} x) (m{i} x))))"
        );
        Ok(Some(Source::scheme(
            Identity::new(&format!("lib:chain-l{i}")),
            text,
        )))
    }
}

fn compile_chain(n: usize, reexport: bool) {
    let last = n - 1;
    compile_program(
        &format!("(import (scheme base) (chain l{last})) (p{last} 1)"),
        &Identity::new("main"),
        &Chain { reexport },
    )
    .unwrap_or_else(|e| panic!("{}", e.message));
}

#[test]
fn a_chain_of_libraries_compiles_in_linear_time() {
    grows_linearly("a chain of libraries", 40, |n| compile_chain(n, false));
}

#[test]
fn a_chain_of_libraries_that_reexport_compiles_in_linear_time() {
    // The exports of library k are k names long, so what has to be written
    // is quadratic in the chain's length whatever the compiler does; the
    // work on top of that is not.
    grows_linearly("a chain of re-exporting libraries", 40, |n| {
        compile_chain(n, true);
    });
}
