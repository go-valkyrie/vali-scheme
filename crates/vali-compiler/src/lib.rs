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
mod expand;
mod library;
mod loader;
mod pattern;
mod phase;

pub use library::{LibraryName, NamePart};
pub use loader::{AsyncLoader, Identity, LoadError, Loader, Source};

use std::fmt;

use vali_bytecode::Module;
use vali_syntax::{Datum, SourceId, Span};

/// Why a program does not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// What is wrong.
    pub message: String,
    /// Where, if known.
    pub span: Option<Span>,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
#[derive(Clone, Debug)]
pub struct Program {
    /// Each library, with its identity.
    pub libraries: Vec<(Identity, Module)>,
    /// The program's body.
    pub body: Module,
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

fn read(source: &str) -> Result<Vec<Datum>, CompileError> {
    let read = vali_syntax::read(source, SourceId(0));
    if let Some(e) = read.errors.first() {
        return Err(CompileError {
            message: format!("read: {}", e.kind),
            span: Some(Span {
                source: SourceId(0),
                range: e.range,
            }),
        });
    }
    Ok(read.datums)
}

/// Reads and compiles `source`, a program that needs no libraries but
/// built-in ones, as a module named `name`.
pub fn compile(source: &str, name: &str) -> Result<Module, CompileError> {
    let program = compile_program(source, &Identity::new(name), &NoLibraries)?;
    if let Some((identity, _)) = program.libraries.first() {
        return Err(CompileError {
            message: format!("the program needs the library {identity}: use compile_program"),
            span: None,
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
    let program = loader::program(&read(source)?)?;
    let graph = loader::fetch(&program, identity, loader)?;
    build(&program, graph, identity)
}

/// As [`compile_program`], with an asynchronous loader: the import graph
/// is fetched first, awaiting the loader, then compiled.
pub async fn compile_program_async(
    source: &str,
    identity: &Identity,
    loader: &impl AsyncLoader,
) -> Result<Program, CompileError> {
    let program = loader::program(&read(source)?)?;
    let graph = loader::fetch_async(&program, identity, loader).await?;
    build(&program, graph, identity)
}

/// Expands and compiles each library of the graph, then the program.
fn build(
    program: &loader::Program,
    graph: loader::Graph,
    identity: &Identity,
) -> Result<Program, CompileError> {
    let mut expander = expand::Expander::new()?;
    let mut names: Vec<Vec<LibraryName>> = vec![Vec::new(); graph.libraries.len()];
    for (name, i) in graph.by_name {
        names[i].push(name);
    }
    let mut libraries = Vec::new();
    for (library, names) in graph.libraries.iter().zip(names) {
        for name in names {
            expander.name_next(name);
        }
        let module = expander.library(library.identity.as_str(), &library.form)?;
        libraries.push((library.identity.clone(), module));
    }
    let body = expander.program(identity.as_str(), &program.imports, &program.body)?;
    Ok(Program { libraries, body })
}

/// Loads a compiled program into `cx`: each library not there already is
/// loaded, run and registered under its identity, then the body is loaded.
/// Returns the body's module, to run.
pub fn load<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    program: Program,
) -> Result<vali_runtime::ModuleId, vali_runtime::Error> {
    for (identity, module) in program.libraries {
        if cx.library(identity.as_str()).is_some() {
            continue;
        }
        let id = cx.load(module)?;
        cx.run(id)?;
        cx.register_library(identity.as_str(), id);
    }
    cx.load(program.body)
}
