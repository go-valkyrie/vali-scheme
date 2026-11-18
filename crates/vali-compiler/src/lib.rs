//! Vali's compiler: core forms to run-format bytecode.
//!
//! [`compile`] reads Scheme text and compiles it to a [`Module`] whose body
//! runs the program's forms in order and returns the last one's value.
//! Top-level `define`s make module variables; every other free identifier
//! is a module variable linked to the global of that name when the module
//! is loaded.
//!
//! The expander (ADR 0015) resolves every identifier by sets of scopes and
//! produces the compiler's tree. The compiler is the direct one, in the
//! manner of Guile 3's baseline compiler: it goes from that tree straight
//! to bytecode. The optimising compiler, through CPS, comes later as a
//! second path into the same format.

mod analyze;
mod ast;
mod codegen;
mod eval;
mod expand;
mod features;
mod library;
mod loader;
mod pattern;
mod phase;
mod repl;

pub use library::{LibraryName, NamePart};
pub use loader::{AsyncLoader, Identity, LoadError, Loader, Source};
pub use repl::Repl;

use std::fmt;

use vali_bytecode::Module;
use vali_syntax::{Datum, Span};

/// Why a program does not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// What is wrong.
    pub message: String,
    /// Where, if known.
    pub span: Option<Span>,
    /// Where, in lines and columns, if the source is known: what
    /// `Display` shows.
    pub location: Option<vali_syntax::Location>,
}

impl CompileError {
    /// This error, located in `sources` if its span is in one.
    pub(crate) fn located(mut self, sources: &vali_syntax::Sources) -> CompileError {
        if self.location.is_none() {
            self.location = self.span.and_then(|span| sources.locate(span));
        }
        self
    }
}

/// `source:line:column: message` where the source is known.
impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(location) = &self.location {
            return write!(f, "{location}: {}", self.message);
        }
        match self.span {
            Some(span) => write!(
                f,
                "{} (bytes {}..{})",
                self.message,
                u32::from(span.range.start()),
                u32::from(span.range.end())
            ),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for CompileError {}

/// A compiled program: the libraries it needs, each after the libraries it
/// imports, and its body.
#[derive(Debug)]
pub struct Program {
    /// Each library, with its identity.
    pub libraries: Vec<(Identity, Module)>,
    /// The program's body.
    pub body: Module,
    /// What syntax objects at run time need, if the program or a library
    /// uses them: [`load`] gives it to the isolate.
    pub syntax: Option<SyntaxData>,
    /// The native libraries among `libraries`, which [`load`] registers
    /// with the isolate before their modules run (ADR 0016 #8).
    pub natives: Vec<(Identity, std::sync::Arc<vali_runtime::NativeLibrary>)>,
    /// What `eval` needs, if the program imports `(scheme eval)`: the
    /// expander that compiled it, which [`load`] keeps in the isolate.
    pub evaluator: Option<Evaluator>,
}

/// The expander that compiled a program that imports `(scheme eval)`,
/// with all it knows of the program's libraries, which `eval` expands new
/// code against (ADR 0013 #7: programs that cannot evaluate code keep
/// nothing).
pub struct Evaluator(Box<eval::Evaluator>);

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Evaluator")
    }
}

/// The scopes, bindings and compiled patterns and templates that syntax
/// objects at run time need (ADR 0015 #10). Kept in memory for now: an
/// isolate holds one program's at a time, and loading another replaces it.
#[derive(Clone)]
pub struct SyntaxData(phase::Shared);

impl std::fmt::Debug for SyntaxData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyntaxData")
    }
}

/// Whether `module` calls the procedures syntax objects at run time use.
pub(crate) fn uses_syntax(module: &Module) -> bool {
    module.variables.iter().any(|v| {
        v.link == vali_bytecode::Link::Primitive
            && [
                phase::QUOTE_SYNTAX,
                phase::SYNTAX_MATCH,
                phase::SYNTAX_BUILD,
            ]
            .contains(&&*v.name)
    })
}

/// A loader with no libraries.
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

/// Reads `source`, registered in `sources` as `identity`.
fn read(
    source: &str,
    identity: &str,
    sources: &mut vali_syntax::Sources,
) -> Result<Vec<Datum>, CompileError> {
    let id = sources.add(identity, source);
    let read = vali_syntax::read(source, id);
    if let Some(e) = read.errors.first() {
        let error = CompileError {
            message: format!("read: {}", e.kind),
            span: Some(Span {
                source: id,
                range: e.range,
            }),
            location: None,
        };
        return Err(error.located(sources));
    }
    Ok(read.datums)
}

/// Reads and compiles `source`, a program that imports only standard
/// libraries, as a module named `name`. The module must not need any
/// library's variables at run time: standard libraries that only provide
/// macros and the procedures implemented in Rust, as `(scheme base)` does,
/// need nothing loaded. Otherwise, use [`compile_program`] and [`load`].
pub fn compile(source: &str, name: &str) -> Result<Module, CompileError> {
    let program = compile_program(source, &Identity::new(name), &NoLibraries)?;
    let linked = program.body.variables.iter().find_map(|v| match &v.link {
        vali_bytecode::Link::Imported(library) => Some(library),
        _ => None,
    });
    if program.syntax.is_some() {
        return Err(CompileError {
            message: "the program uses syntax objects at run time: use compile_program and load"
                .to_owned(),
            span: None,
            location: None,
        });
    }
    if let Some(library) = linked {
        return Err(CompileError {
            message: format!(
                "the program uses the library {library} at run time: use compile_program and load"
            ),
            span: None,
            location: None,
        });
    }
    Ok(program.body)
}

/// Reads and compiles the program `source`, whose identity is `identity`,
/// with the libraries it imports from `loader`.
pub fn compile_program(
    source: &str,
    identity: &Identity,
    loader: &impl Loader,
) -> Result<Program, CompileError> {
    let mut sources = vali_syntax::Sources::new();
    let program = loader::program(&read(source, identity.as_str(), &mut sources)?)
        .map_err(|e| e.located(&sources))?;
    let graph =
        loader::fetch(&program, identity, loader, &mut sources).map_err(|e| e.located(&sources))?;
    build(&program, graph, identity, sources)
}

/// As [`compile_program`], for Vali's own tests: the program may import
/// `(vali primitive)`, and does implicitly if it has no `import` form.
#[doc(hidden)]
pub fn compile_internal_program(
    source: &str,
    identity: &Identity,
    loader: &impl Loader,
) -> Result<Program, CompileError> {
    let mut sources = vali_syntax::Sources::new();
    let mut program = loader::program(&read(source, identity.as_str(), &mut sources)?)
        .map_err(|e| e.located(&sources))?;
    program.internal = true;
    if program.implicit {
        // Only the internal names: (scheme base) exports the others.
        let internal = vali_runtime::primitive_names()
            .into_iter()
            .chain(phase::SYNTAX_PROCEDURES.iter().copied())
            .chain(eval::EVAL_PROCEDURES.iter().copied())
            .filter(|n| n.starts_with('%'))
            .map(str::to_owned)
            .collect();
        program.imports.push(library::ImportSet::Only(
            Box::new(library::ImportSet::Library(LibraryName::new([
                "vali",
                "primitive",
            ]))),
            internal,
        ));
    }
    let graph =
        loader::fetch(&program, identity, loader, &mut sources).map_err(|e| e.located(&sources))?;
    build(&program, graph, identity, sources)
}

/// As [`compile_program`], with an asynchronous loader: the import graph
/// is fetched first, awaiting the loader, then compiled.
pub async fn compile_program_async(
    source: &str,
    identity: &Identity,
    loader: &impl AsyncLoader,
) -> Result<Program, CompileError> {
    let mut sources = vali_syntax::Sources::new();
    let program = loader::program(&read(source, identity.as_str(), &mut sources)?)
        .map_err(|e| e.located(&sources))?;
    let graph = loader::fetch_async(&program, identity, loader, &mut sources)
        .await
        .map_err(|e| e.located(&sources))?;
    build(&program, graph, identity, sources)
}

/// Expands and compiles each library of the graph, then the program.
fn build(
    program: &loader::Program,
    graph: loader::Graph,
    identity: &Identity,
    sources: vali_syntax::Sources,
) -> Result<Program, CompileError> {
    let mut expander = expand::Expander::new()?;
    expander.sources = sources;
    match expand_all(&mut expander, program, graph, identity) {
        Ok((libraries, body, natives, names_eval)) => {
            let syntax = (uses_syntax(&body) || libraries.iter().any(|(_, m)| uses_syntax(m)))
                .then(|| SyntaxData(expander.syntax_data()));
            let evaluator = names_eval.then(|| Evaluator(Box::new(eval::Evaluator::new(expander))));
            Ok(Program {
                libraries,
                body,
                syntax,
                natives,
                evaluator,
            })
        }
        Err(e) => Err(e.located(&expander.sources)),
    }
}

/// What expanding a program and its libraries gives: the libraries'
/// modules, the program's, its native libraries, and whether it imports
/// `(scheme eval)`.
type Expanded = (
    Vec<(Identity, Module)>,
    Module,
    Vec<(Identity, std::sync::Arc<vali_runtime::NativeLibrary>)>,
    bool,
);

fn expand_all(
    expander: &mut expand::Expander,
    program: &loader::Program,
    graph: loader::Graph,
    identity: &Identity,
) -> Result<Expanded, CompileError> {
    expander.set_includes(graph.includes);
    expander.add_available(graph.available);
    let names_eval = graph
        .by_name
        .contains_key(&LibraryName::new(["scheme", "eval"]));
    let mut names: Vec<Vec<LibraryName>> = vec![Vec::new(); graph.libraries.len()];
    for (name, i) in graph.by_name {
        names[i].push(name);
    }
    let mut libraries = Vec::new();
    let natives: Vec<_> = graph
        .libraries
        .iter()
        .filter_map(|l| Some((l.identity.clone(), l.native.clone()?)))
        .collect();
    for (identity, library) in &natives {
        expander.add_native(identity.as_str(), std::sync::Arc::clone(library));
    }
    for (library, names) in graph.libraries.iter().zip(names) {
        for name in names {
            expander.name_next(name);
        }
        let module = expander.library(library.identity.as_str(), &library.form, library.own)?;
        libraries.push((library.identity.clone(), module));
    }
    let body = expander.program(
        identity.as_str(),
        &program.imports,
        program.implicit,
        &program.body,
        program.internal,
    )?;
    Ok((libraries, body, natives, names_eval))
}

/// Loads a compiled program into `cx`: each library not there already is
/// loaded, run and registered under its identity, then the body is loaded.
/// Returns the body's module, to run. If the program imports `(scheme
/// eval)`, evaluated code reaches Vali's own libraries and those the
/// program loaded; [`load_with`] lets it reach the host's too.
pub fn load<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    program: Program,
) -> Result<vali_runtime::ModuleId, vali_runtime::Error> {
    load_inner(cx, program, None)
}

/// As [`load`], keeping `loader` for `eval`: evaluated code that imports a
/// library not loaded yet fetches it there, and what it includes is read
/// there too.
pub fn load_with<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    program: Program,
    loader: std::sync::Arc<dyn Loader + Send + Sync>,
) -> Result<vali_runtime::ModuleId, vali_runtime::Error> {
    load_inner(cx, program, Some(loader))
}

/// Merges a program's syntax data for run time into the isolate's, so the
/// syntax objects of programs loaded before keep their meaning.
pub(crate) fn merge_syntax_data<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    shared: phase::Shared,
) -> Result<(), vali_runtime::Error> {
    let existing = cx.set_expander_data(None).and_then(|data| {
        (data as Box<dyn std::any::Any>)
            .downcast::<phase::Shared>()
            .ok()
    });
    let merged = match existing {
        Some(mut into) => {
            let merged = expand::merge_syntax(&mut into, shared);
            let failed = merged.err();
            cx.set_expander_data(Some(into));
            return match failed {
                Some(e) => Err(vali_runtime::Error::scheme(e.to_string())),
                None => Ok(()),
            };
        }
        None => shared,
    };
    cx.set_expander_data(Some(Box::new(merged)));
    Ok(())
}

fn load_inner<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    mut program: Program,
    loader: Option<std::sync::Arc<dyn Loader + Send + Sync>>,
) -> Result<vali_runtime::ModuleId, vali_runtime::Error> {
    phase::install(cx)?;
    if let Some(Evaluator(mut evaluator)) = program.evaluator.take() {
        evaluator.loader = loader;
        cx.set_evaluator(Some(evaluator));
    }
    if let Some(SyntaxData(shared)) = program.syntax {
        merge_syntax_data(cx, shared)?;
    }
    for (identity, library) in program.natives {
        cx.register_native_library(identity.as_str(), library);
    }
    for (identity, module) in program.libraries {
        // A library the isolate has must be the one this was compiled
        // against: linking to another of the same name would be wrong
        // silently.
        if let Some(id) = cx.library(identity.as_str()) {
            if *cx.module(id) != module {
                return Err(vali_runtime::Error::scheme(format!(
                    "library {identity} is already loaded, compiled differently: \
                     this program was compiled against another version of it"
                )));
            }
            continue;
        }
        let id = cx.load(module)?;
        cx.run(id)?;
        cx.register_library(identity.as_str(), id);
    }
    cx.load(program.body)
}
