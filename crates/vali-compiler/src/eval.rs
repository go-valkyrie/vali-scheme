//! `eval` and `environment` (R7RS 6.12).
//!
//! A program that imports `(scheme eval)` keeps the expander that compiled
//! it, and loading it puts that expander in the isolate, with the loader
//! the host passes, if any. `eval` then expands only the new code, against
//! the libraries already compiled, fetching any it imports that are not
//! there yet. A Rust procedure cannot run Scheme code, so `%eval-prepare`
//! compiles and loads the code and returns thunks: the body of each library
//! newly loaded, then the code's own. `eval`, in Scheme, calls them in
//! order, the last in tail position.
//!
//! An environment is the import sets `environment` was given, checked when
//! it is made. Environments are immutable, as R7RS defines them: a
//! definition in evaluated code stays local to that evaluation.
//!
//! The interaction environment, `(scheme repl)`'s, is the one mutable
//! environment: it imports `(scheme base)`, and what is evaluated in it
//! can define, and define again, names every later evaluation sees, and
//! `(import set ...)` adds to its imports. Its definitions are REPL
//! globals (`expand::repl_global`).

use std::sync::Arc;

use vali_bytecode::Arity;
use vali_runtime::{Cx, Error, Handle, RustObject, Strategy, Value};
use vali_syntax::{SourceId, Span};

use crate::expand::Expander;
use crate::library::{self, ImportSet};
use crate::loader::{self, Identity, Loader};

/// What `eval` needs, kept in the isolate.
pub(crate) struct Evaluator {
    pub(crate) expander: Expander,
    /// The loader for libraries the program did not load.
    pub(crate) loader: Option<Arc<dyn Loader + Send + Sync>>,
    /// How many evaluations there have been, to name each one.
    count: usize,
    /// What the interaction environment imports.
    interaction: Vec<ImportSet>,
}

impl Evaluator {
    pub(crate) fn new(expander: Expander) -> Evaluator {
        Evaluator {
            expander,
            loader: None,
            count: 0,
            interaction: vec![ImportSet::Library(
                library::LibraryName::new(["scheme", "base"]),
                None,
            )],
        }
    }
}

/// An environment: the import sets `environment` was given, or the
/// interaction environment.
#[derive(Clone, Debug)]
pub(crate) enum Environment {
    Imports(Vec<ImportSet>),
    Interaction,
}

impl RustObject for Environment {
    const NAME: &'static str = "environment";
}

/// The procedures `(scheme eval)` is written over, which `(vali primitive)`
/// exports.
pub(crate) const EVAL_PROCEDURES: &[&str] =
    &["%environment", "%eval-prepare", "%interaction-environment"];

/// Binds the procedures `(scheme eval)` is written over in `cx`. They work
/// once a program that imports `(scheme eval)` is loaded.
pub(crate) fn install<S: Strategy>(cx: &mut Cx<S>) -> Result<(), Error> {
    cx.define_primitive(
        "%environment",
        Arity {
            required: 0,
            optional: 0,
            rest: true,
        },
        environment,
    )?;
    cx.define_primitive("%eval-prepare", Arity::exactly(2), eval_prepare)?;
    cx.define_primitive(
        "%interaction-environment",
        Arity::exactly(0),
        interaction_environment,
    )?;
    Ok(())
}

/// `(%interaction-environment)`: the isolate's interaction environment.
fn interaction_environment<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    let environment = cx.object(Environment::Interaction)?;
    cx.get(&environment)
}

fn nowhere() -> Span {
    Span {
        source: SourceId(0),
        range: Default::default(),
    }
}

/// `(%environment set ...)`: an environment of the import sets, as data.
fn environment<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut sets = Vec::with_capacity(args.len());
    for &arg in args {
        // SAFETY: arguments are valid values.
        let datum = unsafe { cx.value_datum(arg, nowhere()) }
            .ok_or_else(|| Error::scheme("environment: expected import sets"))?;
        let set = library::import_set(&datum)
            .map_err(|e| Error::scheme(format!("environment: {}", e.message)))?;
        sets.push(set);
    }
    let environment = cx.object(Environment::Imports(sets))?;
    cx.get(&environment)
}

/// `(%eval-prepare expr environment)`: compiles and loads `expr` in
/// `environment`, with any library it needs that is not loaded yet, and
/// returns the thunks to call in order: each new library's body, then
/// `expr`'s.
fn eval_prepare<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: arguments are valid values.
    let expr = unsafe { cx.value_datum(args[0], nowhere()) }
        .ok_or_else(|| Error::scheme("eval: expected an expression or definition as data"))?;
    // SAFETY: as above.
    let environment = unsafe { cx.root(args[1]) };
    let environment = cx
        .with_object(&environment, |e: &mut Environment| e.clone())
        .map_err(|_| Error::scheme("eval: expected an environment"))?;
    let Some(data) = cx.set_evaluator(None) else {
        return Err(Error::scheme(
            "eval: this isolate cannot evaluate code: load a program that imports (scheme eval)",
        ));
    };
    let mut evaluator = data
        .downcast::<Evaluator>()
        .map_err(|_| Error::scheme("eval: the isolate's evaluator is not Vali's"))?;
    let thunks = prepare(cx, &mut evaluator, expr, environment);
    cx.set_evaluator(Some(evaluator));
    let mut list = Handle::empty_list();
    for thunk in thunks?.iter().rev() {
        list = cx.cons(thunk, &list)?;
    }
    cx.get(&list)
}

/// A loader to fetch with: the host's, or none.
pub(crate) struct Fetcher<'a>(pub(crate) Option<&'a (dyn Loader + Send + Sync)>);

impl Loader for Fetcher<'_> {
    fn load(
        &self,
        name: &library::LibraryName,
        importer: Option<&Identity>,
    ) -> Result<Option<loader::Source>, loader::LoadError> {
        self.0.map_or(Ok(None), |l| l.load(name, importer))
    }

    fn include(&self, path: &str, from: &Identity) -> Result<Option<String>, loader::LoadError> {
        self.0.map_or(Ok(None), |l| l.include(path, from))
    }
}

/// The import sets of `(import set ...)`, if `expr` is that.
fn import_form(expr: &vali_syntax::Datum) -> Option<Result<Vec<ImportSet>, crate::CompileError>> {
    let vali_syntax::DatumKind::List { items, tail: None } = &expr.kind else {
        return None;
    };
    let (head, sets) = items.split_first()?;
    if !matches!(&head.kind, vali_syntax::DatumKind::Symbol(s) if s == "import") {
        return None;
    }
    Some(sets.iter().map(library::import_set).collect())
}

fn prepare<S: Strategy>(
    cx: &mut Cx<S>,
    evaluator: &mut Evaluator,
    expr: vali_syntax::Datum,
    environment: Environment,
) -> Result<Vec<Handle>, Error> {
    // Evaluated code has no source text, so spans would mean nothing.
    let compile_error = |e: crate::CompileError| Error::scheme(format!("eval: {}", e.message));
    let interactive = matches!(environment, Environment::Interaction);
    let mut body = vec![expr];
    let sets = match environment {
        Environment::Imports(sets) => sets,
        Environment::Interaction => {
            let mut sets = evaluator.interaction.clone();
            // `(import set ...)` adds to the interaction environment's
            // imports, once they compile.
            if let Some(added) = import_form(&body[0]) {
                sets.extend(added.map_err(compile_error)?);
                body.clear();
            }
            sets
        }
    };
    evaluator.count += 1;
    // The macros it runs are held to the caps the host set (ADR 0023).
    evaluator.expander.set_caps(&cx.config());
    let identity = Identity::new(&format!("eval:{}", evaluator.count));
    let program = loader::Program {
        libraries: Vec::new(),
        imports: sets.clone(),
        implicit: false,
        body: body.clone(),
        internal: false,
    };
    let fetcher = Fetcher(evaluator.loader.as_deref());
    let expander = &mut evaluator.expander;
    // Libraries compiled already are not fetched again, so their sources
    // are not read again either.
    let mut sources = std::mem::take(&mut expander.sources);
    // A `cond-expand` in the code, or in a library compiled for it, that
    // tests a library its source never names asks the loader then.
    let resolver: crate::expand::Resolver = match &evaluator.loader {
        Some(loader) => {
            let loader = Arc::clone(loader);
            Arc::new(move |name, importer| {
                matches!(
                    loader.load(name, Some(&Identity::new(importer))),
                    Ok(Some(_))
                )
            })
        }
        None => Arc::new(|_, _| false),
    };
    expander.set_resolver(Some(resolver));
    let graph = loader::fetch_beyond(
        &program,
        &identity,
        &fetcher,
        &mut sources,
        &|name| expander.has_library(name),
        &loader::Available::new(),
    );
    expander.sources = sources;
    let graph = graph.map_err(compile_error)?;
    expander.add_includes(graph.includes);
    // Answers hold for one evaluation: the loader may serve more
    // libraries by the next. A library compiled earlier still counts
    // (`library_answer`), and its recorded answers are only checked when
    // an artifact is loaded, which asks afresh.
    expander.reset_available();
    expander.add_available(graph.available);
    let mut names = vec![Vec::new(); graph.libraries.len()];
    for (name, i) in graph.by_name {
        names[i].push(name);
    }
    let mut thunks = Vec::new();
    for (library, names) in graph.libraries.iter().zip(names) {
        let library_identity = library.identity.as_str();
        // A library compiled before keeps its unit, under any new names.
        if let Some(unit) = expander.unit_of(library_identity) {
            for name in names {
                expander.name_unit(name, unit);
            }
            continue;
        }
        if let Some(native) = &library.native {
            expander.add_native(library_identity, Arc::clone(native));
            cx.register_native_library(library_identity, Arc::clone(native));
        }
        for name in names {
            expander.name_next(name);
        }
        let module = crate::compile_library(expander, library).map_err(compile_error)?;
        if cx.library(library_identity).is_none() {
            let id = cx.load_converted(module)?;
            cx.register_library(library_identity, id);
            thunks.push(cx.module_body(id)?);
        }
    }
    // What compiling the code adds to the expander is dropped once it is
    // loaded, or fails to compile, so evaluating in a loop does not grow
    // it. The libraries it fetched stay.
    let mark = expander.mark();
    // Evaluated code gets no optimisation (ADR 0013 #13).
    expander.optimise = false;
    let body = if interactive {
        expander.interaction(identity.as_str(), &sets, &body)
    } else {
        expander.program(identity.as_str(), &sets, false, &body, false)
    };
    expander.optimise = true;
    let loaded = body.map_err(compile_error).and_then(|body| {
        if crate::uses_syntax(&body) {
            crate::merge_syntax_data(cx, expander.syntax_data())?;
        }
        // The module is freed once nothing refers to it (#31). An
        // interaction environment's definitions are isolate globals, so
        // the closures they hold keep their modules alive until they are
        // defined again.
        cx.load_transient(body)
    });
    expander.forget(mark);
    let thunk = loaded?;
    if interactive {
        evaluator.interaction = sets;
    }
    thunks.push(thunk);
    Ok(thunks)
}

#[cfg(test)]
mod tests {
    use vali_runtime::{
        DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
        PreciseSideCounts,
    };

    use super::*;

    /// How many sources `expander` holds.
    fn sources(expander: &Expander) -> usize {
        (1..)
            .take_while(|&i| expander.sources.source(SourceId(i)).is_some())
            .count()
    }

    /// Evaluating in a loop leaves the expander as it was after the first
    /// evaluation: its units, variables, `lambda`s and sources do not grow,
    /// whatever the code defines, returns or fails to compile.
    #[test]
    fn evaluating_does_not_grow_the_expander() {
        let source = "
            (import (scheme base) (scheme eval) (scheme repl))
            (define env (environment '(scheme base)))
            (define (run n)
              (let loop ((i 0) (kept '()))
                (if (= i n)
                    (length kept)
                    (begin
                      (eval '(begin (define x 5) (when (> x 0) (* x 2))) env)
                      (eval '(define-syntax swap!
                               (syntax-rules () ((_ a b) (let ((t a)) (set! a b) (set! b t)))))
                            env)
                      (guard (e (#t #f)) (eval '(if) env))
                      (guard (e (#t #f)) (eval '(car 1) env))
                      (eval `(define counter ,i) (interaction-environment))
                      (eval '(define-syntax twice (syntax-rules () ((_ e) (begin e e))))
                            (interaction-environment))
                      (eval '(twice (set! counter (+ counter 1))) (interaction-environment))
                      (loop (+ i 1)
                            (cons (eval `(lambda (y) (+ y ,i)) env) kept))))))";
        let program = crate::compile_program(source, &Identity::new("leak"), &Fetcher(None))
            .expect("the program compiles");
        let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = crate::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let run = cx.variable(id, "run").unwrap();
        let measure = |cx: &mut Cx<DeferredHeadered>, n: i64| {
            cx.call(&run, &[Handle::fixnum(n).unwrap()]).unwrap();
            let data = cx.set_evaluator(None).unwrap();
            let evaluator = data.downcast::<Evaluator>().unwrap();
            let seen = (evaluator.expander.mark(), sources(&evaluator.expander));
            cx.set_evaluator(Some(evaluator));
            seen
        };
        let once = measure(&mut cx, 1);
        assert_eq!(measure(&mut cx, 50), once);
    }

    /// Deep inputs to the REPL leave no cache behind: the scope context's
    /// and the binding table's are dropped with each input, so they do not
    /// grow for the whole session.
    #[test]
    fn interaction_inputs_leave_no_caches() {
        let source = "
            (import (scheme base) (scheme eval) (scheme repl))
            (define (name i) (string->symbol (string-append \"x\" (number->string i))))
            (define (same depth form)
              (if (= depth 0)
                  form
                  (same (- depth 1) (list 'let '((y (+ y 1))) form))))
            (define (nest depth)
              (let loop ((i depth) (form (same depth (list '+ (name depth) 1))))
                (if (= i 0)
                    form
                    (loop (- i 1)
                          (list 'let (list (list (name i) (name (- i 1)))) form)))))
            (define (run n depth)
              (eval '(define x0 1) (interaction-environment))
              (eval '(define y 1) (interaction-environment))
              (do ((i 0 (+ i 1))) ((= i n))
                (eval (nest depth) (interaction-environment))))";
        let program = crate::compile_program(source, &Identity::new("caches"), &Fetcher(None))
            .expect("the program compiles");
        let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = crate::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let run = cx.variable(id, "run").unwrap();
        for n in [1, 5] {
            let args = [Handle::fixnum(n).unwrap(), Handle::fixnum(300).unwrap()];
            cx.call(&run, &args).unwrap();
            let data = cx.set_evaluator(None).unwrap();
            let mut evaluator = data.downcast::<Evaluator>().unwrap();
            let held = evaluator.expander.cache_len();
            cx.set_evaluator(Some(evaluator));
            assert_eq!(held, 0, "after {n} deep inputs");
        }
    }

    /// An input's local bindings leave the interaction environment's
    /// binding table with it, and only those: what it defines stays. The
    /// table holds what the inputs define, however many locals they bind.
    #[test]
    fn interaction_inputs_leave_no_local_bindings() {
        let source = "
            (import (scheme base) (scheme eval) (scheme read) (scheme repl))
            (define (run text n)
              (do ((i 0 (+ i 1))) ((= i n))
                (eval (read (open-input-string text)) (interaction-environment))))";
        let program = crate::compile_program(source, &Identity::new("locals"), &Fetcher(None))
            .expect("the program compiles");
        let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = crate::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let run = cx.variable(id, "run").unwrap();
        let mut sizes = Vec::new();
        let step = |cx: &mut Cx<DeferredHeadered>, text: &str, n: i64| {
            let text = cx.string(text).unwrap();
            cx.call(&run, &[text, Handle::fixnum(n).unwrap()]).unwrap();
            let data = cx.set_evaluator(None).unwrap();
            let evaluator = data.downcast::<Evaluator>().unwrap();
            let size = evaluator.expander.table_len();
            cx.set_evaluator(Some(evaluator));
            size
        };
        // One definition, then many inputs binding locals of every kind:
        // parameters, `let`, named `let`, internal definitions, and a
        // macro that binds a local of its own. (A local macro's entry
        // stays, since a top-level macro's output can name it.)
        step(
            &mut cx,
            "(define-syntax m (syntax-rules () ((_ e) (let ((t e)) (+ t 1)))))",
            1,
        );
        let defined = step(&mut cx, "(define (f x) (+ x 1))", 1);
        sizes.push(defined);
        let locals = "(let loop ((i 0) (acc '()))
                        (if (< i 3)
                            (let ()
                              (define (h a) (m a))
                              (loop (+ i 1) (cons (h i) acc)))
                            ((lambda (x . rest) (let* ((y x) (z y)) z)) 1 2)))";
        sizes.push(step(&mut cx, locals, 1));
        sizes.push(step(&mut cx, locals, 200));
        assert_eq!(sizes[1], sizes[0], "an input with locals adds no entries");
        assert_eq!(sizes[2], sizes[0], "and neither do many");
        // A failed input leaves none either.
        let failed = |cx: &mut Cx<DeferredHeadered>| {
            let text = cx.string("(let ((q 1)) (if))").unwrap();
            assert!(cx.call(&run, &[text, Handle::fixnum(1).unwrap()]).is_err());
        };
        failed(&mut cx);
        assert_eq!(step(&mut cx, "(define (g y) (+ y 2))", 1), sizes[0] + 1);
        // Definitions are what the table grows by.
        assert_eq!(step(&mut cx, "(define h 1)", 1), sizes[0] + 2);
    }

    /// What the tests below evaluate: each function does one kind of
    /// evaluation, in a loop of `n`, or keeps what one returns.
    const WORLD: &str = "
        (import (scheme base) (scheme eval))
        (define env (environment '(scheme base)))
        (define env2 (environment '(scheme base) '(scheme eval)))
        (define kept #f)
        (define (spin n)
          (let loop ((i 0) (s 0))
            (if (= i n) s (loop (+ i 1) (+ s (eval '(+ 1 2) env))))))
        ;; Closures that refer to their own module through its cells, or to
        ;; themselves: cycles, which only a collection frees.
        (define (cycles n)
          (let loop ((i 0))
            (if (= i n)
                n
                (begin
                  (eval '(begin (define (f) f) f) env)
                  (eval '(let () (define (g) g) (g)) env)
                  (eval '(begin (define x (list 1 2)) (define (h) x) (h)) env)
                  (loop (+ i 1))))))
        (define (errors n)
          (let loop ((i 0))
            (if (= i n)
                n
                (begin
                  (guard (e (#t #f)) (eval '(car 1) env))
                  (guard (e (#t #f)) (eval '(if) env))
                  (guard (e (#t #f)) (eval '(begin (define (f) (car 1)) (f)) env))
                  (loop (+ i 1))))))
        (define (nested n)
          (let loop ((i 0) (s 0))
            (if (= i n)
                s
                (loop (+ i 1)
                      (+ s (eval '(eval '(+ 1 2) (environment '(scheme base))) env2))))))
        (define (keep!) (set! kept (eval '(lambda (y) (+ y 1)) env)))
        (define (call-kept) (kept 5))
        (define (drop!) (set! kept #f))
        (define (keep-continuation!)
          (set! kept (eval '(car (list (call/cc (lambda (c) c)))) env)))
        ;; Re-enters a continuation captured in evaluated code after that
        ;; code returned, and after enough allocation for epochs to run.
        (define (reenter)
          (let ((v (eval '(car (list (call/cc (lambda (c) c)))) env)))
            (if (procedure? v)
                (begin (set! kept v) (spin 3000) (v 7))
                v)))
        (define (constants n)
          (let loop ((i 0) (s 0))
            (if (= i n)
                s
                (loop (+ i 1)
                      (+ s (length (eval '(list 'a \"str\" 3.5 '(1 2 3) #(1 2)) env)))))))";

    /// A world under strategy `S`: the cx, the program's module, and how
    /// many modules are loaded once the first evaluation has loaded what it
    /// imports.
    type World<S> = (Cx<S>, vali_runtime::ModuleId, usize);

    fn world<S: Strategy>() -> World<S> {
        let program = crate::compile_program(WORLD, &Identity::new("world"), &Fetcher(None))
            .expect("the program compiles");
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = crate::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let mut world = (cx, id, 0);
        call(&mut world, "spin", Some(1));
        world.0.collect_garbage();
        world.2 = world.0.loaded_modules();
        world
    }

    /// Calls `name` with `n`, if given, and shows the result.
    fn call<S: Strategy>(world: &mut World<S>, name: &str, n: Option<i64>) -> String {
        let (cx, id, _) = world;
        let f = cx.variable(*id, name).unwrap();
        let args: Vec<Handle> = n.map(|n| Handle::fixnum(n).unwrap()).into_iter().collect();
        let result = cx.call(&f, &args).unwrap();
        cx.display_string(&result).unwrap()
    }

    /// Modules loaded beyond the baseline, after collecting all garbage.
    fn leftover<S: Strategy>(world: &mut World<S>) -> usize {
        world.0.collect_garbage();
        world.0.loaded_modules() - world.2
    }

    /// Every evaluation under a strategy that counts: the module count
    /// stays bounded however many evaluations there are, and every module
    /// is freed once nothing refers to it, whether it was in a cycle, failed
    /// halfway, ran inside another evaluation or was entered again through a
    /// continuation.
    fn evaluated_modules_are_freed<S: Strategy>() {
        let mut w = world::<S>();
        // Epochs come with allocation, and a module counts as allocated, so
        // thousands of evaluations take only as many slots as an epoch
        // lets pile up.
        assert_eq!(call(&mut w, "spin", Some(20_000)), "60000");
        assert!(w.0.module_slots() < 3_000, "{} slots", w.0.module_slots());
        assert_eq!(leftover(&mut w), 0);

        assert_eq!(call(&mut w, "constants", Some(2_000)), "10000");
        assert_eq!(leftover(&mut w), 0);
        assert_eq!(call(&mut w, "cycles", Some(500)), "500");
        assert_eq!(leftover(&mut w), 0);
        assert_eq!(call(&mut w, "errors", Some(500)), "500");
        assert_eq!(leftover(&mut w), 0);
        assert_eq!(call(&mut w, "nested", Some(200)), "600");
        assert_eq!(leftover(&mut w), 0);

        // A module with a closure left is not freed, and the closure
        // works: its code and constants are there.
        call(&mut w, "keep!", None);
        assert_eq!(leftover(&mut w), 1);
        assert_eq!(call(&mut w, "spin", Some(3_000)), "9000");
        assert_eq!(leftover(&mut w), 1);
        assert_eq!(call(&mut w, "call-kept", None), "6");
        // Once nothing refers to the closure, the module goes.
        call(&mut w, "drop!", None);
        assert_eq!(leftover(&mut w), 0);

        // The same for the frames a continuation holds.
        call(&mut w, "keep-continuation!", None);
        assert_eq!(call(&mut w, "spin", Some(3_000)), "9000");
        assert_eq!(leftover(&mut w), 1);
        call(&mut w, "drop!", None);
        assert_eq!(leftover(&mut w), 0);
        assert_eq!(call(&mut w, "reenter", None), "7");
        call(&mut w, "drop!", None);
        assert_eq!(leftover(&mut w), 0);
    }

    #[test]
    fn evaluated_modules_are_freed_under_every_counting_strategy() {
        evaluated_modules_are_freed::<DeferredHeadered>();
        evaluated_modules_are_freed::<DeferredSideCounts>();
        evaluated_modules_are_freed::<PreciseHeadered>();
        evaluated_modules_are_freed::<PreciseSideCounts>();
    }

    /// A constant outlives its module when something refers to it: the
    /// value an evaluation returns is still there, and still read-only,
    /// after its module is freed.
    #[test]
    fn constants_outlive_their_modules() {
        let source = "
            (import (scheme base) (scheme eval))
            (define env (environment '(scheme base)))
            (define (get) (eval '(list 'a \"str\" (quote (1 2 3)) #(4 5)) env))
            (define (churn n)
              (let loop ((i 0)) (if (= i n) n (begin (eval '(+ 1 2) env) (loop (+ i 1))))))
            (define (try) (guard (e (#t 'immutable)) (set-car! (car (cdr (cdr (get)))) 9)))";
        let program = crate::compile_program(source, &Identity::new("c"), &Fetcher(None)).unwrap();
        let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = crate::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let get = cx.variable(id, "get").unwrap();
        let kept = cx.call(&get, &[]).unwrap();
        let churn = cx.variable(id, "churn").unwrap();
        cx.call(&churn, &[Handle::fixnum(3_000).unwrap()]).unwrap();
        cx.collect_garbage();
        assert_eq!(cx.display_string(&kept).unwrap(), "(a str (1 2 3) #(4 5))");
        let try_ = cx.variable(id, "try").unwrap();
        let result = cx.call(&try_, &[]).unwrap();
        assert_eq!(cx.display_string(&result).unwrap(), "immutable");
    }

    /// Under a strategy that never frees, nothing can say when nothing
    /// refers to a module, so evaluated modules stay loaded (#31): the
    /// documented cost of that strategy.
    #[test]
    fn evaluated_modules_stay_loaded_without_collection() {
        let mut w = world::<NoCollection>();
        call(&mut w, "spin", Some(100));
        w.0.collect_garbage();
        assert_eq!(w.0.loaded_modules() - w.2, 100);
    }

    /// REPL inputs are freed too: those that define nothing once they have
    /// run, and the ones a redefinition replaces once nothing else refers
    /// to their closures. A long run of either keeps the modules loaded
    /// bounded.
    fn repl_inputs_are_freed<S: Strategy>() {
        let mut repl = crate::Repl::<S>::new(&HeapConfig::default(), None).unwrap();
        let loaded = |repl: &mut crate::Repl<S>| {
            repl.isolate().with(|cx| {
                cx.collect_garbage();
                cx.loaded_modules()
            })
        };
        repl.eval("(define (f x) (+ x 1))").unwrap();
        let baseline = loaded(&mut repl);
        for i in 0..4_000 {
            assert_eq!(repl.eval("(+ 1 2)").unwrap(), ["3"]);
            repl.eval(&format!("(let ((x {i})) (+ x 1))")).unwrap();
        }
        let slots = repl.isolate().with(|cx| cx.module_slots());
        assert!(slots < 3_000, "{slots} slots");
        assert_eq!(loaded(&mut repl), baseline);
        for i in 0..4_000 {
            repl.eval(&format!("(define (f x) (+ x {i}))")).unwrap();
        }
        let slots = repl.isolate().with(|cx| cx.module_slots());
        assert!(slots < 3_000, "{slots} slots");
        assert_eq!(repl.eval("(f 1)").unwrap(), ["4000"]);
        assert_eq!(loaded(&mut repl), baseline);
    }

    #[test]
    fn repl_inputs_are_freed_under_counting_strategies() {
        repl_inputs_are_freed::<DeferredHeadered>();
        repl_inputs_are_freed::<PreciseSideCounts>();
    }

    /// A module that calls a function directly is refused: the closure a
    /// direct call gives its callee is kept with the module's instance, so
    /// it would keep the module for ever.
    #[test]
    fn modules_with_direct_calls_are_not_transient() {
        // Without the optimiser there are no direct calls to refuse.
        if std::env::var_os("VALI_NO_OPT").is_some() {
            return;
        }
        let module = crate::compile(
            "(define (f x) (if (= x 0) 0 (+ 1 (f (- x 1))))) (f 3)",
            "direct",
        )
        .unwrap();
        let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let error = cx.load_transient(module).unwrap_err();
        assert!(error.to_string().contains("direct calls"), "{error}");
    }
}
