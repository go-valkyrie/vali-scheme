//! A library's functions convert to bytecode when first called (ADR 0017
//! #2). With `VALI_EAGER` set, or `set_eager_conversion`, every function
//! converts as the library loads, which these tests also run under, and
//! then check that nothing is left for a first call.

use vali_bytecode::Converted;
use vali_compiler::{Identity, LibraryName, LoadError, Loader, Program, Source, compile_program};
use vali_runtime::{
    Cx, DeferredHeadered, Error, HeapConfig, ModuleId, NoCollection, PreciseHeadered, Strategy,
};

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

fn eager() -> bool {
    std::env::var_os("VALI_EAGER").is_some()
}

const LIBRARY: &str = "(define-library (test lazy)
  (import (scheme base))
  (export first second unused text capture resume boom adder)
  (begin
    (define (first x) (+ x 1))
    (define (second x) (* x 2))
    (define (unused x) (list x x x))
    (define (text) \"text\")
    (define (adder n) (lambda (x) (+ x n)))
    (define (boom x)
      (car x))
    (define saved #f)
    (define (capture)
      (let ((v (call/cc (lambda (k) (set! saved k) 1))))
        (+ v 0)))
    (define (resume v) (saved v))))";

const IMPORT: &str = "(import (scheme base) (test lazy))";

fn compile(program: &str) -> Program {
    let loader = Map(vec![("(test lazy)", LIBRARY)]);
    compile_program(
        &format!("{IMPORT} {program}"),
        &Identity::new("main"),
        &loader,
    )
    .unwrap()
}

/// The library `(test lazy)`'s module in `program`.
fn library_of(program: &Program) -> &Converted {
    &program
        .libraries
        .iter()
        .find(|(identity, _)| identity.as_str() == "lib:(test lazy)")
        .expect("the library is compiled")
        .1
}

struct Loaded<S: Strategy> {
    cx: Cx<S>,
    body: ModuleId,
    library: ModuleId,
    /// How many functions the library has.
    functions: usize,
}

fn load<S: Strategy>(program: &str) -> Loaded<S> {
    let program = compile(program);
    let functions = library_of(&program).module().functions.len();
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let body = vali_compiler::load(&mut cx, program).unwrap();
    let library = cx
        .library("lib:(test lazy)")
        .expect("the library is loaded");
    Loaded {
        cx,
        body,
        library,
        functions,
    }
}

impl<S: Strategy> Loaded<S> {
    fn run(&mut self) -> Result<String, Error> {
        let result = self.cx.run(self.body)?;
        Ok(self.cx.write_string(&result).unwrap())
    }

    fn converted(&self) -> usize {
        self.cx.converted_functions(self.library)
    }
}

fn first_call_converts<S: Strategy>() {
    let mut loaded = load::<S>("(list (first 1) (first 2) (second 5))");
    if eager() {
        assert_eq!(loaded.converted(), loaded.functions);
    } else {
        // Only the body has been converted: it ran as the library loaded.
        assert_eq!(loaded.converted(), 1);
        assert!(loaded.functions > 4);
    }
    assert_eq!(loaded.run().unwrap(), "(2 3 10)");
    if eager() {
        assert_eq!(loaded.converted(), loaded.functions);
    } else {
        // `first` and `second`, once each however often they were called;
        // `unused`, `text` and `capture` were never called, so never
        // converted.
        assert_eq!(loaded.converted(), 3);
    }
}

#[test]
fn a_function_converts_on_its_first_call() {
    first_call_converts::<NoCollection>();
    first_call_converts::<DeferredHeadered>();
    first_call_converts::<PreciseHeadered>();
}

fn wrong_arity<S: Strategy>() {
    let mut loaded = load::<S>("(first 1 2)");
    let error = loaded.run().unwrap_err().to_string();
    assert!(
        error.contains("first: wrong number of arguments"),
        "{error}"
    );
    if !eager() {
        // The call was refused before it converted anything.
        assert_eq!(loaded.converted(), 1);
    }
    // A correct call converts it, and works.
    let mut loaded = load::<S>("(first 1)");
    assert_eq!(loaded.run().unwrap(), "2");
}

#[test]
fn a_call_with_the_wrong_arity_converts_nothing() {
    wrong_arity::<NoCollection>();
    wrong_arity::<PreciseHeadered>();
}

/// The only way left to fail to convert a function is a converter bug, as
/// the IR validator refuses what conversion cannot do (ADR 0017 #13). A
/// module whose stubs misstate how many free variables a function has
/// stands in for one: `adder` makes a closure of a function that has one,
/// which the check of each converted function finds.
#[test]
fn an_error_at_the_first_call_names_the_function() {
    if eager() {
        return;
    }
    let mut program = compile("(adder 1)");
    let library = program
        .libraries
        .iter_mut()
        .find(|(identity, _)| identity.as_str() == "lib:(test lazy)")
        .unwrap();
    let Converted::Lazy(lazy) = &mut library.1 else {
        panic!("a library converts lazily");
    };
    for function in &mut lazy.module.functions {
        if function.free == 1 {
            function.free = 2;
        }
    }
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let body = vali_compiler::load(&mut cx, program).unwrap();
    let error = cx.run(body).unwrap_err().to_string();
    assert!(
        error.contains("adder: cannot convert to bytecode") && error.contains("free variables"),
        "{error}"
    );
    // The isolate is still usable.
    assert!(cx.run(body).is_err());
}

fn continuation_reenters_a_converted_function<S: Strategy>() {
    // `capture` seals its own frame into a continuation, which is entered
    // again after the function has returned.
    let program = "(define n 0)
         (define r (+ 100 (capture)))
         (set! n (+ n 1))
         (if (< n 3) (resume n))
         (list r n)";
    let mut loaded = load::<S>(program);
    assert_eq!(loaded.run().unwrap(), "(102 3)");
    if !eager() {
        // `capture`, `resume`, and the receiver `capture` gives `call/cc`.
        assert_eq!(loaded.converted(), 4);
    }
    // And again, on the converted code.
    assert_eq!(loaded.run().unwrap(), "(102 3)");
}

#[test]
fn a_continuation_enters_a_converted_function_again() {
    continuation_reenters_a_converted_function::<NoCollection>();
    continuation_reenters_a_converted_function::<DeferredHeadered>();
    continuation_reenters_a_converted_function::<PreciseHeadered>();
}

fn a_function_converts_inside_a_handler<S: Strategy>() {
    // `with-exception-handler` calls its handler in the dynamic context of
    // the raise, so a handler that is a library function converts there.
    let mut loaded = load::<S>(
        "(list (guard (e (#t (first 41))) (raise 'boom))
               (call-with-current-continuation (lambda (k) (second (first 1)))))",
    );
    assert_eq!(loaded.run().unwrap(), "(42 4)");
}

#[test]
fn a_handler_and_a_receiver_can_be_functions_not_yet_converted() {
    a_function_converts_inside_a_handler::<NoCollection>();
    a_function_converts_inside_a_handler::<PreciseHeadered>();
}

#[test]
fn a_debugger_sees_a_function_only_once_it_has_run() {
    let mut loaded = load::<NoCollection>("(first 1)");
    let first = loaded.cx.variable(loaded.library, "first").unwrap();
    let (slots, positions) = loaded.cx.debug_function(&first).unwrap();
    if eager() {
        assert!(slots > 0 && positions > 0);
        return;
    }
    // Not called yet: no frame size and no line table, as the extensions
    // read them, who never see a frame of it.
    assert_eq!((slots, positions), (0, 0));
    loaded.run().unwrap();
    let (slots, positions) = loaded.cx.debug_function(&first).unwrap();
    assert!(slots > 0, "{slots} slots");
    assert!(positions > 0, "{positions} positions");
    // The one that was not called still shows none.
    let unused = loaded.cx.variable(loaded.library, "unused").unwrap();
    assert_eq!(loaded.cx.debug_function(&unused), Some((0, 0)));
}

#[test]
fn a_backtrace_names_and_locates_a_function_converted_on_its_call() {
    let mut loaded = load::<NoCollection>("(boom 5)");
    let Error::Scheme(error) = loaded.run().unwrap_err() else {
        panic!("a Scheme error");
    };
    let frame = error
        .backtrace
        .iter()
        .find(|frame| frame.name == "boom")
        .unwrap_or_else(|| panic!("{:?}", error.backtrace));
    // The line table came with the conversion.
    let position = frame.position.as_ref().expect("a position");
    assert_eq!(position.source.as_ref(), "lib:(test lazy)");
}
