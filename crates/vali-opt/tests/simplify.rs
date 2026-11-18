//! Simplification, as the IR of compiled programs shows it.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source};

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

/// The printed, optimised IR of `source`'s body.
fn ir(source: &str) -> String {
    vali_compiler::compile_internal_program(source, &Identity::new("test"), &NoLibraries)
        .unwrap_or_else(|e| panic!("{source}\n{e}"))
        .ir
        .expect("every module is lowered")
        .to_string()
}

/// Asserts that the optimised IR of `source` has `expected`.
#[track_caller]
fn check(source: &str, expected: &str) {
    let ir = ir(source);
    assert!(ir.contains(expected), "{source}\n{ir}");
}

#[test]
fn constants_fold() {
    // (+ 1 (* 2 3)) is 7, and the constants it no longer needs are gone.
    let ir = ir("(define (f) (+ 1 (* 2 3)))");
    assert!(ir.contains("c0 = #<unspecified>\nc1 = 7\ng0"), "{ir}");
    assert!(ir.contains("fun f1 f () []:\n  return c1\n"), "{ir}");
    check(
        "(define (q) (car (cdr '(1 2 3))))",
        "fun f1 q () []:\n  return c1\n",
    );
}

#[test]
fn ifs_on_constants_keep_one_arm() {
    check(
        "(define (g x) (if (null? '()) x 0))",
        "fun f1 g (v0) []:\n  return v0\n",
    );
    // The join point the `if`'s value went to is just its handler now.
    check(
        "(define (k x) (+ 1 (if (< 1 2) x 0)))",
        "fun f1 k (v0) []:\n  let v3 = + c0 v0 @0:1:14\n  return v3\n",
    );
}

#[test]
fn unused_values_go_unless_they_may_raise() {
    // An unused allocation goes; a copy is replaced by what it copies.
    check(
        "(define (h x) (let ((y x) (z (cons x x))) y))",
        "fun f1 h (v0) []:\n  return v0\n",
    );
    // car may raise, so it stays though its value is unused.
    check(
        "(define (e x) (car x) (vector-length '#(1 2 3)))",
        "fun f1 e (v0) []:\n  let v1 = car v0 @0:1:14\n  return c1\n",
    );
}
