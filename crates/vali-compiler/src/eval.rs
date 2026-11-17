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
}

impl Evaluator {
    pub(crate) fn new(expander: Expander) -> Evaluator {
        Evaluator {
            expander,
            loader: None,
            count: 0,
        }
    }
}

/// An environment: the import sets `environment` was given.
#[derive(Clone, Debug)]
pub(crate) struct Environment {
    sets: Vec<ImportSet>,
}

impl RustObject for Environment {
    const NAME: &'static str = "environment";
}

/// The procedures `(scheme eval)` is written over, which `(vali primitive)`
/// exports.
pub(crate) const EVAL_PROCEDURES: &[&str] = &["%environment", "%eval-prepare"];

/// Binds the procedures `(scheme eval)` is written over in `cx`. They work
/// once a program that imports `(scheme eval)` is loaded.
pub(crate) fn install<S: Strategy>(cx: &mut Cx<S>) -> Result<(), Error> {
    cx.define_native(
        "%environment",
        Arity {
            required: 0,
            optional: 0,
            rest: true,
        },
        environment,
    )?;
    cx.define_native("%eval-prepare", Arity::exactly(2), eval_prepare)?;
    Ok(())
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
    let environment = cx.object(Environment { sets })?;
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
    let sets = cx
        .with_object(&environment, |e: &mut Environment| e.sets.clone())
        .map_err(|_| Error::scheme("eval: expected an environment"))?;
    let Some(data) = cx.set_evaluator(None) else {
        return Err(Error::scheme(
            "eval: this isolate cannot evaluate code: load a program that imports (scheme eval)",
        ));
    };
    let mut evaluator = data
        .downcast::<Evaluator>()
        .map_err(|_| Error::scheme("eval: the isolate's evaluator is not Vali's"))?;
    let thunks = prepare(cx, &mut evaluator, expr, sets);
    cx.set_evaluator(Some(evaluator));
    let mut list = Handle::empty_list();
    for thunk in thunks?.iter().rev() {
        list = cx.cons(thunk, &list)?;
    }
    cx.get(&list)
}

/// A loader to fetch with: the host's, or none.
struct Fetcher<'a>(Option<&'a (dyn Loader + Send + Sync)>);

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

fn prepare<S: Strategy>(
    cx: &mut Cx<S>,
    evaluator: &mut Evaluator,
    expr: vali_syntax::Datum,
    sets: Vec<ImportSet>,
) -> Result<Vec<Handle>, Error> {
    // Evaluated code has no source text, so spans would mean nothing.
    let compile_error = |e: crate::CompileError| Error::scheme(format!("eval: {}", e.message));
    evaluator.count += 1;
    let identity = Identity::new(&format!("eval:{}", evaluator.count));
    let program = loader::Program {
        libraries: Vec::new(),
        imports: sets.clone(),
        implicit: false,
        body: vec![expr.clone()],
    };
    let fetcher = Fetcher(evaluator.loader.as_deref());
    let graph = loader::fetch(&program, &identity, &fetcher).map_err(compile_error)?;
    let expander = &mut evaluator.expander;
    expander.add_includes(graph.includes);
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
        let module = expander
            .library(library_identity, &library.form)
            .map_err(compile_error)?;
        if cx.library(library_identity).is_none() {
            let id = cx.load(module)?;
            cx.register_library(library_identity, id);
            thunks.push(cx.module_body(id)?);
        }
    }
    let body = expander
        .program(identity.as_str(), &sets, false, &[expr])
        .map_err(compile_error)?;
    if crate::uses_syntax(&body) {
        cx.set_expander_data(Some(Box::new(expander.syntax_data())));
    }
    let id = cx.load(body)?;
    thunks.push(cx.module_body(id)?);
    Ok(thunks)
}
