//! Resource limits (ADR 0023): a cap raises an error Scheme code can
//! catch, and the host sees it typed when nothing does.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source, compile_program};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, Error, HeapConfig, Limit, NoCollection,
    PreciseHeadered, PreciseSideCounts, Strategy,
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

/// A program that makes things with `make` until it fails, counting them
/// in `n` and keeping what it made, so a freed object never makes room.
/// `guarded` wraps it in a handler that reports what it saw.
fn grower(make: &str, guarded: bool) -> String {
    let grow = format!("(grow (lambda (i) {make}))");
    let body = if guarded {
        format!("(guard (e (#t (list n (error-object? e) (error-object-message e)))) {grow})")
    } else {
        grow
    };
    format!(
        r#"(import (scheme base) (srfi 88))
(define n 0)
(define kept '())
(define context #'context)
(define (grow make)
  (let loop ((i 0))
    (set! kept (cons (make i) kept))
    (set! n (+ n 1))
    (loop (+ i 1))))
{body}"#
    )
}

const SYMBOL: &str = r#"(string->symbol (string-append "limit-test-" (number->string i)))"#;
const KEYWORD: &str = r#"(string->keyword (string-append "limit-test-" (number->string i)))"#;
const SYNTAX: &str = "(datum->syntax context i)";

fn load<S: Strategy>(source: &str, config: &HeapConfig) -> (Cx<S>, vali_runtime::ModuleId) {
    let program = compile_program(source, &Identity::new("limits"), &NoLibraries)
        .unwrap_or_else(|e| panic!("{source}\n{}", e.message));
    let mut cx = Cx::<S>::new(config);
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    (cx, id)
}

/// How many symbols and keywords, and runtime syntax objects, loading
/// `source` makes by itself.
fn base<S: Strategy>(source: &str) -> (usize, usize) {
    let (cx, _) = load::<S>(source, &HeapConfig::default());
    (cx.interned_symbols(), cx.live_syntaxes())
}

/// What running `make`'s grower does under a cap `extra` past what loading
/// it makes: what a handler saw, and what the host gets from it unhandled.
fn capped<S: Strategy>(make: &str, extra: usize, syntaxes: bool) -> (String, Error) {
    let (symbols, syntax_objects) = base::<S>(&grower(make, true));
    let config = if syntaxes {
        HeapConfig {
            max_syntaxes: Some(syntax_objects + extra),
            ..Default::default()
        }
    } else {
        HeapConfig {
            max_symbols: Some(symbols + extra),
            ..Default::default()
        }
    };
    let (mut cx, id) = load::<S>(&grower(make, true), &config);
    let caught = cx.run(id).and_then(|v| cx.write_string(&v)).unwrap();
    let (mut cx, id) = load::<S>(&grower(make, false), &config);
    let typed = cx.run(id).unwrap_err();
    (caught, typed)
}

macro_rules! under_every_strategy {
    ($f:ident) => {{
        $f::<NoCollection>();
        $f::<DeferredHeadered>();
        $f::<DeferredSideCounts>();
        $f::<PreciseHeadered>();
        $f::<PreciseSideCounts>();
    }};
}

fn symbol_cap<S: Strategy>() {
    let (caught, typed) = capped::<S>(SYMBOL, 3, false);
    assert_eq!(
        caught,
        "(3 #t \"resource limit reached: too many symbols\")",
        "{}",
        S::NAME
    );
    assert_eq!(typed, Error::LimitExceeded(Limit::Symbols), "{}", S::NAME);
}

#[test]
fn the_symbol_cap_is_catchable_and_typed() {
    under_every_strategy!(symbol_cap);
}

fn keyword_cap<S: Strategy>() {
    let (caught, typed) = capped::<S>(KEYWORD, 2, false);
    assert_eq!(
        caught,
        "(2 #t \"resource limit reached: too many symbols\")",
        "{}",
        S::NAME
    );
    assert_eq!(typed, Error::LimitExceeded(Limit::Symbols), "{}", S::NAME);
}

#[test]
fn the_keyword_cap_is_catchable_and_typed() {
    under_every_strategy!(keyword_cap);
}

fn syntax_cap<S: Strategy>() {
    // The program's own `#'context` is made when it runs, so it takes one
    // of the four.
    let (caught, typed) = capped::<S>(SYNTAX, 4, true);
    assert_eq!(
        caught,
        "(3 #t \"resource limit reached: too many syntax objects\")",
        "{}",
        S::NAME
    );
    assert_eq!(typed, Error::LimitExceeded(Limit::Syntaxes), "{}", S::NAME);
}

#[test]
fn the_syntax_cap_is_catchable_and_typed() {
    under_every_strategy!(syntax_cap);
}

/// Under the caps a program behaves as it does without them: the same
/// result, and the same symbols.
fn under_the_cap<S: Strategy>() {
    let source = r#"(import (scheme base))
(define (names n)
  (let loop ((i 0) (acc '()))
    (if (= i n)
        (reverse acc)
        (loop (+ i 1) (cons (string->symbol (string-append "under-" (number->string i))) acc)))))
(list (names 3) (eq? (string->symbol "under-1") (string->symbol "under-1")) (syntax->datum (datum->syntax #'x '(a b))))"#;
    let (symbols, syntaxes) = base::<S>(source);
    let config = HeapConfig {
        // Room for the three names the program makes, and the one it
        // quotes: hits on the existing ones cost nothing.
        max_symbols: Some(symbols + 3),
        max_syntaxes: Some(syntaxes + 3),
        ..Default::default()
    };
    let (mut cx, id) = load::<S>(source, &config);
    let limited = cx.run(id).and_then(|v| cx.write_string(&v)).unwrap();
    let (mut cx, id) = load::<S>(source, &HeapConfig::default());
    let unlimited = cx.run(id).and_then(|v| cx.write_string(&v)).unwrap();
    assert_eq!(limited, unlimited, "{}", S::NAME);
    assert_eq!(limited, "((under-0 under-1 under-2) #t (a b))");
}

#[test]
fn programs_under_the_caps_behave_as_before() {
    under_every_strategy!(under_the_cap);
}
