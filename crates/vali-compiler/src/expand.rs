//! The expander (ADR 0015): from syntax objects to the resolved tree.
//!
//! Every identifier is resolved by sets of scopes. The core forms are bound
//! in the *core* scope, and a program's forms carry that scope and the
//! program's own, so a program's definitions shadow core forms and core
//! forms stay reachable from code that macros introduce.
//!
//! Bodies are expanded in two passes, as definition contexts must be: the
//! first finds every definition, splicing `begin`s, so that the second
//! expands each value and expression with every name in the body bound.
//!
//! A free identifier, one with no binding, is a module variable linked to
//! the global of that name; until libraries exist, that is how the standard
//! procedures are reached.
//!
//! Macros are `syntax-rules` transformers, matched and built in Rust
//! (`pattern`). Each use flips a fresh macro scope on its input and output;
//! a use in a definition context also adds a use-site scope to its input,
//! which definitions then drop, so a macro can define a name its use
//! supplies. The derived forms are macros in `(scheme base)`, whose source
//! is in `lib/`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use vali_syntax::{
    AlreadyBound, BindingTable, Datum, DatumKind, Exposed, Origin, Phase, Phases, Resolution,
    Scope, ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax, TooManyScopes,
    free_identifier_eq,
};

use vali_bytecode::{Export, Link, Module, ModuleBuilder, validate};
use vali_runtime::{Cx, Handle, NoCollection};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};
use crate::library::{ExportSpec, ImportSet, LibraryForm, LibraryName};
use crate::pattern::{self, Builder, Match, Pattern, Spelling, Template, Vars};
use crate::phase::{self, Compiled, QUOTE_SYNTAX, SYNTAX_BUILD, SYNTAX_MATCH, Shared};

/// How many macro steps may produce one form, counting both uses nested
/// in expressions and a body's forms that expand into further uses, so a
/// macro that expands into itself forever is reported. Legitimate code
/// nests deeply too: a `cond` with a thousand clauses is a thousand uses
/// deep.
const MAX_MACRO_DEPTH: usize = 10_000;

type Result<T> = std::result::Result<T, CompileError>;

/// When less stack than this is left, recursion moves to a new segment of
/// `STACK_GROWTH` bytes.
pub(crate) const RED_ZONE: usize = 128 * 1024;
pub(crate) const STACK_GROWTH: usize = 2 * 1024 * 1024;

/// The core forms (ADR 0015 #15) that exist so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Core {
    Quote,
    If,
    Begin,
    Lambda,
    Let,
    Letrec,
    Set,
    Define,
    DefineSyntax,
    LetSyntax,
    LetrecSyntax,
    SyntaxRules,
    QuoteSyntax,
    SyntaxCase,
    Syntax,
    Include,
    IncludeCi,
    CondExpand,
}

const CORE: &[(&str, Core)] = &[
    ("quote", Core::Quote),
    ("if", Core::If),
    ("begin", Core::Begin),
    ("lambda", Core::Lambda),
    ("let", Core::Let),
    ("letrec", Core::Letrec),
    ("letrec*", Core::Letrec),
    ("set!", Core::Set),
    ("define", Core::Define),
    ("define-syntax", Core::DefineSyntax),
    ("let-syntax", Core::LetSyntax),
    ("letrec-syntax", Core::LetrecSyntax),
    ("syntax-rules", Core::SyntaxRules),
    ("quote-syntax", Core::QuoteSyntax),
    ("syntax-case", Core::SyntaxCase),
    ("syntax", Core::Syntax),
    ("include", Core::Include),
    ("include-ci", Core::IncludeCi),
    ("cond-expand", Core::CondExpand),
];

/// What an identifier can be bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Core(Core),
    Local(VarId),
    /// A top-level variable: the unit, library or program, that defines
    /// it, and its name there.
    Variable(usize, String),
    /// A standard procedure implemented in Rust, reached as the global of
    /// that name, and inlined where the compiler can.
    Primitive(String),
    /// A macro, by number.
    Macro(usize),
    /// A REPL's top-level variable: the isolate's global of that name,
    /// bound late, and never inlined.
    Global(String),
    /// A `syntax-case` pattern variable: the hidden local holding what it
    /// matched, and its depth.
    PatternVar(VarId, usize),
}

/// A `syntax-rules` macro.
struct Macro {
    name: String,
    transformer: Transformer,
}

enum Transformer {
    /// `syntax-rules`, run in Rust.
    Rules(Vec<Clause>),
    /// A procedure in the phase-1 isolate.
    Procedure(Handle),
}

struct Clause {
    pattern: Pattern,
    template: Template,
    /// Each pattern variable's depth.
    depths: Vec<usize>,
}

/// One form of a body, after the first pass.
enum Item {
    /// `(define id value)` at top level: a module variable.
    Global(String, Option<Syntax>),
    /// `(define id value)` in a body: a local variable.
    Local(VarId, Option<Syntax>),
    /// `(define id value)` at a REPL's top level: an isolate global, by its
    /// global name.
    Repl(String, Option<Syntax>),
    Expr(Syntax),
}

fn error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError {
        message: message.into(),
        span: Some(span),
        location: None,
    }
}

/// The constant `n`.
fn number(n: usize, span: Span) -> Expr {
    Expr::Const(Datum {
        kind: DatumKind::Number(vali_num::Number::Integer(n.into())),
        span,
    })
}

fn too_many(e: TooManyScopes) -> CompileError {
    CompileError {
        message: e.to_string(),
        span: None,
        location: None,
    }
}

/// A library or the program, as it is compiled.
struct Unit {
    identity: String,
    /// What it exports, by external name.
    interface: HashMap<String, Binding>,
    /// The units it imports, which its instances need first.
    imports: Vec<usize>,
    /// Its compiled module, once there is one.
    module: Option<Module>,
    /// Whether an unbound identifier is an error. It is in a library and
    /// in a program with imports; a program with none refers to the
    /// isolate's globals by name, as a script does.
    strict: bool,
    /// Whether it is a REPL's input, compiled in the interaction
    /// environment: its top-level definitions are REPL globals, and an
    /// unbound identifier names one.
    repl: bool,
}

/// The isolate global a REPL's top-level `name` is. REPL globals have
/// names of their own, so redefining `car` at the REPL cannot change the
/// `car` libraries link to, which is the isolate's global `car`.
pub(crate) fn repl_global(name: &str) -> String {
    format!("repl:{name}")
}

pub(crate) struct Expander {
    pub(crate) vars: Vec<VarInfo>,
    pub(crate) lambdas: Vec<Lambda>,
    context: ScopeContext,
    table: BindingTable<String, Binding>,
    /// The scopes of the core forms.
    core: ScopeSetId,
    /// The scopes of the current unit's own forms.
    top: ScopeSetId,
    /// The units compiled so far, the current one last.
    units: Vec<Unit>,
    /// Each library name, to its unit.
    by_name: HashMap<LibraryName, usize>,
    /// What `(vali core)` exports: the core forms.
    core_interface: Vec<(String, Binding)>,
    /// The units instantiated in the phase-1 isolate.
    phase1_units: HashSet<usize>,
    /// The `lambda`s being expanded, outermost first.
    stack: Vec<LambdaId>,
    /// Suffixes for module variables that macros introduce.
    fresh: usize,
    macros: Vec<Macro>,
    /// Macro uses in expressions being expanded, one inside another.
    depth: usize,
    /// The isolate transformers written in Scheme run in, once one exists.
    phase1: Option<Cx<NoCollection>>,
    /// What phase-1 code's syntax procedures look up by number.
    compiled: Compiled,
    /// For each transformer being expanded, one inside another, where its
    /// `lambda` sits in `stack`. Code above the innermost runs at phase 1.
    floors: Vec<usize>,
    /// The native libraries the program imports, which the phase-1
    /// isolate registers, so their expansion-safe functions run there.
    natives: Vec<(String, std::sync::Arc<vali_runtime::NativeLibrary>)>,
    /// The interaction environment's scope sets, once it is used: where
    /// its imports are bound, and where its own definitions are, inside
    /// those. Every REPL input uses the same ones, so what one defines the
    /// next sees.
    repl: Option<(ScopeSetId, ScopeSetId)>,
    /// The files bodies include, fetched with the import graph.
    includes: crate::loader::Includes,
    /// The sources read, which spans' source ids index.
    pub(crate) sources: vali_syntax::Sources,
}

impl Expander {
    pub(crate) fn new() -> Result<Expander> {
        let mut context = ScopeContext::new();
        let core_scope = context.new_scope(ScopeKind::Library).map_err(too_many)?;
        let core = context.intern(&[core_scope]).map_err(too_many)?;
        let mut table = BindingTable::new();
        for &(name, form) in CORE {
            table
                .bind(
                    &context,
                    name.to_owned(),
                    core,
                    Phases::All,
                    Binding::Core(form),
                )
                .map_err(|_| CompileError {
                    message: format!("{name} is bound twice"),
                    span: None,
                    location: None,
                })?;
        }
        let expander = Expander {
            vars: Vec::new(),
            lambdas: Vec::new(),
            context,
            table,
            core,
            top: core,
            units: Vec::new(),
            by_name: HashMap::new(),
            core_interface: CORE
                .iter()
                .map(|&(name, form)| (name.to_owned(), Binding::Core(form)))
                .collect(),
            phase1_units: HashSet::new(),
            stack: Vec::new(),
            fresh: 0,
            macros: Vec::new(),
            depth: 0,
            phase1: None,
            compiled: Compiled::default(),
            floors: Vec::new(),
            natives: Vec::new(),
            includes: crate::loader::Includes::new(),
            sources: vali_syntax::Sources::new(),
            repl: None,
        };
        Ok(expander)
    }

    /// Notes a native library the program imports, for the phase-1
    /// isolate to register.
    /// The files the program and its libraries include, fetched with the
    /// import graph.
    pub(crate) fn set_includes(&mut self, includes: crate::loader::Includes) {
        self.includes = includes;
    }

    /// The forms `(include path ...)`, or `include-ci`, stands for: each
    /// file's data, in the lexical context of the `include` keyword
    /// (R7RS 4.1.7).
    fn included(&mut self, items: &[Syntax], ci: bool, span: Span) -> Result<Vec<Syntax>> {
        let keyword = if ci { "include-ci" } else { "include" };
        let identity = self
            .units
            .last()
            .expect("a unit being expanded")
            .identity
            .clone();
        let scopes = items[0].scopes();
        let mut forms = Vec::new();
        for item in &items[1..] {
            let DatumKind::String(path) = item.to_datum().kind else {
                return Err(error(
                    format!("{keyword}: expected file names as strings"),
                    item.span(),
                ));
            };
            let text = match self.includes.get(&(identity.clone(), path.clone())) {
                Some(Ok(text)) => text.clone(),
                Some(Err(why)) => return Err(error(format!("{keyword}: {why}"), span)),
                None => {
                    return Err(error(
                        format!(
                            "{keyword}: {path} was not read: only an include written out in the source is"
                        ),
                        span,
                    ));
                }
            };
            for datum in crate::loader::read_included(&text, ci, &path, &mut self.sources)? {
                forms.push(Syntax::from_datum(&datum, scopes));
            }
        }
        Ok(forms)
    }

    /// The forms of the first `cond-expand` clause whose requirement holds
    /// (R7RS 4.2.1), or none. A `(library name)` requirement holds for
    /// Vali's libraries and those compiled so far.
    fn cond_expand(&mut self, items: &[Syntax], span: Span) -> Result<Vec<Syntax>> {
        for clause in &items[1..] {
            let Exposed::List {
                items: parts,
                tail: None,
            } = self.expose(clause)?
            else {
                return Err(error("cond-expand: expected a clause", clause.span()));
            };
            let Some((requirement, body)) = parts.split_first() else {
                return Err(error("cond-expand: expected a clause", clause.span()));
            };
            let requirement = requirement.to_datum();
            let by_name = &self.by_name;
            if crate::features::is_else(&requirement)
                || crate::features::satisfied(&requirement, &mut |n| {
                    crate::loader::is_standard(n) || by_name.contains_key(n)
                })?
            {
                return Ok(body.to_vec());
            }
        }
        let _ = span;
        Ok(Vec::new())
    }

    pub(crate) fn add_native(
        &mut self,
        identity: &str,
        library: std::sync::Arc<vali_runtime::NativeLibrary>,
    ) {
        if let Some(cx) = self.phase1.as_mut() {
            cx.register_native_library(identity, std::sync::Arc::clone(&library));
        }
        self.natives.push((identity.to_owned(), library));
    }

    /// Notes that `name` is a library's name for the next unit compiled.
    pub(crate) fn name_next(&mut self, name: LibraryName) {
        self.by_name.insert(name, self.units.len());
    }

    /// The unit compiled with `identity`, if there is one.
    pub(crate) fn unit_of(&self, identity: &str) -> Option<usize> {
        self.units.iter().position(|u| u.identity == identity)
    }

    /// Notes that `name` is also the name of the library `unit` is.
    pub(crate) fn name_unit(&mut self, name: LibraryName, unit: usize) {
        self.by_name.insert(name, unit);
    }

    /// More files that bodies include, fetched for code compiled later.
    pub(crate) fn add_includes(&mut self, includes: crate::loader::Includes) {
        self.includes.extend(includes);
    }

    /// Expands and compiles a library, which becomes the next unit.
    pub(crate) fn library(&mut self, identity: &str, form: &LibraryForm) -> Result<Module> {
        if self.natives.iter().any(|(i, _)| i == identity) {
            return self.native_library(identity, form);
        }
        let scope = self.scope(ScopeKind::Library)?;
        let top = self.context.intern(&[scope]).map_err(too_many)?;
        self.unit(
            identity,
            &form.imports,
            top,
            top,
            &form.body,
            &form.exports,
            form.span,
            false,
        )
    }

    /// The unit for a native library (ADR 0016 #8): a module whose
    /// variables link straight to the library's functions as it loads, and
    /// export them. `form` names the functions; it has no body.
    fn native_library(&mut self, identity: &str, form: &LibraryForm) -> Result<Module> {
        let index = self.units.len();
        let mut builder = ModuleBuilder::new(identity);
        let mut interface = HashMap::new();
        for spec in &form.exports {
            let variable = builder.native(identity, &spec.internal);
            builder.export(&spec.external, variable);
            interface.insert(
                spec.external.clone(),
                Binding::Variable(index, spec.external.clone()),
            );
        }
        let unspecified = builder.constant(vali_bytecode::Constant::Unspecified);
        let mut body = vali_bytecode::FunctionBuilder::new(
            Some(identity),
            vali_bytecode::Arity::exactly(0),
            2,
            0,
        );
        body.emit(vali_bytecode::Instruction::LoadConstant {
            dst: 1,
            constant: unspecified,
        })
        .emit(vali_bytecode::Instruction::Return { src: 1 });
        let body = body
            .finish()
            .map_err(|e| error(format!("{identity}: {e}"), form.span))?;
        let body = builder.function(body);
        let module = builder.finish(body);
        validate(&module).map_err(|e| error(format!("{identity}: {e}"), form.span))?;
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface,
            imports: Vec::new(),
            module: Some(module.clone()),
            strict: true,
            repl: false,
        });
        Ok(module)
    }

    /// Expands and compiles a program. With imports, it sees only what it
    /// imports. An `implicit` import, of `(scheme base)` for a program with
    /// no `import` form (ADR 0007 #11), is bound in a scope around the
    /// program's own, so the program can define names it also imports, as
    /// a script or a REPL can.
    pub(crate) fn program(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        implicit: bool,
        body: &[vali_syntax::Datum],
    ) -> Result<Module> {
        let span = body.first().map_or(
            Span {
                source: SourceId(0),
                range: Default::default(),
            },
            |d| d.span,
        );
        let scope = self.scope(ScopeKind::Library)?;
        if implicit {
            let around = self.scope(ScopeKind::Library)?;
            let imported = self.context.intern(&[around]).map_err(too_many)?;
            let top = self.context.intern(&[around, scope]).map_err(too_many)?;
            return self.unit(identity, imports, imported, top, body, &[], span, false);
        }
        let top = self.context.intern(&[scope]).map_err(too_many)?;
        self.unit(identity, imports, top, top, body, &[], span, false)
    }

    /// Expands and compiles a REPL's input in the interaction environment
    /// (R7RS 6.12's `interaction-environment`): `imports` bound around the
    /// REPL's own definitions, which persist from one input to the next.
    pub(crate) fn interaction(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        body: &[vali_syntax::Datum],
    ) -> Result<Module> {
        let span = body.first().map_or(
            Span {
                source: SourceId(0),
                range: Default::default(),
            },
            |d| d.span,
        );
        let (imported, top) = match self.repl {
            Some(scopes) => scopes,
            None => {
                let around = self.scope(ScopeKind::Library)?;
                let scope = self.scope(ScopeKind::Library)?;
                let imported = self.context.intern(&[around]).map_err(too_many)?;
                let top = self.context.intern(&[around, scope]).map_err(too_many)?;
                self.repl = Some((imported, top));
                (imported, top)
            }
        };
        // Imports bound before are bound again, which is a no-op.
        self.unit(identity, imports, imported, top, body, &[], span, true)
    }

    /// Whether the unit being compiled is a REPL's input.
    fn in_repl(&self) -> bool {
        self.units.last().is_some_and(|u| u.repl)
    }

    /// `(define id ...)` at a REPL's top level: binds `id` to a REPL
    /// global, replacing whatever it was bound to there, and returns the
    /// global's name.
    fn define_repl(&mut self, id: &Syntax) -> Result<String> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error("define: expected a name", id.span()))?;
        let global = if id.scopes() == self.top {
            repl_global(name)
        } else {
            self.fresh += 1;
            repl_global(&format!("{name}~{}", self.fresh))
        };
        self.table.rebind(
            &self.context,
            name.to_owned(),
            id.scopes(),
            Phases::All,
            Binding::Global(global.clone()),
        );
        Ok(global)
    }

    /// Expands `body` as a new unit whose forms carry `top`, with
    /// `imports` bound at `imported`, and compiles it into a module that
    /// exports every variable it defines, by name, for linking.
    #[expect(
        clippy::too_many_arguments,
        reason = "each says how the unit is scoped"
    )]
    fn unit(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        imported: ScopeSetId,
        top: ScopeSetId,
        body: &[vali_syntax::Datum],
        exports: &[ExportSpec],
        span: Span,
        repl: bool,
    ) -> Result<Module> {
        let index = self.units.len();
        let imported_at = imported;
        let mut imported = Vec::new();
        for set in imports {
            if let Some(&unit) = self.by_name.get(set.library()) {
                imported.push(unit);
            }
        }
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface: HashMap::new(),
            imports: imported,
            module: None,
            strict: imported_at == top,
            repl,
        });
        self.top = top;
        for set in imports {
            self.import(set, imported_at, span)?;
        }
        let forms = body.iter().map(|d| Syntax::from_datum(d, top)).collect();
        let lambda = self.open_lambda(None);
        let expr = self.body(forms, true, span)?;
        self.close_lambda(lambda, Vec::new(), None, expr);
        let mut interface = HashMap::new();
        for spec in exports {
            let id = Syntax::identifier(&spec.internal, top, spec.span, None);
            let Some(binding) = self.resolve(&id)? else {
                return Err(error(
                    format!(
                        "{} is exported but neither defined nor imported",
                        spec.internal
                    ),
                    spec.span,
                ));
            };
            if interface.insert(spec.external.clone(), binding).is_some() {
                return Err(error(
                    format!("{} is exported twice", spec.external),
                    spec.span,
                ));
            }
        }
        let module = self.module(lambda, identity)?;
        let unit = &mut self.units[index];
        unit.interface = interface;
        unit.module = Some(module.clone());
        Ok(module)
    }

    /// Compiles the `lambda` `body` as a module's body, exporting every
    /// variable the module defines under its own name.
    fn module(&self, body: LambdaId, name: &str) -> Result<Module> {
        let loops = crate::analyze::loops(&self.vars, &self.lambdas, body);
        let generator = crate::codegen::Generator {
            vars: &self.vars,
            lambdas: &self.lambdas,
            loops: &loops,
            module: ModuleBuilder::new(name),
            sources: &self.sources,
            calls: Vec::new(),
        };
        let mut module = generator.program(body)?;
        for (i, variable) in module.variables.iter().enumerate() {
            if variable.link == Link::Defined {
                module.exports.push(Export {
                    name: variable.name.clone(),
                    variable: u16::try_from(i).expect("at most 65,536 variables"),
                });
            }
        }
        validate(&module).map_err(|e| CompileError {
            message: format!("{name}: the compiler produced invalid bytecode: {e}"),
            span: None,
            location: None,
        })?;
        Ok(module)
    }

    /// What the library `name` exports, by name.
    fn interface(&self, name: &LibraryName, span: Span) -> Result<Vec<(String, Binding)>> {
        if *name == LibraryName::new(["vali", "core"]) {
            return Ok(self.core_interface.clone());
        }
        if *name == LibraryName::new(["vali", "primitive"]) {
            // The syntax procedures exist only in the phase-1 isolate, but
            // are bound everywhere, so transformers in libraries can name
            // them.
            return Ok(vali_runtime::primitive_names()
                .into_iter()
                .chain(phase::SYNTAX_PROCEDURES.iter().copied())
                .chain(crate::eval::EVAL_PROCEDURES.iter().copied())
                .map(|n| (n.to_owned(), Binding::Primitive(n.to_owned())))
                .collect());
        }
        let Some(&unit) = self.by_name.get(name) else {
            return Err(error(format!("library {name} is not available"), span));
        };
        let mut bindings: Vec<_> = self.units[unit]
            .interface
            .iter()
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect();
        bindings.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(bindings)
    }

    /// What an import set brings in, by the names it brings them in under.
    fn import_bindings(&self, set: &ImportSet, span: Span) -> Result<Vec<(String, Binding)>> {
        let missing = |name: &str, library: &LibraryName| {
            error(format!("{library} does not export {name}"), span)
        };
        Ok(match set {
            ImportSet::Library(name) => self.interface(name, span)?,
            ImportSet::Only(inner, names) => {
                let all = self.import_bindings(inner, span)?;
                let mut kept = Vec::new();
                for name in names {
                    let found = all
                        .iter()
                        .find(|(n, _)| n == name)
                        .ok_or_else(|| missing(name, set.library()))?;
                    kept.push(found.clone());
                }
                kept
            }
            ImportSet::Except(inner, names) => {
                let all = self.import_bindings(inner, span)?;
                for name in names {
                    if !all.iter().any(|(n, _)| n == name) {
                        return Err(missing(name, set.library()));
                    }
                }
                all.into_iter()
                    .filter(|(n, _)| !names.contains(n))
                    .collect()
            }
            ImportSet::Prefix(inner, prefix) => self
                .import_bindings(inner, span)?
                .into_iter()
                .map(|(n, b)| (format!("{prefix}{n}"), b))
                .collect(),
            ImportSet::Rename(inner, renames) => {
                let mut all = self.import_bindings(inner, span)?;
                for (from, to) in renames {
                    let entry = all
                        .iter_mut()
                        .find(|(n, _)| n == from)
                        .ok_or_else(|| missing(from, set.library()))?;
                    entry.0.clone_from(to);
                }
                all
            }
        })
    }

    /// Binds what `set` imports at `top`. Importing one name twice is
    /// fine if both mean the same.
    fn import(&mut self, set: &ImportSet, top: ScopeSetId, span: Span) -> Result<()> {
        for (name, binding) in self.import_bindings(set, span)? {
            match self.table.bind(
                &self.context,
                name.clone(),
                top,
                Phases::All,
                binding.clone(),
            ) {
                Ok(()) => {}
                Err(AlreadyBound { existing }) if *existing == binding => {}
                Err(_) => {
                    return Err(error(
                        format!("{name} is imported twice, with different meanings"),
                        span,
                    ));
                }
            }
        }
        Ok(())
    }

    // Scopes and bindings.

    fn scope(&mut self, kind: ScopeKind) -> Result<Scope> {
        self.context.new_scope(kind).map_err(too_many)
    }

    fn add(&mut self, stx: &Syntax, scope: Scope) -> Result<Syntax> {
        stx.add(&mut self.context, scope).map_err(too_many)
    }

    fn expose<'s>(&mut self, stx: &'s Syntax) -> Result<Exposed<'s>> {
        stx.expose(&mut self.context).map_err(too_many)
    }

    /// The elements of a proper list, or an error naming `what` it should
    /// have been.
    fn items(&mut self, stx: &Syntax, what: &str) -> Result<Vec<Syntax>> {
        stx.list_items(&mut self.context)
            .map_err(too_many)?
            .ok_or_else(|| error(format!("expected {what}"), stx.span()))
    }

    /// What `id` refers to: `None` if it is free.
    fn resolve(&self, id: &Syntax) -> Result<Option<Binding>> {
        match id.resolve(&self.context, &self.table, Phase(0)) {
            Resolution::Bound(b) => Ok(Some(b.clone())),
            Resolution::Unbound => Ok(None),
            Resolution::Ambiguous(_) => Err(error(
                format!(
                    "{} is ambiguous: more than one binding could be meant",
                    id.as_identifier().unwrap_or("this identifier")
                ),
                id.span(),
            )),
        }
    }

    /// The keyword `stx` uses, a core form or a macro, if it is a list
    /// whose head names one.
    fn form(&mut self, stx: &Syntax) -> Result<Option<(Binding, Vec<Syntax>)>> {
        if !stx.is_list() {
            return Ok(None);
        }
        let Exposed::List { items, tail } = self.expose(stx)? else {
            return Ok(None);
        };
        match items.first() {
            Some(head) if head.as_identifier().is_some() => match self.resolve(head)? {
                Some(Binding::Core(form)) if tail.is_none() => {
                    Ok(Some((Binding::Core(form), items)))
                }
                Some(Binding::Macro(m)) => Ok(Some((Binding::Macro(m), items))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// The core form `stx` is, if it is a list whose head names one.
    fn core_form(&mut self, stx: &Syntax) -> Result<Option<(Core, Vec<Syntax>)>> {
        Ok(match self.form(stx)? {
            Some((Binding::Core(form), items)) => Some((form, items)),
            _ => None,
        })
    }

    /// `id` without the use-site scopes of the body defining it.
    fn strip(&mut self, id: &Syntax, use_sites: &[Scope]) -> Result<Syntax> {
        let mut id = id.clone();
        for &scope in use_sites {
            id = id.remove(&mut self.context, scope).map_err(too_many)?;
        }
        Ok(id)
    }

    // Macros.

    /// Compiles a transformer, `(syntax-rules ...)`, for the macro `name`.
    fn transformer(&mut self, spec: &Syntax, name: &str) -> Result<usize> {
        let Some((Core::SyntaxRules, items)) = self.core_form(spec)? else {
            let procedure = self.procedure(spec, name)?;
            self.macros.push(Macro {
                name: name.to_owned(),
                transformer: Transformer::Procedure(procedure),
            });
            return Ok(self.macros.len() - 1);
        };
        // `(syntax-rules (literal ...) clause ...)`, or with a custom
        // ellipsis, `(syntax-rules ellipsis (literal ...) clause ...)`.
        let (ellipsis, rest) = match &items[1..] {
            [e, rest @ ..] if e.as_identifier().is_some() => {
                (e.as_identifier().expect("checked").to_owned(), rest)
            }
            rest => ("...".to_owned(), rest),
        };
        let Some((literals, clauses)) = rest.split_first() else {
            return Err(error(
                "syntax-rules: expected a list of literals",
                spec.span(),
            ));
        };
        let literals = self.items(literals, "a list of literals")?;
        if let Some(l) = literals.iter().find(|l| l.as_identifier().is_none()) {
            return Err(error(
                "syntax-rules: a literal must be an identifier",
                l.span(),
            ));
        }
        let spelling = Spelling {
            ellipsis: &ellipsis,
            literals: &literals,
        };
        let mut compiled = Vec::new();
        for clause in clauses {
            let parts = self.items(clause, "(pattern template)")?;
            let [pattern, template] = &parts[..] else {
                return Err(error(
                    "syntax-rules: expected (pattern template)",
                    clause.span(),
                ));
            };
            // The pattern's first element stands for the keyword and is
            // ignored: it is not a pattern variable.
            let Exposed::List {
                items: elements,
                tail,
            } = self.expose(pattern)?
            else {
                return Err(error(
                    "syntax-rules: a pattern must be a list",
                    pattern.span(),
                ));
            };
            if elements.is_empty() {
                return Err(error(
                    "syntax-rules: a pattern must start with the keyword",
                    pattern.span(),
                ));
            }
            let rest = Syntax::list(
                elements[1..].to_vec(),
                tail,
                pattern.scopes(),
                pattern.span(),
                None,
            );
            let mut vars = Vars::new();
            let mut compiled_pattern =
                pattern::pattern(&mut self.context, &spelling, &mut vars, &rest, 0)?;
            if let Pattern::List { before, .. } = &mut compiled_pattern {
                before.insert(0, Pattern::Discard);
            }
            let mut find = |_: &ScopeContext, id: &Syntax| vars.lookup(id);
            let template =
                pattern::template(&mut self.context, &spelling, &mut find, template, 0, false)?;
            compiled.push(Clause {
                pattern: compiled_pattern,
                template,
                depths: vars.depths(),
            });
        }
        self.macros.push(Macro {
            name: name.to_owned(),
            transformer: Transformer::Rules(compiled),
        });
        Ok(self.macros.len() - 1)
    }

    /// Expands, compiles and runs a transformer written in Scheme, at
    /// phase 1, returning the procedure it evaluates to.
    fn procedure(&mut self, spec: &Syntax, name: &str) -> Result<Handle> {
        let body = self.open_lambda(Some(name));
        self.floors.push(self.stack.len() - 1);
        let expr = self.named(spec, name);
        self.floors.pop();
        self.close_lambda(body, Vec::new(), None, expr?);
        let module = self.module(body, name)?;
        // Instantiate the libraries the transformer uses at phase 1.
        let failed = |e: vali_runtime::Error| error(format!("{name}: {e}"), spec.span());
        for variable in &module.variables {
            if let Link::Imported(identity) = &variable.link
                && let Some(unit) = self.units.iter().position(|u| *u.identity == **identity)
            {
                self.instantiate(unit).map_err(failed)?;
            }
        }
        let cx = self.phase1()?;
        let id = cx.load(module).map_err(failed)?;
        let procedure = cx.run(id).map_err(failed)?;
        if cx.get(&procedure).map_err(failed)?.tag() != vali_abi::Tag::Closure {
            return Err(error(
                format!("{name}: the transformer is not a procedure"),
                spec.span(),
            ));
        }
        Ok(procedure)
    }

    /// Loads and runs `unit` in the phase-1 isolate, after what it imports,
    /// unless it is there already: a fresh instance for this expansion
    /// (ADR 0001 #2).
    fn instantiate(&mut self, unit: usize) -> std::result::Result<(), vali_runtime::Error> {
        if self.phase1_units.contains(&unit) {
            return Ok(());
        }
        for dependency in self.units[unit].imports.clone() {
            self.instantiate(dependency)?;
        }
        let module = self.units[unit]
            .module
            .clone()
            .expect("an imported library is compiled before its importers");
        let identity = self.units[unit].identity.clone();
        let cx = self
            .phase1()
            .map_err(|e| vali_runtime::Error::scheme(e.message))?;
        let id = cx.load(module)?;
        cx.run(id)?;
        cx.register_library(&identity, id);
        self.phase1_units.insert(unit);
        Ok(())
    }

    /// The phase-1 isolate, made on first use.
    fn phase1(&mut self) -> Result<&mut Cx<NoCollection>> {
        if self.phase1.is_none() {
            let mut cx = phase::instance().map_err(|e| CompileError {
                message: format!("the expander's isolate: {e}"),
                span: None,
                location: None,
            })?;
            for (identity, library) in &self.natives {
                cx.register_native_library(identity, std::sync::Arc::clone(library));
            }
            self.phase1 = Some(cx);
        }
        Ok(self.phase1.as_mut().expect("just made"))
    }

    /// Calls a transformer written in Scheme on `input`, lending it the
    /// expander's scopes for the syntax procedures.
    fn run_transformer(
        &mut self,
        m: usize,
        procedure: &Handle,
        input: Syntax,
        form: &Syntax,
        origin: Arc<Origin>,
    ) -> Result<Syntax> {
        let name = self.macros[m].name.clone();
        let failed = |e: vali_runtime::Error| error(format!("{name}: {e}"), form.span());
        let shared = Shared {
            context: std::mem::replace(&mut self.context, ScopeContext::new()),
            table: std::mem::take(&mut self.table),
            compiled: std::mem::take(&mut self.compiled),
            origin: Some(origin),
            sources: self.sources.clone(),
        };
        let cx = self
            .phase1
            .as_mut()
            .expect("a procedure transformer has an isolate");
        // Allocate first, so a failure leaves the state where it was.
        let input = match cx.syntax(input) {
            Ok(input) => input,
            Err(e) => {
                self.context = shared.context;
                self.table = shared.table;
                self.compiled = shared.compiled;
                return Err(failed(e));
            }
        };
        cx.set_expander_data(Some(Box::new(shared)));
        let result = cx.call(procedure, &[input]);
        let shared = cx
            .set_expander_data(None)
            .and_then(|data| (data as Box<dyn std::any::Any>).downcast::<Shared>().ok())
            .expect("the expander's state comes back");
        self.context = shared.context;
        self.table = shared.table;
        self.compiled = shared.compiled;
        let output = result.map_err(failed)?;
        let cx = self.phase1.as_mut().expect("as above");
        let value = cx.get(&output).map_err(failed)?;
        // SAFETY: the handle keeps the value valid.
        unsafe { cx.value_syntax(value, ScopeSetId::EMPTY, form.span()) }.ok_or_else(|| {
            error(
                format!("{name}: the transformer returned something that is not syntax"),
                form.span(),
            )
        })
    }

    /// An error if `v`, a local variable, belongs to code outside the
    /// transformer being expanded: it doesn't exist when the transformer
    /// runs.
    fn in_context(&self, v: VarId, id: &Syntax) -> Result<()> {
        if let Some(&floor) = self.floors.last()
            && !self.stack[floor..].contains(&self.vars[v].binder)
        {
            return Err(error(
                format!(
                    "{}: used out of context: a macro's transformer cannot use a \
                     variable of the code around it",
                    self.vars[v].name
                ),
                id.span(),
            ));
        }
        Ok(())
    }

    /// A reference to the top-level variable `name` of unit `owner`.
    fn variable(&self, owner: usize, name: String) -> Expr {
        if owner == self.units.len() - 1 {
            Expr::Global(name)
        } else {
            Expr::Imported(self.units[owner].identity.clone(), name)
        }
    }

    /// An error if a transformer uses a variable of the unit being
    /// compiled, which runs only after the transformer has.
    fn global_in_context(&self, owner: usize, global: &str, id: &Syntax) -> Result<()> {
        if self.floors.is_empty() || owner != self.units.len() - 1 {
            return Ok(());
        }
        Err(error(
            format!(
                "{global}: defined by the program, so it does not exist yet when \
                 a macro's transformer runs"
            ),
            id.span(),
        ))
    }

    /// One macro step: flips a fresh macro scope on the use, adds
    /// `use_site` if the use is in a definition context, transforms it,
    /// and flips the macro scope again on the output, so what the macro
    /// introduced has the scope and what it passed through doesn't.
    /// `chain` is how many macro steps produced `form` in a body.
    fn apply_macro(
        &mut self,
        m: usize,
        form: &Syntax,
        use_site: Option<Scope>,
        chain: usize,
    ) -> Result<Syntax> {
        if self.depth + chain >= MAX_MACRO_DEPTH {
            return Err(error(
                format!(
                    "{}: too many macro steps; does the macro expand into itself forever?",
                    self.macros[m].name
                ),
                form.span(),
            ));
        }
        let scope = self.scope(ScopeKind::Macro)?;
        let mut input = form.flip(&mut self.context, scope).map_err(too_many)?;
        if let Some(use_site) = use_site {
            input = self.add(&input, use_site)?;
        }
        let origin = Arc::new(Origin {
            macro_name: self.macros[m].name.as_str().into(),
            use_site: form.span(),
            parent: form.origin().cloned(),
        });
        if let Transformer::Procedure(procedure) = &self.macros[m].transformer {
            let procedure = procedure.clone();
            let output = self.run_transformer(m, &procedure, input, form, origin)?;
            return output.flip(&mut self.context, scope).map_err(too_many);
        }
        let table = &self.table;
        let same = |cx: &ScopeContext, a: &Syntax, b: &Syntax| {
            free_identifier_eq(cx, table, Phase(0), a, b)
        };
        let mac = &self.macros[m];
        let Transformer::Rules(clauses) = &mac.transformer else {
            unreachable!("procedures were handled above")
        };
        for clause in clauses {
            let mut binds: Vec<Option<Match>> = vec![None; clause.depths.len()];
            if pattern::matches(
                &mut self.context,
                &same,
                &clause.pattern,
                &input,
                &mut binds,
            )? {
                let builder = Builder {
                    depths: clause.depths.clone(),
                    origin: Some(&origin),
                    span: form.span(),
                };
                let output = builder.build(&clause.template, &mut binds)?;
                return output.flip(&mut self.context, scope).map_err(too_many);
            }
        }
        Err(error(
            format!("{}: no pattern matches this use", mac.name),
            form.span(),
        ))
    }

    /// Expands a macro use as an expression.
    fn macro_expr(&mut self, m: usize, form: &Syntax, name: Option<&str>) -> Result<Expr> {
        let output = self.apply_macro(m, form, None, 0)?;
        self.depth += 1;
        let result = match name {
            Some(name) => self.named(&output, name),
            None => self.expr(&output),
        };
        self.depth -= 1;
        result
    }

    /// `(let-syntax ((id transformer) ...) body ...)`, and
    /// `letrec-syntax`, whose transformers see their own bindings.
    fn let_syntax(
        &mut self,
        keyword: &str,
        args: &[Syntax],
        recursive: bool,
        span: Span,
    ) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            ));
        };
        let bindings = self.bindings(bindings, keyword)?;
        let scope = self.scope(ScopeKind::Binder)?;
        for (id, spec) in &bindings {
            let spec = if recursive {
                self.add(spec, scope)?
            } else {
                spec.clone()
            };
            let name = id.as_identifier().expect("checked").to_owned();
            let m = self.transformer(&spec, &name)?;
            let id = self.add(id, scope)?;
            self.bind(&id, Binding::Macro(m), keyword)?;
        }
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        self.body(body, false, span)
    }

    // Scopes and bindings.

    /// Binds `id`, refusing a second binding with the same scopes.
    fn bind(&mut self, id: &Syntax, binding: Binding, what: &str) -> Result<()> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error(format!("{what}: expected a name"), id.span()))?;
        self.table
            .bind(
                &self.context,
                name.to_owned(),
                id.scopes(),
                Phases::All,
                binding,
            )
            .map_err(|_| error(format!("{what}: {name} is bound twice"), id.span()))
    }

    /// A new local variable bound to `id` in the current `lambda`.
    fn bind_local(&mut self, id: &Syntax, what: &str) -> Result<VarId> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error(format!("{what}: expected a name"), id.span()))?
            .to_owned();
        self.vars.push(VarInfo {
            name,
            binder: self.current(),
            assigned: false,
            captured: false,
        });
        let v = self.vars.len() - 1;
        self.bind(id, Binding::Local(v), what)?;
        Ok(v)
    }

    // Lambdas.

    fn current(&self) -> LambdaId {
        *self.stack.last().expect("inside a lambda")
    }

    fn open_lambda(&mut self, name: Option<&str>) -> LambdaId {
        self.lambdas.push(Lambda {
            name: name.map(str::to_owned),
            required: Vec::new(),
            rest: None,
            body: Expr::Unspecified,
            free: Vec::new(),
        });
        let id = self.lambdas.len() - 1;
        self.stack.push(id);
        id
    }

    fn close_lambda(
        &mut self,
        id: LambdaId,
        required: Vec<VarId>,
        rest: Option<VarId>,
        body: Expr,
    ) {
        self.stack.pop();
        let lambda = &mut self.lambdas[id];
        lambda.required = required;
        lambda.rest = rest;
        lambda.body = body;
    }

    /// A reference to `v`, recording captures and free variables.
    fn local(&mut self, v: VarId) -> Expr {
        let binder = self.vars[v].binder;
        if binder != self.current() {
            self.vars[v].captured = true;
            let from = self
                .stack
                .iter()
                .position(|&l| l == binder)
                .expect("the binder is an enclosing lambda");
            for &l in &self.stack[from + 1..] {
                if !self.lambdas[l].free.contains(&v) {
                    self.lambdas[l].free.push(v);
                }
            }
        }
        Expr::Local(v)
    }

    // Bodies.

    /// A body: definitions, then expressions. At top level, definitions
    /// make module variables and may come between expressions; elsewhere
    /// they make local variables, all before the first expression.
    fn body(&mut self, forms: Vec<Syntax>, top: bool, span: Span) -> Result<Expr> {
        // Each form with the number of macro steps that produced it.
        let mut queue: VecDeque<(Syntax, usize)> = if top {
            forms.into_iter().map(|f| (f, 0)).collect()
        } else {
            // Definitions in a body shadow the parameters around it.
            let scope = self.scope(ScopeKind::Binder)?;
            forms
                .iter()
                .map(|f| Ok((self.add(f, scope)?, 0)))
                .collect::<Result<_>>()?
        };
        let mut items = Vec::new();
        let mut use_sites = Vec::new();
        while let Some((form, chain)) = queue.pop_front() {
            let definitions_end = !top && items.iter().any(|i| matches!(i, Item::Expr(_)));
            match self.form(&form)? {
                Some((Binding::Core(Core::Begin), parts)) => {
                    for part in parts.into_iter().skip(1).rev() {
                        queue.push_front((part, chain));
                    }
                }
                // Spliced into the body, as `begin` is.
                Some((Binding::Core(core @ (Core::Include | Core::IncludeCi)), parts)) => {
                    let forms = self.included(&parts, core == Core::IncludeCi, form.span())?;
                    for part in forms.into_iter().rev() {
                        queue.push_front((part, chain));
                    }
                }
                Some((Binding::Core(Core::CondExpand), parts)) => {
                    for part in self.cond_expand(&parts, form.span())?.into_iter().rev() {
                        queue.push_front((part, chain));
                    }
                }
                Some((Binding::Macro(m), _)) => {
                    let use_site = self.scope(ScopeKind::UseSite)?;
                    use_sites.push(use_site);
                    let output = self.apply_macro(m, &form, Some(use_site), chain)?;
                    queue.push_front((output, chain + 1));
                }
                Some((Binding::Core(Core::DefineSyntax), parts)) => {
                    if definitions_end {
                        return Err(error(
                            "define-syntax: definitions must come before a body's expressions",
                            form.span(),
                        ));
                    }
                    let [_, id, spec] = &parts[..] else {
                        return Err(error(
                            "define-syntax: expected a name and a transformer",
                            form.span(),
                        ));
                    };
                    let id = self.strip(id, &use_sites)?;
                    let name = id
                        .as_identifier()
                        .ok_or_else(|| error("define-syntax: expected a name", id.span()))?
                        .to_owned();
                    let m = self.transformer(spec, &name)?;
                    if top && self.in_repl() {
                        // A REPL may define a name again, as anything.
                        self.table.rebind(
                            &self.context,
                            name,
                            id.scopes(),
                            Phases::All,
                            Binding::Macro(m),
                        );
                    } else {
                        self.bind(&id, Binding::Macro(m), "define-syntax")?;
                    }
                }
                Some((Binding::Core(Core::Define), parts)) => {
                    if definitions_end {
                        return Err(error(
                            "define: definitions must come before a body's expressions",
                            form.span(),
                        ));
                    }
                    let (id, value) = self.definition(&form, &parts)?;
                    let id = self.strip(&id, &use_sites)?;
                    items.push(if top && self.in_repl() {
                        Item::Repl(self.define_repl(&id)?, value)
                    } else if top {
                        Item::Global(self.define_global(&id)?, value)
                    } else {
                        Item::Local(self.bind_local(&id, "define")?, value)
                    });
                }
                _ => items.push(Item::Expr(form)),
            }
        }
        if top {
            let mut exprs = Vec::new();
            for item in items {
                exprs.push(match item {
                    Item::Global(name, value) => {
                        let value = match value {
                            Some(v) => self.named(&v, &name)?,
                            None => Expr::Unspecified,
                        };
                        Expr::SetGlobal(name, Box::new(value))
                    }
                    Item::Repl(global, value) => {
                        let name = global.strip_prefix("repl:").unwrap_or(&global).to_owned();
                        let value = match value {
                            Some(v) => self.named(&v, &name)?,
                            None => Expr::Unspecified,
                        };
                        Expr::SetFree(global, Box::new(value))
                    }
                    Item::Expr(e) => self.expr(&e)?,
                    Item::Local(..) => unreachable!("top-level definitions are global"),
                });
            }
            if exprs.is_empty() {
                exprs.push(Expr::Unspecified);
            }
            return Ok(Expr::Begin(exprs));
        }
        let mut bindings = Vec::new();
        let mut exprs = Vec::new();
        for item in items {
            match item {
                Item::Local(v, value) => {
                    let name = self.vars[v].name.clone();
                    let value = match value {
                        Some(value) => self.named(&value, &name)?,
                        None => Expr::Unspecified,
                    };
                    bindings.push((v, value));
                }
                Item::Expr(e) => exprs.push(e),
                Item::Global(..) | Item::Repl(..) => unreachable!("body definitions are local"),
            }
        }
        if exprs.is_empty() {
            return Err(error("a body needs at least one expression", span));
        }
        let body = self.sequence(&exprs, span)?;
        Ok(if bindings.is_empty() {
            body
        } else {
            Expr::Letrec(bindings, Box::new(body))
        })
    }

    /// A top-level definition's module variable. One the program wrote
    /// keeps its name; one a macro introduced gets a fresh name, so it
    /// cannot collide with the program's.
    fn define_global(&mut self, id: &Syntax) -> Result<String> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error("define: expected a name", id.span()))?;
        let global = if id.scopes() == self.top {
            name.to_owned()
        } else {
            self.fresh += 1;
            format!("{name}~{}", self.fresh)
        };
        let unit = self.units.len() - 1;
        match self.table.bind(
            &self.context,
            name.to_owned(),
            id.scopes(),
            Phases::All,
            Binding::Variable(unit, global.clone()),
        ) {
            Ok(()) => {}
            // A redefinition at top level assigns the same variable.
            Err(AlreadyBound {
                existing: Binding::Variable(owner, existing),
            }) if *owner == unit => return Ok(existing.clone()),
            Err(AlreadyBound { .. }) => {
                return Err(error(
                    format!("define: {name} is imported, and cannot be redefined"),
                    id.span(),
                ));
            }
        }
        Ok(global)
    }

    /// `(define id)`, `(define id value)` or `(define (id . formals) body
    /// ...)`: the name and the value, if any.
    fn definition(&mut self, form: &Syntax, parts: &[Syntax]) -> Result<(Syntax, Option<Syntax>)> {
        match parts {
            [_, id] if id.as_identifier().is_some() => Ok((id.clone(), None)),
            [_, id, value] if id.as_identifier().is_some() => Ok((id.clone(), Some(value.clone()))),
            [keyword, target, body @ ..] if target.is_list() => {
                let Exposed::List { items, tail } = self.expose(target)? else {
                    unreachable!("a list")
                };
                let Some((id, params)) = items.split_first() else {
                    return Err(error("define: expected a name", target.span()));
                };
                // `(lambda formals body ...)`, with `lambda` meaning the
                // core form whatever the program binds.
                let lambda = Syntax::identifier("lambda", self.core, keyword.span(), None);
                let formals = Syntax::list(
                    params.to_vec(),
                    tail,
                    target.scopes(),
                    target.span(),
                    target.origin().cloned(),
                );
                let mut elements = vec![lambda, formals];
                elements.extend(body.iter().cloned());
                let value = Syntax::list(
                    elements,
                    None,
                    form.scopes(),
                    form.span(),
                    form.origin().cloned(),
                );
                Ok((id.clone(), Some(value)))
            }
            _ => Err(error(
                "define: expected a name and at most one expression",
                form.span(),
            )),
        }
    }

    fn sequence(&mut self, forms: &[Syntax], span: Span) -> Result<Expr> {
        if forms.is_empty() {
            return Err(error("expected at least one expression", span));
        }
        let mut exprs = forms
            .iter()
            .map(|f| self.expr(f))
            .collect::<Result<Vec<_>>>()?;
        Ok(if exprs.len() == 1 {
            exprs.pop().expect("one")
        } else {
            Expr::Begin(exprs)
        })
    }

    // Expressions.

    /// Expands `stx`, naming it `name` if it is a `lambda`, or a macro
    /// use that expands into one.
    fn named(&mut self, stx: &Syntax, name: &str) -> Result<Expr> {
        match self.form(stx)? {
            Some((Binding::Core(Core::Lambda), parts)) => {
                self.lambda(Some(name), &parts, stx.span())
            }
            Some((Binding::Macro(m), _)) => self.macro_expr(m, stx, Some(name)),
            _ => self.expr(stx),
        }
    }

    /// Expands an expression. Deeply nested code recurses deeply, so the
    /// stack grows on demand.
    pub(crate) fn expr(&mut self, stx: &Syntax) -> Result<Expr> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.expr_here(stx))
    }

    fn expr_here(&mut self, stx: &Syntax) -> Result<Expr> {
        if let Some(name) = stx.as_identifier() {
            return match self.resolve(stx)? {
                Some(Binding::Local(v)) => {
                    self.in_context(v, stx)?;
                    Ok(self.local(v))
                }
                Some(Binding::Variable(owner, global)) => {
                    self.global_in_context(owner, &global, stx)?;
                    Ok(self.variable(owner, global))
                }
                Some(Binding::Primitive(name)) => Ok(Expr::Free(name)),
                Some(Binding::Global(name)) => Ok(Expr::ReplGlobal(name)),
                None if self.in_repl() => Ok(Expr::ReplGlobal(repl_global(name))),
                None if self.units.last().is_some_and(|u| u.strict) => Err(error(
                    format!("{name} is not bound: is it imported?"),
                    stx.span(),
                )),
                // A program with no imports still reaches every primitive
                // by name, until programs are strict too.
                None if vali_runtime::primitive_names().contains(&name) => {
                    Ok(Expr::Free(name.to_owned()))
                }
                None => Ok(Expr::ReplGlobal(name.to_owned())),
                Some(Binding::Core(_) | Binding::Macro(_)) => Err(error(
                    format!("{name}: a syntactic keyword is not an expression"),
                    stx.span(),
                )),
                Some(Binding::PatternVar(..)) => Err(error(
                    format!("{name}: a pattern variable can only be used in a template"),
                    stx.span(),
                )),
            };
        }
        if let Some(atom) = stx.as_atom() {
            return match atom {
                // SRFI 88: a keyword evaluates to itself.
                DatumKind::Labelled(..) | DatumKind::LabelRef(_) => Err(error(
                    "datum labels are only allowed inside quoted data",
                    stx.span(),
                )),
                _ => Ok(Expr::Const(stx.to_datum())),
            };
        }
        let span = stx.span();
        let (items, tail) = match self.expose(stx)? {
            Exposed::List { items, tail } => (items, tail),
            // Vectors are self-evaluating (R7RS 4.1.2).
            _ => return Ok(Expr::Const(stx.to_datum())),
        };
        if tail.is_some() {
            return Err(error("a dotted list is not an expression", span));
        }
        let Some(head) = items.first() else {
            return Err(error("() is not an expression; quote it", span));
        };
        if head.as_identifier().is_some() {
            match self.resolve(head)? {
                Some(Binding::Core(form)) => {
                    let keyword = head.as_identifier().expect("an identifier").to_owned();
                    return self.core(form, &keyword, &items, span);
                }
                Some(Binding::Macro(m)) => return self.macro_expr(m, stx, None),
                _ => {}
            }
        }
        if items.len() - 1 > usize::from(u8::MAX) - 4 {
            return Err(error("too many arguments in one call", span));
        }
        let f = self.expr(head)?;
        let args = items[1..]
            .iter()
            .map(|a| self.expr(a))
            .collect::<Result<Vec<_>>>()?;
        Ok(Expr::Call(Box::new(f), args, span))
    }

    fn core(&mut self, form: Core, keyword: &str, items: &[Syntax], span: Span) -> Result<Expr> {
        let args = &items[1..];
        match form {
            Core::Quote => match args {
                [datum] => Ok(Expr::Const(datum.to_datum())),
                _ => Err(error(format!("{keyword}: expected one datum"), span)),
            },
            Core::If => {
                let (c, t, e) = match args {
                    [c, t] => (c, t, None),
                    [c, t, e] => (c, t, Some(e)),
                    _ => {
                        return Err(error(
                            format!("{keyword}: expected a test and one or two branches"),
                            span,
                        ));
                    }
                };
                let c = self.expr(c)?;
                let t = self.expr(t)?;
                let e = match e {
                    Some(e) => self.expr(e)?,
                    None => Expr::Unspecified,
                };
                Ok(Expr::If(Box::new(c), Box::new(t), Box::new(e)))
            }
            Core::Begin => self.sequence(args, span),
            Core::Include | Core::IncludeCi => {
                let forms = self.included(items, form == Core::IncludeCi, span)?;
                self.sequence(&forms, span)
            }
            Core::CondExpand => {
                let forms = self.cond_expand(items, span)?;
                if forms.is_empty() {
                    Ok(Expr::Unspecified)
                } else {
                    self.sequence(&forms, span)
                }
            }
            Core::Lambda => self.lambda(None, items, span),
            Core::Let => self.let_(keyword, args, span),
            Core::Letrec => self.letrec(keyword, args, span),
            Core::Set => {
                let [target, value] = args else {
                    return Err(error(
                        format!("{keyword}: expected a variable and an expression"),
                        span,
                    ));
                };
                let Some(name) = target.as_identifier() else {
                    return Err(error(
                        format!("{keyword}: expected a variable"),
                        target.span(),
                    ));
                };
                let name = name.to_owned();
                match self.resolve(target)? {
                    Some(Binding::Local(v)) => {
                        self.in_context(v, target)?;
                        let value = self.expr(value)?;
                        self.vars[v].assigned = true;
                        let Expr::Local(v) = self.local(v) else {
                            unreachable!("a local")
                        };
                        Ok(Expr::SetLocal(v, Box::new(value)))
                    }
                    Some(Binding::Variable(owner, global)) => {
                        self.global_in_context(owner, &global, target)?;
                        if owner != self.units.len() - 1 {
                            return Err(error(
                                format!("{keyword}: {name} is imported, and cannot be assigned"),
                                target.span(),
                            ));
                        }
                        let value = self.expr(value)?;
                        Ok(Expr::SetGlobal(global, Box::new(value)))
                    }
                    Some(Binding::Global(global)) => {
                        let value = self.expr(value)?;
                        Ok(Expr::SetFree(global, Box::new(value)))
                    }
                    None if self.in_repl() => {
                        let value = self.expr(value)?;
                        Ok(Expr::SetFree(repl_global(&name), Box::new(value)))
                    }
                    Some(Binding::Primitive(_)) => Err(error(
                        format!("{keyword}: {name} is imported, and cannot be assigned"),
                        target.span(),
                    )),
                    Some(Binding::Core(_) | Binding::Macro(_) | Binding::PatternVar(..)) => {
                        Err(error(
                            format!("{keyword}: {name} is a syntactic keyword"),
                            target.span(),
                        ))
                    }
                    None => Err(error(
                        format!("{keyword}: {name} is not defined"),
                        target.span(),
                    )),
                }
            }
            Core::Define | Core::DefineSyntax => Err(error(
                format!("{keyword} is only allowed at top level or at the start of a body"),
                span,
            )),
            Core::LetSyntax => self.let_syntax(keyword, args, false, span),
            Core::LetrecSyntax => self.let_syntax(keyword, args, true, span),
            Core::SyntaxRules => Err(error(
                format!("{keyword} is only allowed as a macro's transformer"),
                span,
            )),
            Core::QuoteSyntax => {
                let [form] = args else {
                    return Err(error(format!("{keyword}: expected one form"), span));
                };
                self.compiled.quoted.push(form.clone());
                let index = number(self.compiled.quoted.len() - 1, span);
                Ok(Expr::Call(
                    Box::new(Expr::Free(QUOTE_SYNTAX.to_owned())),
                    vec![index],
                    span,
                ))
            }
            Core::SyntaxCase => self.syntax_case(keyword, args, span),
            Core::Syntax => self.syntax_template(keyword, args, span),
        }
    }

    // syntax-case.

    /// The scopes, bindings and compiled syntax so far, for syntax objects
    /// at run time (ADR 0015 #10).
    pub(crate) fn syntax_data(&self) -> Shared {
        Shared {
            context: self.context.clone(),
            table: self.table.clone(),
            compiled: self.compiled.clone(),
            origin: None,
            sources: self.sources.clone(),
        }
    }

    /// A local variable of the current `lambda` that no identifier names.
    fn hidden_local(&mut self, name: &str) -> VarId {
        self.vars.push(VarInfo {
            name: name.to_owned(),
            binder: self.current(),
            assigned: false,
            captured: false,
        });
        self.vars.len() - 1
    }

    /// `(syntax-case input (literal ...) clause ...)`: the input in a
    /// hidden local, then each clause in turn.
    fn syntax_case(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        let [input, literals, clauses @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected an input, a list of literals and clauses"),
                span,
            ));
        };
        let literals = self.items(literals, "a list of literals")?;
        if let Some(l) = literals.iter().find(|l| l.as_identifier().is_none()) {
            return Err(error(
                format!("{keyword}: a literal must be an identifier"),
                l.span(),
            ));
        }
        let input = self.expr(input)?;
        let tmp = self.hidden_local(keyword);
        let clauses = self.clauses(keyword, tmp, &literals, clauses, span)?;
        Ok(Expr::Let(vec![(tmp, input)], Box::new(clauses)))
    }

    /// The first of `clauses`, falling through to the rest. A clause binds
    /// its pattern variables, in a scope its fender and output have, to
    /// hidden locals holding what each matched. With a fender, the rest is
    /// a procedure, called if either the pattern or the fender fails, so it
    /// is compiled once.
    fn clauses(
        &mut self,
        keyword: &str,
        tmp: VarId,
        literals: &[Syntax],
        clauses: &[Syntax],
        span: Span,
    ) -> Result<Expr> {
        let Some((clause, rest)) = clauses.split_first() else {
            // No clause matched.
            let input = self.local(tmp);
            return Ok(Expr::Call(
                Box::new(Expr::Free("syntax-violation".to_owned())),
                vec![
                    Expr::Const(Datum {
                        kind: DatumKind::Boolean(false),
                        span,
                    }),
                    Expr::Const(Datum {
                        kind: DatumKind::String("invalid syntax".to_owned()),
                        span,
                    }),
                    input,
                ],
                span,
            ));
        };
        let parts = self.items(clause, "(pattern [fender] output)")?;
        let (pattern, fender, output) = match &parts[..] {
            [pattern, output] => (pattern, None, output),
            [pattern, fender, output] => (pattern, Some(fender), output),
            _ => {
                return Err(error(
                    format!("{keyword}: expected (pattern [fender] output)"),
                    clause.span(),
                ));
            }
        };
        let spelling = Spelling {
            ellipsis: "...",
            literals,
        };
        let mut vars = Vars::new();
        let compiled = pattern::pattern(&mut self.context, &spelling, &mut vars, pattern, 0)?;
        self.compiled.patterns.push((compiled, vars.len()));
        let k = self.compiled.patterns.len() - 1;

        // What follows if this clause fails: the rest, inline, or with a
        // fender, a procedure that runs it.
        let (rest, fail) = if fender.is_some() {
            let thunk = self.open_lambda(None);
            let body = self.clauses(keyword, tmp, literals, rest, span)?;
            self.close_lambda(thunk, Vec::new(), None, body);
            let fail = self.hidden_local("fail");
            (None, Some((fail, Expr::Lambda(thunk))))
        } else {
            (
                Some(self.clauses(keyword, tmp, literals, rest, span)?),
                None,
            )
        };
        let otherwise = |this: &mut Self| match &fail {
            Some((f, _)) => Expr::Call(Box::new(this.local(*f)), Vec::new(), span),
            None => unreachable!("only with a fender"),
        };

        let scope = self.scope(ScopeKind::Binder)?;
        let m = self.hidden_local("match");
        let mut binds = Vec::new();
        let pattern_vars: Vec<(Syntax, usize)> = vars.iter().cloned().collect();
        for (i, (id, depth)) in pattern_vars.into_iter().enumerate() {
            let id = self.add(&id, scope)?;
            let name = id
                .as_identifier()
                .expect("pattern variables are identifiers");
            let v = self.hidden_local(name);
            self.bind(&id, Binding::PatternVar(v, depth), keyword)?;
            let element = Expr::Call(
                Box::new(Expr::Free("vector-ref".to_owned())),
                vec![Expr::Local(m), number(i, span)],
                span,
            );
            binds.push((v, element));
        }
        let output = self.add(output, scope)?;
        let output = self.expr(&output)?;
        let body = match fender {
            Some(fender) => {
                let fender = self.add(fender, scope)?;
                let fender = self.expr(&fender)?;
                Expr::If(
                    Box::new(fender),
                    Box::new(output),
                    Box::new(otherwise(self)),
                )
            }
            None => output,
        };
        let input = self.local(tmp);
        let matched = Expr::Call(
            Box::new(Expr::Free(SYNTAX_MATCH.to_owned())),
            vec![input, number(k, span)],
            span,
        );
        let unmatched = match rest {
            Some(rest) => rest,
            None => otherwise(self),
        };
        let attempt = Expr::Let(
            vec![(m, matched)],
            Box::new(Expr::If(
                Box::new(Expr::Local(m)),
                Box::new(Expr::Let(binds, Box::new(body))),
                Box::new(unmatched),
            )),
        );
        Ok(match fail {
            Some((fail, thunk)) => Expr::Let(vec![(fail, thunk)], Box::new(attempt)),
            None => attempt,
        })
    }

    /// `(syntax template)`: a call building the template, with the matches
    /// of the pattern variables it uses, found by resolving its
    /// identifiers, so a template inside a nested `lambda` still sees its
    /// clause's variables.
    fn syntax_template(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        let [form] = args else {
            return Err(error(format!("{keyword}: expected one template"), span));
        };
        let spelling = Spelling {
            ellipsis: "...",
            literals: &[],
        };
        let mut used: Vec<(VarId, usize)> = Vec::new();
        let table = &self.table;
        let mut find = |cx: &ScopeContext, id: &Syntax| match id.resolve(cx, table, Phase(0)) {
            Resolution::Bound(&Binding::PatternVar(v, depth)) => {
                let i = used.iter().position(|&(u, _)| u == v).unwrap_or_else(|| {
                    used.push((v, depth));
                    used.len() - 1
                });
                Some((i, depth))
            }
            _ => None,
        };
        let template = pattern::template(&mut self.context, &spelling, &mut find, form, 0, false)?;
        if used.len() > usize::from(u8::MAX) - 5 {
            return Err(error(
                format!("{keyword}: a template may use at most 250 pattern variables"),
                span,
            ));
        }
        let depths = used.iter().map(|&(_, d)| d).collect();
        self.compiled.templates.push((template, depths));
        let mut args = vec![number(self.compiled.templates.len() - 1, span)];
        for (v, _) in used {
            self.in_context(v, form)?;
            args.push(self.local(v));
        }
        Ok(Expr::Call(
            Box::new(Expr::Free(SYNTAX_BUILD.to_owned())),
            args,
            span,
        ))
    }

    /// `(lambda formals body ...)`, from its items.
    fn lambda(&mut self, name: Option<&str>, items: &[Syntax], span: Span) -> Result<Expr> {
        let [keyword, formals, body @ ..] = items else {
            let keyword = items
                .first()
                .and_then(Syntax::as_identifier)
                .unwrap_or("lambda");
            return Err(error(
                format!("{keyword}: expected parameters and a body"),
                span,
            ));
        };
        let keyword = keyword.as_identifier().unwrap_or("lambda").to_owned();
        let id = self.open_lambda(name);
        let scope = self.scope(ScopeKind::Binder)?;
        let formals = self.add(formals, scope)?;
        let (params, rest) = if formals.as_identifier().is_some() {
            (Vec::new(), Some(formals))
        } else if formals.is_list() {
            let Exposed::List { items, tail } = self.expose(&formals)? else {
                unreachable!("a list")
            };
            (items, tail)
        } else {
            return Err(error(
                format!("{keyword}: expected parameters"),
                formals.span(),
            ));
        };
        if params.len() > usize::from(u8::MAX) {
            return Err(error(format!("{keyword}: too many parameters"), span));
        }
        let required = params
            .iter()
            .map(|p| self.bind_local(p, &keyword))
            .collect::<Result<Vec<_>>>()?;
        let rest = rest.map(|r| self.bind_local(&r, &keyword)).transpose()?;
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        let body = self.body(body, false, span)?;
        self.close_lambda(id, required, rest, body);
        Ok(Expr::Lambda(id))
    }

    /// The `(id init)` pairs of a binding list.
    fn bindings(&mut self, list: &Syntax, keyword: &str) -> Result<Vec<(Syntax, Syntax)>> {
        let list = self
            .items(list, "bindings")
            .map_err(|e| error(format!("{keyword}: {}", e.message), list.span()))?;
        list.iter()
            .map(
                |b| match b.list_items(&mut self.context).map_err(too_many)? {
                    Some(pair) if pair.len() == 2 && pair[0].as_identifier().is_some() => {
                        Ok((pair[0].clone(), pair[1].clone()))
                    }
                    _ => Err(error(
                        format!("{keyword}: expected (name expression)"),
                        b.span(),
                    )),
                },
            )
            .collect()
    }

    /// `(let ((id init) ...) body ...)` and named `let`.
    fn let_(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        match args {
            [name, bindings, body @ ..] if name.as_identifier().is_some() => {
                // Named let: a loop procedure, called with the inits, which
                // are expanded outside its scope.
                let bindings = self.bindings(bindings, keyword)?;
                let inits = bindings
                    .iter()
                    .map(|(_, init)| self.expr(init))
                    .collect::<Result<Vec<_>>>()?;
                let scope = self.scope(ScopeKind::Binder)?;
                let id = self.add(name, scope)?;
                let v = self.bind_local(&id, keyword)?;
                let params = bindings.into_iter().map(|(p, _)| p).collect();
                let formals = Syntax::list(params, None, name.scopes(), span, None);
                let mut lambda = vec![Syntax::identifier("lambda", self.core, span, None), formals];
                lambda.extend(body.iter().cloned());
                let lambda = Syntax::list(lambda, None, name.scopes(), span, None);
                let lambda = self.add(&lambda, scope)?;
                let Some((Core::Lambda, items)) = self.core_form(&lambda)? else {
                    unreachable!("built as a lambda")
                };
                let name = self.vars[v].name.clone();
                let lambda = self.lambda(Some(&name), &items, span)?;
                Ok(Expr::Letrec(
                    vec![(v, lambda)],
                    Box::new(Expr::Call(Box::new(Expr::Local(v)), inits, span)),
                ))
            }
            [bindings, body @ ..] => {
                let bindings = self.bindings(bindings, keyword)?;
                let mut inits = Vec::new();
                for (id, init) in &bindings {
                    let name = id.as_identifier().expect("checked").to_owned();
                    inits.push(self.named(init, &name)?);
                }
                let scope = self.scope(ScopeKind::Binder)?;
                let mut vars = Vec::new();
                for (id, _) in &bindings {
                    let id = self.add(id, scope)?;
                    vars.push(self.bind_local(&id, keyword)?);
                }
                let body = body
                    .iter()
                    .map(|f| self.add(f, scope))
                    .collect::<Result<Vec<_>>>()?;
                let body = self.body(body, false, span)?;
                Ok(Expr::Let(
                    vars.into_iter().zip(inits).collect(),
                    Box::new(body),
                ))
            }
            [] => Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            )),
        }
    }

    /// `(letrec ((id init) ...) body ...)`, and `letrec*`: each init sees
    /// every variable, and they run in order.
    fn letrec(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            ));
        };
        let bindings = self.bindings(bindings, keyword)?;
        let scope = self.scope(ScopeKind::Binder)?;
        let mut vars = Vec::new();
        for (id, _) in &bindings {
            let id = self.add(id, scope)?;
            vars.push(self.bind_local(&id, keyword)?);
        }
        let mut inits = Vec::new();
        for (&v, (_, init)) in vars.iter().zip(&bindings) {
            let init = self.add(init, scope)?;
            let name = self.vars[v].name.clone();
            inits.push(self.named(&init, &name)?);
        }
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        let body = self.body(body, false, span)?;
        Ok(Expr::Letrec(
            vars.into_iter().zip(inits).collect(),
            Box::new(body),
        ))
    }
}
