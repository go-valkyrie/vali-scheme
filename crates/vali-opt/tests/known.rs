//! Calls to known procedures, as the IR of compiled programs shows them.

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
    let mut ir =
        vali_compiler::compile_internal_program(source, &Identity::new("test"), &NoLibraries)
            .unwrap_or_else(|e| panic!("{source}\n{e}"))
            .ir
            .expect("every module is lowered");
    vali_opt::optimise(&mut ir);
    ir.to_string()
}

#[test]
fn calls_to_known_procedures_are_direct() {
    // `fib` calls itself directly, and reads no global to do it.
    let fib = ir("(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 30)");
    assert!(
        fib.contains(
            "fun f1 fib (v0) []:
  let v1 = < v0 c0 @0:1:20
  if v1:
    return v0
  else:
    let v2 = - v0 c1 @0:1:38
    let v3 = call direct f1(v2) @0:1:33
    let v4 = - v0 c0 @0:1:52
    let v5 = call direct f1(v4) @0:1:47
    let v6 = + v3 v5 @0:1:30
    return v6
"
        ),
        "{fib}"
    );
    // A call in tail position stays one.
    assert!(fib.contains("  tail direct f1(c2) @0:1:64\n"), "{fib}");
    // Another procedure of the module is known too; this one is recursive,
    // so it is not inlined.
    let other = ir("(define (down n) (if (= n 0) 0 (down (- n 1)))) (define (f y) (+ (down y) 1))");
    assert!(
        other.contains("fun f2 f (v0) []:\n  let v1 = call direct f1(v0)"),
        "{other}"
    );
}

#[test]
fn procedures_that_may_change_are_not_known() {
    // Assigned again: the call goes through the global.
    let assigned = ir("(define (g) 1) (define (f) (g)) (set! g (lambda () 2))");
    assert!(!assigned.contains("direct"), "{assigned}");
    // Defined after code that could call it, which must find it unbound;
    // `f`, defined before, is known.
    let late = ir("(define (f) (g)) (define x (f)) (define (g) 1)");
    assert!(
        late.contains("fun f1 f () []:\n  let v0 = global g0 @0:1:12\n  tail v0() @0:1:12\n"),
        "{late}"
    );
    // Called with the wrong number of arguments, which must raise.
    let wrong = ir("(define (f x) x) (define (h) (f 1 2))");
    assert!(!wrong.contains("direct"), "{wrong}");
    // A closure with free variables is not one function's.
    let closed = ir("(define f (let ((n 1)) (lambda () n))) (define (h) (f))");
    assert!(!closed.contains("direct"), "{closed}");
}

#[test]
fn optional_and_rest_arguments_are_direct_when_accepted() {
    let rest = ir("(define (f x . r) r) (define (h) (f 1 2 3))");
    assert!(rest.contains("tail direct f1(c1 c2 c3)"), "{rest}");
    let none = ir("(define (f x . r) r) (define (h) (f))");
    assert!(!none.contains("direct"), "{none}");
}
