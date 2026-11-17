//! Rust bridges (ADR 0016): a native library's functions and methods,
//! called from Scheme.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source};
use vali_runtime::bridge::{Bytes, Rest};
use vali_runtime::{
    Cx, DeferredHeadered, Error, Handle, HeapConfig, NativeLibrary, NoCollection, Obj,
    PreciseHeadered, RustObject, Strategy,
};

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

struct Counter {
    n: i64,
}

impl RustObject for Counter {
    const NAME: &'static str = "counter";
}

impl Counter {
    fn new(start: i64) -> Counter {
        Counter { n: start }
    }

    fn add(&mut self, by: Option<i64>) -> i64 {
        self.n += by.unwrap_or(1);
        self.n
    }

    fn value(&self) -> i64 {
        self.n
    }
}

fn library() -> NativeLibrary {
    NativeLibrary::new(&["test", "bridges"])
        .function("make-counter", Counter::new)
        .method("counter-add!", Counter::add)
        .method("counter-value", Counter::value)
        .predicate::<Counter>("counter?")
        .function("sum", |xs: Rest<i64>| xs.0.iter().sum::<i64>())
        .function("greet", |name: String, times: Option<u8>| {
            name.repeat(usize::from(times.unwrap_or(1)))
        })
        .function("halve", |n: i64| {
            if n % 2 == 0 {
                Ok(n / 2)
            } else {
                Err(format!("{n} is odd"))
            }
        })
        .function("byte-count", |b: Bytes| b.0.len())
        .function("both", |a: Obj<Counter>, b: Obj<Counter>| vec![a, b])
        .function("echo", |xs: Vec<Handle>| xs)
        .function("boom", || -> i64 { panic!("kaboom") })
}

fn run_under<S: Strategy>(source: &str) -> Result<String, Error> {
    let program = vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries)
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.install_primitives()?;
    cx.define_globals(&library())?;
    let id = vali_compiler::load(&mut cx, program)?;
    let result = cx.run(id)?;
    cx.collect_garbage();
    cx.write_string(&result)
}

/// The same answer, or the same error, under every strategy.
fn run(source: &str) -> Result<String, String> {
    let show = |r: Result<String, Error>| r.map_err(|e| e.to_string());
    let result = show(run_under::<NoCollection>(source));
    assert_eq!(
        show(run_under::<DeferredHeadered>(source)),
        result,
        "{source}"
    );
    assert_eq!(
        show(run_under::<PreciseHeadered>(source)),
        result,
        "{source}"
    );
    result
}

#[test]
fn functions_and_methods() {
    let cases = [
        (
            "(define c (make-counter 10))
             (counter-add! c)
             (counter-add! c 5)
             (list (counter-value c) (counter? c) (counter? 5) c)",
            "(16 #t #f #<counter>)",
        ),
        (
            "(list (sum) (sum 1 2 3) (greet \"ab\") (greet \"ab\" 3) (halve 10) (byte-count (bytevector 1 2)))",
            "(0 6 \"ab\" \"ababab\" 5 2)",
        ),
        (
            "(define a (make-counter 1))
             (define b (make-counter 2))
             (let ((pair (both a b)))
               (list (eq? (car pair) a) (map counter-value pair)))",
            "(#t (1 2))",
        ),
        ("(echo (list 1 \"two\" 'three))", "(1 \"two\" three)"),
    ];
    for (source, expected) in cases {
        assert_eq!(run(source).as_deref(), Ok(expected), "{source}");
    }
}

#[test]
fn errors_name_the_procedure_and_argument() {
    let error = |source: &str| run(source).unwrap_err();
    assert!(error("(halve 3)").contains("halve: 3 is odd"));
    assert!(error("(make-counter \"x\")").contains(
        "make-counter: argument 1: expected an exact integer that fits in i64, got \"x\""
    ));
    assert!(
        error("(counter-value 5)").contains("counter-value: argument 1: expected counter, got 5")
    );
    assert!(
        error("(greet \"x\" 300)")
            .contains("greet: argument 2: expected an exact integer that fits in u8")
    );
    assert!(error("(make-counter)").contains("wrong number of arguments"));
    assert!(
        error("(both (make-counter 1) 2)").contains("both: argument 2: expected counter, got 2")
    );
    // A panic is an error, and the isolate goes on.
    assert!(error("(boom)").contains("boom: panicked: kaboom"));
    assert_eq!(
        run("(guard (e (#t (counter-value (make-counter 7)))) (boom))").as_deref(),
        Ok("7")
    );
}

/// Serves the native library as `(test bridges)`, and a Scheme library
/// that uses it as `(test wrapper)`.
struct Natives(std::sync::Arc<NativeLibrary>);

impl Loader for Natives {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(match name.to_string().as_str() {
            "(test bridges)" => Some(Source::native(
                Identity::new("native:test/bridges"),
                std::sync::Arc::clone(&self.0),
            )),
            "(test wrapper)" => Some(Source::scheme(
                Identity::new("lib:test/wrapper"),
                "(define-library (test wrapper)
                   (import (scheme base) (test bridges))
                   (export twice)
                   (begin (define (twice n) (counter-value (make-counter (* 2 n))))))",
            )),
            _ => None,
        })
    }
}

fn run_imported(source: &str) -> Result<String, String> {
    let loader = Natives(std::sync::Arc::new(library()));
    let program = vali_compiler::compile_program(source, &Identity::new("main"), &loader)
        .map_err(|e| e.message)?;
    let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
    cx.install_primitives().map_err(|e| e.to_string())?;
    let id = vali_compiler::load(&mut cx, program).map_err(|e| e.to_string())?;
    let result = cx.run(id).map_err(|e| e.to_string())?;
    cx.write_string(&result).map_err(|e| e.to_string())
}

#[test]
fn native_libraries_come_through_the_loader() {
    assert_eq!(
        run_imported(
            "(import (scheme base) (test bridges))
             (define c (make-counter 1))
             (counter-add! c 41)
             (list (counter-value c) (counter? c) (sum 1 2 3))"
        )
        .as_deref(),
        Ok("(42 #t 6)")
    );
    assert_eq!(
        run_imported("(import (scheme base) (prefix (test bridges) b:)) (b:sum 4 5)").as_deref(),
        Ok("9")
    );
    assert_eq!(
        run_imported("(import (scheme base) (test wrapper)) (twice 21)").as_deref(),
        Ok("42")
    );
    // Native functions cannot run at expansion time (ADR 0009 #17).
    let error = run_imported(
        "(import (scheme base) (test bridges))
         (define-syntax m (lambda (stx) (sum 1 2) (syntax 1)))
         (m)",
    )
    .unwrap_err();
    assert!(
        error.contains("sum: cannot be called at expansion time"),
        "{error}"
    );
}
