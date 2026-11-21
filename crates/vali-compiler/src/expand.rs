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
    AlreadyBound, BindingTable, Datum, DatumKind, Exposed, Origin, Phase, Phases, Remap,
    Resolution, Scope, ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax, TooManyScopes,
    free_identifier_eq,
};

use vali_bytecode::{Converted, Link, Module, ModuleBuilder, validate};
use vali_dist::Digest;
use vali_runtime::{Cx, DeferredHeadered, Handle};

mod artifact;
mod context;
mod published;
mod quasiquote;
mod transformers;
mod units;

pub(crate) use context::merge_syntax;
pub(crate) use context::optimising;
use context::*;
use transformers::*;
use units::*;
pub(crate) use units::{build_stamp, decode_library_artifact};

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

/// The internal primitives that Vali's own exported macros expand into, so
/// that code a user's library gets from them links them. Only these, besides
/// the primitives syntax-case uses in a transformer, may a library that is
/// not Vali's own link (a test checks that the macros in `lib/` emit no
/// others). `define-record-type` in `lib/scheme/base.sld` is the macro that
/// emits them. Each checks all of its arguments, since an artifact can call
/// them directly.
#[doc(hidden)]
pub const MACRO_PRIMITIVES: &[&str] = &[
    "%make-record-type",
    "%record-constructor-indices",
    "%make-record",
    "%record?",
    "%record-field-index",
    "%record-ref",
    "%record-set!",
];

/// The highest phase a transformer written in Scheme may run at: how many
/// transformers may be nested in each other's code. A phase has an isolate
/// of its own, made when it is first used, so a phase that artifact bytes
/// name is not trusted to be anything near what memory allows. Real macros
/// reach phase 1 or 2.
pub(crate) const MAX_PHASE: usize = 16;

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
    LetStar,
    Letrec,
    Set,
    Define,
    DefineSyntax,
    LetSyntax,
    LetrecSyntax,
    SyntaxRules,
    QuoteSyntax,
    Quasiquote,
    Unquote,
    UnquoteSplicing,
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
    ("let*", Core::LetStar),
    ("letrec", Core::Letrec),
    ("letrec*", Core::Letrec),
    ("set!", Core::Set),
    ("define", Core::Define),
    ("define-syntax", Core::DefineSyntax),
    ("let-syntax", Core::LetSyntax),
    ("letrec-syntax", Core::LetrecSyntax),
    ("syntax-rules", Core::SyntaxRules),
    ("quote-syntax", Core::QuoteSyntax),
    ("quasiquote", Core::Quasiquote),
    ("unquote", Core::Unquote),
    ("unquote-splicing", Core::UnquoteSplicing),
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
    /// A macro, by the unit that defined it and its number there: the same
    /// macro reached through different imports is the same binding.
    Macro(MacroId),
    /// A REPL's top-level variable: the isolate's global of that name,
    /// bound late, and never inlined.
    Global(String),
    /// A `syntax-case` pattern variable: the hidden local holding what it
    /// matched, and its depth.
    PatternVar(VarId, usize),
}

/// A macro: the unit that defined it, by its index in the build, and its
/// number among that unit's macros.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MacroId {
    unit: u32,
    index: u32,
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

fn string(s: &str, span: Span) -> Expr {
    Expr::Const(Datum {
        kind: DatumKind::String(s.to_owned()),
        span,
    })
}

/// FNV-1a over `body`, spans included: a fixed, deterministic hash (ADR
/// 0001 #4). It walks the datums with a stack of its own, so a deeply
/// nested one cannot overflow the native stack.
fn body_hash(body: &[vali_syntax::Datum]) -> u64 {
    use vali_syntax::DatumKind;

    fn text(feed: &mut impl FnMut(&[u8]), tag: u8, text: &str) {
        feed(&[tag]);
        feed(&text.len().to_le_bytes());
        feed(text.as_bytes());
    }

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let mut work: Vec<&vali_syntax::Datum> = body.iter().rev().collect();
    while let Some(datum) = work.pop() {
        feed(&datum.span.source.0.to_le_bytes());
        feed(&u32::from(datum.span.range.start()).to_le_bytes());
        feed(&u32::from(datum.span.range.end()).to_le_bytes());
        match &datum.kind {
            DatumKind::Number(n) => text(&mut feed, b'n', &n.to_string()),
            DatumKind::Boolean(b) => feed(&[b'b', u8::from(*b)]),
            DatumKind::Character(c) => {
                feed(b"c");
                feed(&u32::from(*c).to_le_bytes());
            }
            DatumKind::String(s) => text(&mut feed, b's', s),
            DatumKind::Symbol(s) => text(&mut feed, b'y', s),
            DatumKind::Keyword(s) => text(&mut feed, b'k', s),
            DatumKind::Bytevector(bytes) => {
                text(&mut feed, b'u', "");
                feed(&bytes.len().to_le_bytes());
                feed(bytes);
            }
            DatumKind::List { items, tail } => {
                feed(&[b'l', u8::from(tail.is_some())]);
                feed(&items.len().to_le_bytes());
                work.extend(tail.as_deref());
                work.extend(items.iter().rev());
            }
            DatumKind::Vector(items) => {
                feed(b"v");
                feed(&items.len().to_le_bytes());
                work.extend(items.iter().rev());
            }
            DatumKind::Labelled(label, inner) => {
                feed(b"=");
                feed(&label.to_le_bytes());
                work.push(inner);
            }
            DatumKind::LabelRef(label) => {
                feed(b"#");
                feed(&label.to_le_bytes());
            }
            _ => feed(b"?"),
        }
    }
    hash
}

fn too_many(e: TooManyScopes) -> CompileError {
    CompileError {
        message: e.to_string(),
        span: None,
        location: None,
    }
}

/// The isolate global a REPL's top-level `name` is. REPL globals have
/// names of their own, so redefining `car` at the REPL cannot change the
/// `car` libraries link to, which is the isolate's global `car`.
pub(crate) fn repl_global(name: &str) -> String {
    format!("repl:{name}")
}

/// Asks a loader whether it has a library, for the identity importing it.
pub(crate) type Resolver = Arc<dyn Fn(&LibraryName, &str) -> bool + Send + Sync>;

pub(crate) struct Expander {
    pub(crate) vars: Vec<VarInfo>,
    pub(crate) lambdas: Vec<Lambda>,
    context: ScopeContext,
    table: BindingTable<String, Binding>,
    /// The scope of the core forms, and its set.
    core_scope: Scope,
    core: ScopeSetId,
    /// The units whose syntax exports are remapped into the context.
    imported_syntax: HashSet<usize>,
    /// The number of the next macro the current unit defines.
    next_macro: u32,
    /// Who the live context (`context`, `table`, `macros` and
    /// `compiled`) belongs to.
    owner: Option<ContextOwner>,
    /// The interaction environment's context, set aside while another
    /// unit expands.
    repl_state: Option<ContextState>,
    /// The scopes of the current unit's own forms.
    top: ScopeSetId,
    /// The units compiled so far, the current one last.
    units: Vec<Unit>,
    /// Each library name, to its unit.
    by_name: HashMap<LibraryName, usize>,
    /// What `(vali core)` exports: the core forms.
    core_interface: Vec<(String, Binding)>,
    /// The `lambda`s being expanded, outermost first.
    stack: Vec<LambdaId>,
    /// Suffixes for module variables that macros introduce.
    fresh: usize,
    /// Suffixes for REPL globals that macros introduce, which, unlike a
    /// unit's own variables, every REPL input shares.
    repl_fresh: usize,
    /// The macros of the live context, its own and imported copies.
    macros: HashMap<MacroId, Macro>,
    /// Macro uses in expressions being expanded, one inside another.
    depth: usize,
    /// The isolates expansion-time code runs in for the unit being
    /// expanded: phase 1 first, then phase 2 and so on, each made on first
    /// use and dropped when the next unit starts.
    phases: Vec<PhaseIsolate>,
    /// Counts the units started, so a transformer made in an earlier
    /// unit's isolate is made again in the current one's.
    generation: u64,
    /// What phase-1 code's syntax procedures look up by number.
    compiled: phase::CompiledByUnit,
    /// For each transformer being expanded, one inside another, where its
    /// `lambda` sits in `stack`. Code above the innermost runs at phase 1.
    floors: Vec<usize>,
    /// The native libraries the program imports, which the phase-1
    /// isolate registers, so their expansion-safe functions run there.
    natives: Vec<(String, std::sync::Arc<vali_runtime::NativeLibrary>)>,
    /// Whether the next unit is one of Vali's own (`Unit::own`).
    next_own: bool,
    /// Whether the next unit is a library, which converts to bytecode as
    /// its functions are first called (`Unit::module`); a program or an
    /// evaluated expression converts whole.
    next_library: bool,
    /// Whether the next unit's key includes a hash of its text
    /// (`Unit::key`).
    next_hashed: bool,
    /// The interaction environment's scope sets, once it is used: where
    /// its imports are bound, and where its own definitions are, inside
    /// those. Every REPL input uses the same ones, so what one defines the
    /// next sees.
    repl: Option<(ScopeSetId, ScopeSetId)>,
    /// How many scopes the interaction environment's context had when the
    /// input being compiled began: the scopes numbered from here are the
    /// input's, and `forget` drops its local bindings (`forget_locals`).
    repl_floor: Option<u32>,
    /// The files bodies include, fetched with the import graph.
    includes: crate::loader::Includes,
    /// Which libraries bodies' `cond-expand`s may find (`add_available`).
    available: crate::loader::Available,
    /// Asks the host's loader whether a library exists, for a name the
    /// import graph's walk did not ask about (`library_answer`).
    resolver: Option<Resolver>,
    /// The library a `cond-expand` or an artifact's record asked about
    /// that nothing could answer yet, and who asked: the caller answers
    /// it, then expands again with the answer in `add_available`.
    needed: Option<(LibraryName, String)>,
    /// The sources read, which spans' source ids index.
    pub(crate) sources: vali_syntax::Sources,
    /// The IR of the last module compiled.
    pub(crate) last_ir: Option<Arc<vali_dist::Module>>,
    /// The same before it was optimised.
    pub(crate) last_lowered: Option<vali_dist::Module>,
    /// Whether what is compiled now is optimised: false while evaluated
    /// code or a REPL input is.
    pub(crate) optimise: bool,
    /// The caps the host set on its isolate, which the isolates that run
    /// its macros share (ADR 0023). Unset while building outside an isolate.
    pub(crate) caps: vali_runtime::HeapConfig,
}

impl Expander {
    pub(crate) fn new() -> Result<Expander> {
        let ContextState {
            context,
            table,
            core_scope,
            core,
            ..
        } = ContextState::fresh()?;
        let expander = Expander {
            vars: Vec::new(),
            lambdas: Vec::new(),
            context,
            table,
            core_scope,
            core,
            imported_syntax: HashSet::new(),
            next_macro: 0,
            owner: None,
            repl_state: None,
            top: core,
            units: Vec::new(),
            by_name: HashMap::new(),
            core_interface: CORE
                .iter()
                .map(|&(name, form)| (name.to_owned(), Binding::Core(form)))
                .collect(),
            stack: Vec::new(),
            fresh: 0,
            repl_fresh: 0,
            macros: HashMap::new(),
            depth: 0,
            phases: Vec::new(),
            generation: 0,
            compiled: Default::default(),
            floors: Vec::new(),
            natives: Vec::new(),
            next_own: false,
            next_library: false,
            next_hashed: false,
            includes: crate::loader::Includes::new(),
            available: crate::loader::Available::new(),
            resolver: None,
            needed: None,
            sources: vali_syntax::Sources::new(),
            last_ir: None,
            last_lowered: None,
            optimise: true,
            caps: Default::default(),
            repl: None,
            repl_floor: None,
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

    /// Notes which libraries bodies' `cond-expand`s may find.
    pub(crate) fn add_available(&mut self, available: crate::loader::Available) {
        self.available.extend(available);
    }

    /// Lets expansion ask `resolver` about a library the walk before
    /// expansion did not ask about, as `eval` does: its loader is at
    /// hand, and what it compiled is not redone.
    pub(crate) fn set_resolver(&mut self, resolver: Option<Resolver>) {
        self.resolver = resolver;
    }

    /// Whether expansion stopped for a library it needs an answer about.
    pub(crate) fn needs_answer(&self) -> bool {
        self.needed.is_some()
    }

    /// The library expansion needed an answer about and had none for, if
    /// it stopped for that.
    pub(crate) fn take_needed(&mut self) -> Option<(LibraryName, String)> {
        self.needed.take()
    }

    /// Whether the library `name` is available to `importer`'s tests: the
    /// walk's answer if it asked, else a compiled library's, else the
    /// resolver's, which is kept so the answer is the same each time. With
    /// no resolver, `None`, and `name` is noted as needed: the loader is
    /// not at hand, so whoever started expansion answers and starts again.
    fn library_answer(&mut self, name: &LibraryName, importer: &str) -> Option<bool> {
        if let Some(&found) = self.available.get(name) {
            return Some(found);
        }
        if self.has_library(name) {
            return Some(true);
        }
        let Some(resolver) = &self.resolver else {
            if self.needed.is_none() {
                self.needed = Some((name.clone(), importer.to_owned()));
            }
            return None;
        };
        let found = resolver(name, importer);
        self.available.insert(name.clone(), found);
        Some(found)
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
            let datum = item.to_datum();
            let DatumKind::String(path) = &datum.kind else {
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
            for datum in crate::loader::read_included(&text, ci, path, &mut self.sources)? {
                forms.push(Syntax::from_datum(&datum, scopes));
            }
        }
        Ok(forms)
    }

    /// The forms of the first `cond-expand` clause whose requirement holds
    /// (R7RS 4.2.1), or none. A `(library name)` requirement holds for
    /// Vali's libraries and those the loader has: asked before expansion
    /// for each name written out in the source, and when the test is met
    /// for a name expansion made (`library_answer`).
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
            let importer = self.units.last().map(|u| u.identity.clone());
            let importer = importer.unwrap_or_default();
            let mut asked = Vec::new();
            let satisfied = crate::features::is_else(&requirement)
                || crate::features::satisfied(&requirement, &mut |n| {
                    if crate::loader::is_standard(n) {
                        return true;
                    }
                    let Some(found) = self.library_answer(n, &importer) else {
                        return false;
                    };
                    asked.push((n.to_string(), found));
                    found
                })?;
            if let Some((name, _)) = &self.needed {
                return Err(error(
                    format!("cond-expand: library {name} was not asked about"),
                    span,
                ));
            }
            // The unit being expanded followed what it found.
            if let Some(unit) = self.units.last_mut() {
                for answer in asked {
                    if !unit.asked.contains(&answer) {
                        unit.asked.push(answer);
                    }
                }
            }
            if satisfied {
                return Ok(body.to_vec());
            }
        }
        Ok(Vec::new())
    }

    /// Gives the isolates that run macros the caps of the host's isolate,
    /// those made already and those to come (ADR 0023).
    pub(crate) fn set_caps(&mut self, caps: &vali_runtime::HeapConfig) {
        self.caps = *caps;
        for phase in &mut self.phases {
            phase.cx.set_caps(caps);
        }
    }

    pub(crate) fn add_native(
        &mut self,
        identity: &str,
        library: std::sync::Arc<vali_runtime::NativeLibrary>,
    ) {
        for phase in &mut self.phases {
            phase
                .cx
                .register_native_library(identity, std::sync::Arc::clone(&library));
        }
        self.natives.push((identity.to_owned(), library));
    }

    /// Notes that `name` is a library's name for the next unit compiled.
    pub(crate) fn name_next(&mut self, name: LibraryName) {
        self.by_name.insert(name, self.units.len());
    }

    /// Whether the library `identity` was loaded from its artifact in
    /// place of being compiled.
    pub(crate) fn loaded_from_artifact(&self, identity: &str) -> bool {
        self.unit_of(identity)
            .is_some_and(|unit| self.units[unit].pruned && !self.units[unit].round_tripped)
    }

    /// The unit compiled with `identity`, if there is one.
    pub(crate) fn unit_of(&self, identity: &str) -> Option<usize> {
        self.units.iter().position(|u| u.identity == identity)
    }

    /// Whether the library `name` is compiled, under that name.
    pub(crate) fn has_library(&self, name: &LibraryName) -> bool {
        self.by_name.contains_key(name)
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
    /// `own` says whether it is one of Vali's own libraries.
    pub(crate) fn library(
        &mut self,
        identity: &str,
        form: &LibraryForm,
        own: bool,
        source: Option<Digest>,
    ) -> Result<Converted> {
        if self.natives.iter().any(|(i, _)| i == identity) {
            return self.native_library(identity, form).map(Converted::Eager);
        }
        self.start_context(false, &form.imports)?;
        self.next_own = own;
        self.next_library = true;
        let scope = self.scope(ScopeKind::Library)?;
        let top = self.context.intern(&[scope]).map_err(too_many)?;
        let module = self.unit(
            identity,
            &form.imports,
            top,
            top,
            &form.body,
            &form.exports,
            form.span,
            false,
        )?;
        let unit = self.units.len() - 1;
        self.units[unit].source = source;
        self.publish(unit, form, top)?;
        if !round_trip() {
            return self.digested(unit, module);
        }
        // Written, dropped and loaded again, so what follows runs on what
        // the artifact holds.
        let bytes = self.library_artifact(unit)?;
        self.units.pop();
        let module = self.load_library(identity, &bytes, own, source)?;
        self.units[unit].round_tripped = true;
        Ok(module)
    }

    /// `module`, the code of library `unit`, with the digest that tells it
    /// from another build: that of its IR's encoding, which is what its
    /// artifact holds, so a library loaded from one has the same.
    fn digested(&mut self, unit: usize, mut module: Converted) -> Result<Converted> {
        let Some(ir) = self.units[unit].ir.clone() else {
            return Ok(module);
        };
        let identity = &self.units[unit].identity;
        let bytes = vali_dist::encode(&ir).map_err(|e| CompileError {
            message: format!("{identity}: {e}"),
            span: None,
            location: None,
        })?;
        module.module_mut().digest = Some(vali_dist::Digest::of(&bytes).0);
        self.units[unit].module = Some(module.clone());
        Ok(module)
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
            self.letrec_expr(bindings, body)
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
                    self.variable(owner, global, stx)
                }
                Some(Binding::Primitive(name)) => Ok(Expr::Free(name)),
                Some(Binding::Global(name)) => Ok(Expr::ReplGlobal(name)),
                None if self.in_repl() => Ok(Expr::ReplGlobal(repl_global(name))),
                None => Err(error(
                    format!("{name} is not bound: is it imported?"),
                    stx.span(),
                )),
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
        // A call's window, its procedure and arguments must fit in a
        // frame (`wide` forms address up to 65,535 slots).
        if items.len() - 1 > usize::from(u16::MAX) - 8 {
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
            Core::LetStar => self.let_star(keyword, args, span),
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
                let (unit, table) = self.unit_table(span);
                table.quoted.push(form.clone());
                let index = number(table.quoted.len() - 1, span);
                Ok(Expr::Call(
                    Box::new(Expr::Free(QUOTE_SYNTAX.to_owned())),
                    vec![unit, index],
                    span,
                ))
            }
            Core::Quasiquote => self.quasiquote(keyword, args, span),
            Core::Unquote | Core::UnquoteSplicing => Err(error(
                format!("{keyword}: misplaced auxiliary syntax"),
                span,
            )),
            Core::SyntaxCase => self.syntax_case(keyword, args, span),
            Core::Syntax => self.syntax_template(keyword, args, span),
        }
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

    /// `(let* ((id init) ...) body ...)`: each binding inside the one
    /// before, which is nested `let`s, built here in one pass. As a macro
    /// that expands to a `let` and a `let*` it took a step for each binding,
    /// each rebuilding the bindings after it, so time and memory grew with
    /// the square of their number.
    fn let_star(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            ));
        };
        let bindings = self.bindings(bindings, keyword)?;
        let let_ = || Syntax::identifier("let", self.core, span, None);
        let empty = Syntax::list(Vec::new(), None, self.core, span, None);
        let mut nested = vec![let_(), empty];
        nested.extend(body.iter().cloned());
        let mut nested = Syntax::list(nested, None, self.core, span, None);
        for (id, init) in bindings.into_iter().rev() {
            let binding = Syntax::list(vec![id, init], None, self.core, span, None);
            let bindings = Syntax::list(vec![binding], None, self.core, span, None);
            nested = Syntax::list(vec![let_(), bindings, nested], None, self.core, span, None);
        }
        self.expr(&nested)
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
        Ok(self.letrec_expr(vars.into_iter().zip(inits).collect(), body))
    }

    /// A `letrec*` over `bindings`. Lowering patches a closure that
    /// captured a variable early only when the closure is the binding's
    /// own value. A captured variable with any other value may be seen by
    /// a closure made inside that value, which nothing patches, so it
    /// lives in a box that is filled once the value is computed.
    fn letrec_expr(&mut self, bindings: Vec<(VarId, Expr)>, body: Expr) -> Expr {
        for (v, init) in &bindings {
            if self.vars[*v].captured && !matches!(init, Expr::Lambda(_)) {
                self.vars[*v].assigned = true;
            }
        }
        Expr::Letrec(bindings, Box::new(body))
    }
}
