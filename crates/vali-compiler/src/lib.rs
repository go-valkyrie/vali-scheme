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

use std::fmt;

use vali_bytecode::{Module, ModuleBuilder, validate};
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

/// Reads and compiles `source` as a module named `name`.
pub fn compile(source: &str, name: &str) -> Result<Module, CompileError> {
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
    compile_datums(&read.datums, name)
}

/// Compiles already-read datums as a module named `name`.
pub fn compile_datums(datums: &[Datum], name: &str) -> Result<Module, CompileError> {
    let mut expander = expand::Expander::new()?;
    let program = expander.program(datums)?;
    let loops = analyze::loops(&expander.vars, &expander.lambdas, program);
    let generator = codegen::Generator {
        vars: &expander.vars,
        lambdas: &expander.lambdas,
        loops: &loops,
        rebound: &expander.rebound,
        module: ModuleBuilder::new(name),
    };
    let module = generator.program(program)?;
    validate(&module).map_err(|e| CompileError {
        message: format!("the compiler produced invalid bytecode: {e}"),
        span: None,
    })?;
    Ok(module)
}
