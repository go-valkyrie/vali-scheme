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
            interaction: vec![ImportSet::Library(library::LibraryName::new([
                "scheme", "base",
            ]))],
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
    cx.define_native(
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
    let identity = Identity::new(&format!("eval:{}", evaluator.count));
    let program = loader::Program {
        libraries: Vec::new(),
        imports: sets.clone(),
        implicit: false,
        body: body.clone(),
    };
    let fetcher = Fetcher(evaluator.loader.as_deref());
    let graph = loader::fetch(
        &program,
        &identity,
        &fetcher,
        &mut evaluator.expander.sources,
    )
    .map_err(compile_error)?;
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
    let body = if interactive {
        let module = expander
            .interaction(identity.as_str(), &sets, &body)
            .map_err(compile_error)?;
        evaluator.interaction = sets;
        module
    } else {
        expander
            .program(identity.as_str(), &sets, false, &body)
            .map_err(compile_error)?
    };
    let expander = &mut evaluator.expander;
    if crate::uses_syntax(&body) {
        cx.set_expander_data(Some(Box::new(expander.syntax_data())));
    }
    let id = cx.load(body)?;
    thunks.push(cx.module_body(id)?);
    Ok(thunks)
}
