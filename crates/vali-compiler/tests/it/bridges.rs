//! Rust bridges (ADR 0016): a native library's functions and methods,
//! called from Scheme.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source};
use vali_runtime::bridge::{Bytes, Rest};
use vali_runtime::{
    Cx, DeferredHeadered, Error, Handle, HeapConfig, NativeLibrary, NoCollection, Obj,
    PreciseHeadered, RustObject, Strategy,
};

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

/// Serves `(test bridges)` as a native library.
struct Bridges;

impl Loader for Bridges {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((name.to_string() == "(test bridges)").then(|| {
            Source::native(
                Identity::new("native:test/bridges"),
                std::sync::Arc::new(library()),
            )
        }))
    }
}

/// Runs `source`, a program body that sees `(scheme base)` and `(test
/// bridges)`, under strategy `S`.
fn run_under<S: Strategy>(source: &str) -> Result<String, Error> {
    let program = vali_compiler::compile_program(
        &format!("(import (scheme base) (test bridges)) {source}"),
        &Identity::new("test"),
        &Bridges,
    )
    .unwrap_or_else(|e| panic!("{source}\n{e}"));
    let mut cx = Cx::<S>::new(&HeapConfig::default());
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

/// A parameter whose conversion panics on `#f`, to test what a panic while
/// converting leaves behind.
struct Touchy(i64);

impl vali_runtime::bridge::FromScheme for Touchy {
    const EXPECTED: &'static str = "an exact integer";

    unsafe fn from_scheme(
        cx: &mut dyn vali_runtime::bridge::Context,
        v: vali_runtime::Value,
    ) -> Option<Self> {
        assert!(v != vali_runtime::Value::FALSE, "touchy about #f");
        // SAFETY: passed on to the caller.
        unsafe { i64::from_scheme(cx, v) }.map(Touchy)
    }
}

/// A Rust object and its bridge, written with the macros.
#[vali_runtime::object]
struct Greeter {
    greeting: String,
}

#[vali_runtime::bridge]
impl Greeter {
    #[scheme(name = "make-greeter")]
    fn new(greeting: &str) -> Greeter {
        Greeter {
            greeting: greeting.to_owned(),
        }
    }

    fn greet(&self, name: &str) -> String {
        format!("{}, {name}", self.greeting)
    }

    #[scheme(name = "greeter-set!")]
    fn set(&mut self, greeting: &str) {
        self.greeting = greeting.to_owned();
    }

    fn byte_sum(bytes: &[u8]) -> u64 {
        bytes.iter().map(|&b| u64::from(b)).sum()
    }

    #[scheme(name = "same-greeting?")]
    fn same(&self, other: &Greeter) -> bool {
        self.greeting == other.greeting
    }

    #[scheme(name = "copy-greeting!")]
    fn copy_from(&mut self, other: &Greeter) {
        self.greeting = other.greeting.clone();
    }

    /// Converting `by` happens after `self` is borrowed.
    #[scheme(name = "greeter-repeat!")]
    fn repeat(&mut self, by: Touchy) -> usize {
        self.greeting = self.greeting.repeat(by.0 as usize);
        self.greeting.len()
    }

    #[scheme(expansion_safe, version = 2)]
    fn shout(text: &str) -> String {
        text.to_uppercase()
    }

    #[scheme(skip)]
    #[allow(dead_code)]
    fn helper(&self) -> usize {
        self.greeting.len()
    }
}

fn greeters() -> NativeLibrary {
    Greeter::bridge(NativeLibrary::new(&["test", "greeters"]))
}

#[test]
fn the_macros_write_the_bridges() {
    let run = |source: &str| -> Result<String, String> {
        let program = vali_compiler::compile_program(
            &format!("(import (scheme base) (test greeters)) {source}"),
            &Identity::new("test"),
            &Greeters,
        )
        .map_err(|e| e.message)?;
        let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
        let id = vali_compiler::load(&mut cx, program).map_err(|e| e.to_string())?;
        let result = cx.run(id).map_err(|e| e.to_string())?;
        cx.write_string(&result).map_err(|e| e.to_string())
    };
    assert_eq!(
        run("(define g (make-greeter \"hello\"))
             (define h (make-greeter \"hi\"))
             (greeter-set! g \"hi\")
             (list (greet g \"you\") (byte-sum (bytevector 1 2 3)) (same-greeting? g h)
                   (same-greeting? g g) (shout \"abc\") g)")
        .as_deref(),
        Ok("(\"hi, you\" 6 #t #t \"ABC\" #<greeter>)")
    );
    // Lending `g` as `&mut` and as `&` at once is refused, not undefined.
    let error = run("(define g (make-greeter \"x\")) (copy-greeting! g g)").unwrap_err();
    assert!(error.contains("greeter is already borrowed"), "{error}");
    let error = run("(greet 5 \"x\")").unwrap_err();
    assert!(
        error.contains("greet: argument 1: expected greeter, got 5"),
        "{error}"
    );
    let error = run("(greet (make-greeter \"x\") 'y)").unwrap_err();
    assert!(
        error.contains("greet: argument 2: expected a string, got y"),
        "{error}"
    );
    let error = run("(define g (make-greeter \"x\")) (helper g)").unwrap_err();
    assert!(error.contains("helper"), "{error}");
    // A panic converting an argument after the receiver is borrowed releases
    // the borrow: the greeter is usable afterwards.
    assert_eq!(
        run("(define g (make-greeter \"ab\"))
             (guard (e (#t #f)) (greeter-repeat! g #f))
             (greeter-repeat! g 2)")
        .as_deref(),
        Ok("4")
    );
}

/// Serves `(test greeters)` as a native library.
struct Greeters;

impl Loader for Greeters {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((name.to_string() == "(test greeters)").then(|| {
            Source::native(
                Identity::new("native:test/greeters"),
                std::sync::Arc::new(greeters()),
            )
        }))
    }
}

#[test]
fn expansion_safe_functions_run_at_expansion_time() {
    let compile = |source: &str| {
        vali_compiler::compile_program(source, &Identity::new("main"), &Greeters)
            .map_err(|e| e.message)
    };
    let program = compile(
        "(import (scheme base) (test greeters))
         (define-syntax m
           (lambda (stx)
             (syntax-case stx () ((k) (datum->syntax (syntax k) (shout \"made at expansion\"))))))
         (m)",
    )
    .unwrap();
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "\"MADE AT EXPANSION\"");
    let error = compile(
        "(import (scheme base) (test greeters))
         (define-syntax m (lambda (stx) (greet (make-greeter \"x\") \"y\") (syntax 1)))
         (m)",
    )
    .unwrap_err();
    assert!(
        error.contains("make-greeter: cannot be called at expansion time"),
        "{error}"
    );
}
