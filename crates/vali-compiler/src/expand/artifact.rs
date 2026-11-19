//! A unit's syntax export as bytes (ADR 0001 #7, ADR 0012 #5).
//!
//! What is written is the export as an importer would see it: remapped
//! into a fresh context first ([`Expander::export_syntax`]), so only the
//! scopes its macros, compiled syntax and bindings reach are there,
//! numbered in the order they were met. The section is:
//!
//! - the scopes, by kind, and the sets, as lists of scopes;
//! - the core forms' scope and set, and the next macro's number;
//! - the units whose exports are remapped in;
//! - the macros: their units and numbers, names, and transformers, a
//!   `syntax-rules` transformer's clauses or a procedure's module as
//!   distribution code;
//! - each unit's compiled syntax;
//! - the bindings that are not local, sorted.
//!
//! Units, sources and macro origins are written in full where they first
//! appear, and by number after: a unit by its identity, which loading
//! resolves among the units it knows; a source by its identity and text,
//! which loading adds to the expander's sources unless they are there
//! already; an origin by its macro's name, use site, and parent.
//!
//! Numbers are written as their external representation, which reading
//! gives back exactly.

use std::collections::HashMap;
use std::sync::Arc;

use vali_dist::{DecodeError, Reader, Writer};
use vali_syntax::{Effect, NewNode, Node, Sources, TextRange, TextSize};

use super::{Binding, CORE, Clause, ContextState, Macro, MacroId, Procedure, Transformer};
use crate::CompileError;
use crate::pattern::{Pattern, Repeat, Template};
use crate::phase::Compiled;
use vali_syntax::{
    BindingTable, DatumKind, Origin, Phase, Phases, Scope, ScopeContext, ScopeSetId, SourceId,
    Span, Syntax,
};

use super::{MAX_PHASE, RED_ZONE, STACK_GROWTH};
use crate::pattern::MAX_NESTING;

type Result<T> = std::result::Result<T, String>;

/// Writes a syntax export.
struct Encoder<'a> {
    w: Writer,
    sources: &'a Sources,
    /// Each unit's identity, by its index in the build.
    identities: &'a [String],
    units: HashMap<usize, usize>,
    source_refs: HashMap<SourceId, usize>,
    origins: HashMap<*const Origin, usize>,
}

/// Encodes `state`, a syntax export in a context of its own.
pub(super) fn encode(
    state: &ContextState,
    identities: &[String],
    sources: &Sources,
) -> Result<Vec<u8>> {
    let mut e = Encoder {
        w: Writer::new(),
        sources,
        identities,
        units: HashMap::new(),
        source_refs: HashMap::new(),
        origins: HashMap::new(),
    };
    let context = &state.context;
    e.w.usize(context.scope_count());
    for scope in context.all_scopes() {
        e.w.byte(match context.kind(scope) {
            vali_syntax::ScopeKind::Macro => 0,
            vali_syntax::ScopeKind::UseSite => 1,
            vali_syntax::ScopeKind::Binder => 2,
            vali_syntax::ScopeKind::Library => 3,
            vali_syntax::ScopeKind::Other => 4,
        });
    }
    e.w.usize(context.set_count());
    for set in context.all_sets() {
        let scopes = context.scopes(set);
        e.w.usize(scopes.len());
        for scope in scopes {
            e.w.u32(scope.index());
        }
    }
    e.w.u32(state.core_scope.index());
    e.w.u32(state.core.index());
    e.w.u32(state.next_macro);
    let mut imported: Vec<usize> = state.imported.iter().copied().collect();
    imported.sort_unstable();
    e.w.usize(imported.len());
    for unit in imported {
        e.unit(unit)?;
    }
    let mut ids: Vec<&MacroId> = state.macros.keys().collect();
    ids.sort();
    e.w.usize(ids.len());
    for id in ids {
        e.macro_id(*id)?;
        e.r#macro(&state.macros[id])?;
    }
    let mut keys: Vec<&String> = state.compiled.keys().collect();
    keys.sort();
    e.w.usize(keys.len());
    for key in keys {
        e.w.str(key);
        e.compiled(&state.compiled[key])?;
    }
    let mut entries: Vec<_> = state
        .table
        .entries()
        .filter(|(_, _, _, b)| !matches!(b, Binding::Local(_) | Binding::PatternVar(..)))
        .collect();
    entries.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    e.w.usize(entries.len());
    for (symbol, set, phases, binding) in entries {
        e.w.str(symbol);
        e.w.u32(set.index());
        match phases {
            Phases::All => e.w.byte(0),
            Phases::Only(phases) => {
                e.w.byte(1);
                e.w.usize(phases.len());
                for phase in phases {
                    e.w.int(i64::from(phase.0));
                }
            }
        }
        e.binding(binding)?;
    }
    Ok(e.w.finish())
}

impl Encoder<'_> {
    fn unit(&mut self, unit: usize) -> Result<()> {
        if let Some(&n) = self.units.get(&unit) {
            self.w.usize(n + 1);
            return Ok(());
        }
        let identity = self
            .identities
            .get(unit)
            .ok_or_else(|| format!("unit {unit} has no identity"))?;
        self.w.usize(0);
        self.w.str(identity);
        self.units.insert(unit, self.units.len());
        Ok(())
    }

    fn macro_id(&mut self, id: MacroId) -> Result<()> {
        self.unit(id.unit as usize)?;
        self.w.u32(id.index);
        Ok(())
    }

    fn r#macro(&mut self, m: &Macro) -> Result<()> {
        self.w.str(&m.name);
        match &m.transformer {
            Transformer::Rules(clauses) => {
                self.w.byte(0);
                self.w.usize(clauses.len());
                for clause in clauses {
                    self.pattern(&clause.pattern)?;
                    self.template(&clause.template)?;
                    self.depths(&clause.depths);
                }
            }
            Transformer::Procedure(p) => {
                self.w.byte(1);
                self.w.usize(p.phase);
                let bytes = vali_dist::encode(&p.ir).map_err(|e| e.to_string())?;
                self.w.bytes(&bytes);
            }
        }
        Ok(())
    }

    fn depths(&mut self, depths: &[usize]) {
        self.w.usize(depths.len());
        for &d in depths {
            self.w.usize(d);
        }
    }

    fn compiled(&mut self, compiled: &Compiled) -> Result<()> {
        self.w.usize(compiled.quoted.len());
        for s in &compiled.quoted {
            self.syntax(s)?;
        }
        self.w.usize(compiled.patterns.len());
        for (p, n) in &compiled.patterns {
            self.pattern(p)?;
            self.w.usize(*n);
        }
        self.w.usize(compiled.templates.len());
        for (t, depths) in &compiled.templates {
            self.template(t)?;
            self.depths(depths);
        }
        Ok(())
    }

    fn binding(&mut self, binding: &Binding) -> Result<()> {
        match binding {
            Binding::Core(core) => {
                let name = CORE
                    .iter()
                    .find(|(_, c)| c == core)
                    .map(|(name, _)| *name)
                    .ok_or("a core form with no name")?;
                self.w.byte(0);
                self.w.str(name);
            }
            Binding::Variable(unit, name) => {
                self.w.byte(1);
                self.unit(*unit)?;
                self.w.str(name);
            }
            Binding::Primitive(name) => {
                self.w.byte(2);
                self.w.str(name);
            }
            Binding::Macro(id) => {
                self.w.byte(3);
                self.macro_id(*id)?;
            }
            Binding::Global(name) => {
                self.w.byte(4);
                self.w.str(name);
            }
            Binding::Local(_) | Binding::PatternVar(..) => {
                return Err("a local binding in a syntax export".into());
            }
        }
        Ok(())
    }

    fn span(&mut self, span: Span) -> Result<()> {
        if span.source == SourceId(0) {
            self.w.usize(0);
        } else if let Some(&n) = self.source_refs.get(&span.source) {
            self.w.usize(n + 2);
        } else {
            let (identity, text) = self
                .sources
                .source(span.source)
                .ok_or("a span in an unknown source")?;
            self.w.usize(1);
            self.w.str(identity);
            self.w.str(text);
            self.source_refs.insert(span.source, self.source_refs.len());
        }
        self.w.u32(u32::from(span.range.start()));
        self.w.u32(u32::from(span.range.end()));
        Ok(())
    }

    /// An origin: how many new origins follow, what the outermost of them
    /// continues (none, or one written before), then each, outermost
    /// first.
    fn origin(&mut self, origin: Option<&Arc<Origin>>) -> Result<()> {
        let mut new = Vec::new();
        let mut base = origin;
        while let Some(o) = base {
            if self.origins.contains_key(&Arc::as_ptr(o)) {
                break;
            }
            new.push(o);
            base = o.parent.as_ref();
        }
        self.w.usize(new.len());
        match base {
            None => self.w.usize(0),
            Some(o) => self.w.usize(self.origins[&Arc::as_ptr(o)] + 1),
        }
        for o in new.into_iter().rev() {
            self.w.str(&o.macro_name);
            self.span(o.use_site)?;
            self.origins.insert(Arc::as_ptr(o), self.origins.len());
        }
        Ok(())
    }

    fn syntax(&mut self, s: &Syntax) -> Result<()> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.syntax_here(s))
    }

    fn syntax_here(&mut self, s: &Syntax) -> Result<()> {
        match s.node() {
            Node::Identifier(name) => {
                self.w.byte(0);
                self.w.str(name);
            }
            Node::List { items, tail } => {
                self.w.byte(1);
                self.w.usize(items.len());
                for item in items {
                    self.syntax(item)?;
                }
                self.w.bool(tail.is_some());
                if let Some(tail) = tail {
                    self.syntax(tail)?;
                }
            }
            Node::Vector(items) => {
                self.w.byte(2);
                self.w.usize(items.len());
                for item in items {
                    self.syntax(item)?;
                }
            }
            Node::Atom(atom) => {
                self.w.byte(3);
                self.atom(atom)?;
            }
        }
        self.w.u32(s.scopes().index());
        let pending = s.pending();
        self.w.usize(pending.len());
        for &(scope, effect) in pending {
            self.w.u32(scope.index());
            self.w.byte(match effect {
                Effect::Add => 0,
                Effect::Remove => 1,
                Effect::Flip => 2,
            });
        }
        self.span(s.span())?;
        self.origin(s.origin())
    }

    fn atom(&mut self, atom: &DatumKind) -> Result<()> {
        match atom {
            DatumKind::Number(n) => {
                self.w.byte(0);
                self.w.str(&n.to_string());
            }
            DatumKind::Boolean(b) => {
                self.w.byte(1);
                self.w.bool(*b);
            }
            DatumKind::Character(c) => {
                self.w.byte(2);
                self.w.u32(u32::from(*c));
            }
            DatumKind::String(s) => {
                self.w.byte(3);
                self.w.str(s);
            }
            DatumKind::Symbol(s) => {
                self.w.byte(4);
                self.w.str(s);
            }
            DatumKind::Keyword(s) => {
                self.w.byte(5);
                self.w.str(s);
            }
            DatumKind::Bytevector(bytes) => {
                self.w.byte(6);
                self.w.bytes(bytes);
            }
            _ => return Err("a datum label in a syntax export".into()),
        }
        Ok(())
    }

    fn patterns(&mut self, patterns: &[Pattern]) -> Result<()> {
        self.w.usize(patterns.len());
        patterns.iter().try_for_each(|p| self.pattern(p))
    }

    fn pattern(&mut self, p: &Pattern) -> Result<()> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.pattern_here(p))
    }

    fn pattern_here(&mut self, p: &Pattern) -> Result<()> {
        match p {
            Pattern::Discard => self.w.byte(0),
            Pattern::Var(n) => {
                self.w.byte(1);
                self.w.usize(*n);
            }
            Pattern::Literal(s) => {
                self.w.byte(2);
                self.syntax(s)?;
            }
            Pattern::Datum(text) => {
                self.w.byte(3);
                self.w.str(text);
            }
            Pattern::List {
                before,
                repeat,
                tail,
            } => {
                self.w.byte(4);
                self.patterns(before)?;
                self.repeat(repeat.as_ref())?;
                self.w.bool(tail.is_some());
                if let Some(tail) = tail {
                    self.pattern(tail)?;
                }
            }
            Pattern::Vector { before, repeat } => {
                self.w.byte(5);
                self.patterns(before)?;
                self.repeat(repeat.as_ref())?;
            }
        }
        Ok(())
    }

    fn repeat(&mut self, repeat: Option<&Repeat>) -> Result<()> {
        self.w.bool(repeat.is_some());
        if let Some(r) = repeat {
            self.pattern(&r.each)?;
            self.depths(&r.vars);
            self.patterns(&r.after)?;
        }
        Ok(())
    }

    fn items(&mut self, items: &[(Template, usize)]) -> Result<()> {
        self.w.usize(items.len());
        for (t, ellipses) in items {
            self.template(t)?;
            self.w.usize(*ellipses);
        }
        Ok(())
    }

    fn template(&mut self, t: &Template) -> Result<()> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.template_here(t))
    }

    fn template_here(&mut self, t: &Template) -> Result<()> {
        match t {
            Template::Var(n) => {
                self.w.byte(0);
                self.w.usize(*n);
            }
            Template::Constant(s) => {
                self.w.byte(1);
                self.syntax(s)?;
            }
            Template::List {
                items,
                tail,
                syntax,
            } => {
                self.w.byte(2);
                self.items(items)?;
                self.w.bool(tail.is_some());
                if let Some(tail) = tail {
                    self.template(tail)?;
                }
                self.syntax(syntax)?;
            }
            Template::Vector { items, syntax } => {
                self.w.byte(3);
                self.items(items)?;
                self.syntax(syntax)?;
            }
        }
        Ok(())
    }
}

/// Reads a syntax export.
struct Decoder<'a, 'b> {
    r: Reader<'a>,
    context: ScopeContext,
    scopes: Vec<Scope>,
    sets: Vec<ScopeSetId>,
    sources: &'b mut Sources,
    source_ids: Vec<SourceId>,
    /// Resolves a unit's identity to its index in this build.
    resolve: &'b dyn Fn(&str) -> Option<usize>,
    units: Vec<usize>,
    origins: Vec<Arc<Origin>>,
    /// How deep in patterns and templates the decoder is.
    depth: usize,
    /// What the pattern being decoded has bound.
    facts: Facts,
}

/// How deeply a decoded pattern or template may nest: twice what source
/// may (`MAX_NESTING`), as a compiled pattern has a level for a repeat that
/// its source has none for. What walks them recurses on this, and drops
/// them recursively, so the depth is what the stack must hold, and what
/// the input says is not trusted with it.
const MAX_DECODED_NESTING: usize = 2 * MAX_NESTING;

/// What decoding a pattern met, to check it as a whole once its variable
/// count is known.
#[derive(Default)]
struct Facts {
    /// The variables it binds, in the order they were met.
    vars: Vec<usize>,
    /// Each repeat's range of `vars` for the variables its repeated
    /// pattern binds, and the variables it names.
    repeats: Vec<(usize, usize, Vec<usize>)>,
}

impl Facts {
    /// Checks that the pattern binds each of variables `0..count` once,
    /// and each repeat names exactly the variables its repeated pattern
    /// binds, as the compiler makes them. Matching can then rely on it: an
    /// unbound variable is a panic, and a count beyond what is bound is
    /// the size of the allocation made for each match.
    fn check(&self, count: usize) -> std::result::Result<(), String> {
        if self.vars.len() != count {
            return Err(format!(
                "a pattern binds {} variables, not the {count} it counts",
                self.vars.len()
            ));
        }
        let mut position = vec![usize::MAX; count];
        for (at, &v) in self.vars.iter().enumerate() {
            match position.get_mut(v) {
                Some(slot) if *slot == usize::MAX => *slot = at,
                Some(_) => return Err(format!("a pattern binds variable {v} twice")),
                None => return Err(format!("a pattern binds variable {v} of {count}")),
            }
        }
        for (start, end, named) in &self.repeats {
            if named.len() != end - start {
                return Err("a repeat does not name the variables it repeats".to_owned());
            }
            let mut places: Vec<usize> = named
                .iter()
                .map(|&v| position.get(v).copied().unwrap_or(usize::MAX))
                .collect();
            places.sort_unstable();
            if places.iter().zip(*start..*end).any(|(&p, q)| p != q) {
                return Err("a repeat does not name the variables it repeats".to_owned());
            }
        }
        Ok(())
    }
}

type Decoded<T> = std::result::Result<T, DecodeError>;

/// Decodes a syntax export, resolving the units it names with `resolve`
/// and adding the sources its spans are in to `sources`.
pub(super) fn decode(
    bytes: &[u8],
    resolve: &dyn Fn(&str) -> Option<usize>,
    sources: &mut Sources,
) -> std::result::Result<ContextState, CompileError> {
    let mut d = Decoder {
        r: Reader::new(bytes),
        context: ScopeContext::new(),
        scopes: Vec::new(),
        sets: Vec::new(),
        sources,
        source_ids: Vec::new(),
        resolve,
        units: Vec::new(),
        origins: Vec::new(),
        depth: 0,
        facts: Facts::default(),
    };
    d.state().map_err(|e| CompileError {
        message: format!("a syntax export: {e}"),
        span: None,
        location: None,
    })
}

impl Decoder<'_, '_> {
    fn state(&mut self) -> Decoded<ContextState> {
        let n = self.r.count(1)?;
        for _ in 0..n {
            let kind = match self.r.byte()? {
                0 => vali_syntax::ScopeKind::Macro,
                1 => vali_syntax::ScopeKind::UseSite,
                2 => vali_syntax::ScopeKind::Binder,
                3 => vali_syntax::ScopeKind::Library,
                4 => vali_syntax::ScopeKind::Other,
                b => return Err(self.r.error(format!("unknown scope kind {b}"))),
            };
            let scope = self
                .context
                .new_scope(kind)
                .map_err(|e| self.r.error(e.to_string()))?;
            self.scopes.push(scope);
        }
        let n = self.r.count(1)?;
        for _ in 0..n {
            let len = self.r.count(1)?;
            let mut scopes = Vec::with_capacity(len);
            for _ in 0..len {
                scopes.push(self.scope()?);
            }
            let set = self
                .context
                .intern(&scopes)
                .map_err(|e| self.r.error(e.to_string()))?;
            self.sets.push(set);
        }
        let core_scope = self.scope()?;
        let core = self.set()?;
        let next_macro = self.r.u32()?;
        let n = self.r.count(1)?;
        let mut imported = std::collections::HashSet::new();
        for _ in 0..n {
            imported.insert(self.unit()?);
        }
        let n = self.r.count(3)?;
        let mut macros = HashMap::with_capacity(n);
        for _ in 0..n {
            let id = self.macro_id()?;
            let m = self.r#macro()?;
            macros.insert(id, m);
        }
        let n = self.r.count(4)?;
        let mut compiled = HashMap::with_capacity(n);
        for _ in 0..n {
            let key = self.r.str()?.to_owned();
            let c = self.compiled()?;
            compiled.insert(key, c);
        }
        let n = self.r.count(4)?;
        let mut table = BindingTable::new();
        for _ in 0..n {
            let symbol = self.r.str()?.to_owned();
            let set = self.set()?;
            let phases = match self.r.byte()? {
                0 => Phases::All,
                1 => {
                    let len = self.r.count(1)?;
                    let mut phases = Vec::with_capacity(len);
                    for _ in 0..len {
                        let p = self.r.int()?;
                        phases.push(Phase(
                            i32::try_from(p).map_err(|_| self.r.error("a phase overflows"))?,
                        ));
                    }
                    Phases::Only(phases)
                }
                b => return Err(self.r.error(format!("unknown phases {b}"))),
            };
            let binding = self.binding()?;
            if table
                .bind(&self.context, symbol.clone(), set, phases, binding)
                .is_err()
            {
                return Err(self.r.error(format!("{symbol} is bound twice")));
            }
        }
        self.r.end()?;
        Ok(ContextState {
            context: std::mem::take(&mut self.context),
            table,
            core_scope,
            core,
            macros,
            compiled,
            imported,
            next_macro,
        })
    }

    fn scope(&mut self) -> Decoded<Scope> {
        let n = self.r.uint()?;
        usize::try_from(n)
            .ok()
            .and_then(|n| self.scopes.get(n).copied())
            .ok_or_else(|| self.r.error(format!("scope {n} does not exist")))
    }

    fn set(&mut self) -> Decoded<ScopeSetId> {
        let n = self.r.uint()?;
        usize::try_from(n)
            .ok()
            .and_then(|n| self.sets.get(n).copied())
            .ok_or_else(|| self.r.error(format!("scope set {n} does not exist")))
    }

    fn unit(&mut self) -> Decoded<usize> {
        match self.r.uint()? {
            0 => {
                let identity = self.r.str()?;
                let unit = (self.resolve)(identity).ok_or_else(|| {
                    self.r
                        .error(format!("{identity} is named, but is not loaded"))
                })?;
                self.units.push(unit);
                Ok(unit)
            }
            n => usize::try_from(n - 1)
                .ok()
                .and_then(|n| self.units.get(n).copied())
                .ok_or_else(|| self.r.error(format!("unit {n} does not exist"))),
        }
    }

    fn macro_id(&mut self) -> Decoded<MacroId> {
        let unit = u32::try_from(self.unit()?).map_err(|_| self.r.error("too many units"))?;
        Ok(MacroId {
            unit,
            index: self.r.u32()?,
        })
    }

    fn depths(&mut self) -> Decoded<Vec<usize>> {
        let n = self.r.count(1)?;
        (0..n)
            .map(|_| {
                let d = self.r.uint()?;
                usize::try_from(d).map_err(|_| self.r.error("a depth overflows"))
            })
            .collect()
    }

    fn index(&mut self) -> Decoded<usize> {
        let n = self.r.uint()?;
        usize::try_from(n).map_err(|_| self.r.error("an index overflows"))
    }

    fn r#macro(&mut self) -> Decoded<Macro> {
        let name = self.r.str()?.to_owned();
        let transformer = match self.r.byte()? {
            0 => {
                let n = self.r.count(3)?;
                let mut clauses = Vec::with_capacity(n);
                for _ in 0..n {
                    let pattern = self.pattern()?;
                    let facts = std::mem::take(&mut self.facts);
                    let template = self.template()?;
                    let depths = self.depths()?;
                    let vars = depths.len();
                    if max_var(&pattern).max(max_template_var(&template)) > vars {
                        return Err(self.r.error(format!(
                            "{name}: a clause names a variable it does not bind"
                        )));
                    }
                    facts
                        .check(vars)
                        .map_err(|why| self.r.error(format!("{name}: {why}")))?;
                    clauses.push(Clause {
                        pattern,
                        template,
                        depths,
                    });
                }
                Transformer::Rules(clauses)
            }
            1 => {
                let phase = self.index()?;
                if phase > MAX_PHASE {
                    return Err(self.r.error(format!(
                        "{name}: a transformer at phase {phase}, above {MAX_PHASE}"
                    )));
                }
                let bytes = self.r.bytes()?;
                let ir = vali_dist::decode(bytes)?;
                Transformer::Procedure(Procedure {
                    ir: Arc::new(ir),
                    code: Arc::default(),
                    phase,
                    made: None,
                })
            }
            b => return Err(self.r.error(format!("unknown transformer {b}"))),
        };
        Ok(Macro { name, transformer })
    }

    fn compiled(&mut self) -> Decoded<Compiled> {
        let n = self.r.count(1)?;
        let mut quoted = Vec::with_capacity(n);
        for _ in 0..n {
            quoted.push(self.syntax()?);
        }
        let n = self.r.count(2)?;
        let mut patterns = Vec::with_capacity(n);
        for _ in 0..n {
            let p = self.pattern()?;
            let facts = std::mem::take(&mut self.facts);
            let vars = self.index()?;
            if max_var(&p) > vars {
                return Err(self.r.error("a pattern names a variable it does not count"));
            }
            facts.check(vars).map_err(|why| self.r.error(why))?;
            patterns.push((p, vars));
        }
        let n = self.r.count(2)?;
        let mut templates = Vec::with_capacity(n);
        for _ in 0..n {
            let t = self.template()?;
            let depths = self.depths()?;
            if max_template_var(&t) > depths.len() {
                return Err(self.r.error("a template names a variable with no depth"));
            }
            templates.push((t, depths));
        }
        Ok(Compiled {
            quoted,
            patterns,
            templates,
        })
    }

    fn binding(&mut self) -> Decoded<Binding> {
        Ok(match self.r.byte()? {
            0 => {
                let name = self.r.str()?;
                let core = CORE
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| *c)
                    .ok_or_else(|| self.r.error(format!("{name} is not a core form")))?;
                Binding::Core(core)
            }
            1 => {
                let unit = self.unit()?;
                Binding::Variable(unit, self.r.str()?.to_owned())
            }
            2 => Binding::Primitive(self.r.str()?.to_owned()),
            3 => Binding::Macro(self.macro_id()?),
            4 => Binding::Global(self.r.str()?.to_owned()),
            b => return Err(self.r.error(format!("unknown binding {b}"))),
        })
    }

    fn span(&mut self) -> Decoded<Span> {
        let source = match self.r.uint()? {
            0 => SourceId(0),
            1 => {
                let identity = self.r.str()?;
                let text = self.r.str()?;
                let id = self.sources.intern(identity, text);
                self.source_ids.push(id);
                id
            }
            n => usize::try_from(n - 2)
                .ok()
                .and_then(|n| self.source_ids.get(n).copied())
                .ok_or_else(|| self.r.error(format!("source {n} does not exist")))?,
        };
        let start = self.r.u32()?;
        let end = self.r.u32()?;
        let len = self
            .sources
            .source(source)
            .map_or(0, |(_, text)| text.len());
        if start > end || (source != SourceId(0) && end as usize > len) {
            return Err(self.r.error("a span outside its source"));
        }
        Ok(Span {
            source,
            range: TextRange::new(TextSize::from(start), TextSize::from(end)),
        })
    }

    fn origin(&mut self) -> Decoded<Option<Arc<Origin>>> {
        let new = self.r.count(2)?;
        let mut origin = match self.r.uint()? {
            0 => None,
            n => Some(
                usize::try_from(n - 1)
                    .ok()
                    .and_then(|n| self.origins.get(n).cloned())
                    .ok_or_else(|| self.r.error(format!("origin {n} does not exist")))?,
            ),
        };
        for _ in 0..new {
            let macro_name = self.r.str()?.into();
            let use_site = self.span()?;
            let o = Arc::new(Origin {
                macro_name,
                use_site,
                parent: origin,
            });
            self.origins.push(Arc::clone(&o));
            origin = Some(o);
        }
        Ok(origin)
    }

    fn syntaxes(&mut self) -> Decoded<Vec<Syntax>> {
        let n = self.r.count(1)?;
        (0..n).map(|_| self.syntax()).collect()
    }

    fn syntax(&mut self) -> Decoded<Syntax> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.syntax_here())
    }

    fn syntax_here(&mut self) -> Decoded<Syntax> {
        let node = match self.r.byte()? {
            0 => NewNode::Identifier(self.r.str()?.into()),
            1 => {
                let items = self.syntaxes()?;
                let tail = if self.r.bool()? {
                    Some(self.syntax()?)
                } else {
                    None
                };
                NewNode::List { items, tail }
            }
            2 => NewNode::Vector(self.syntaxes()?),
            3 => NewNode::Atom(self.atom()?),
            b => return Err(self.r.error(format!("unknown syntax tag {b}"))),
        };
        let scopes = self.set()?;
        let n = self.r.count(2)?;
        let mut pending = Vec::with_capacity(n);
        for _ in 0..n {
            let scope = self.scope()?;
            let effect = match self.r.byte()? {
                0 => Effect::Add,
                1 => Effect::Remove,
                2 => Effect::Flip,
                b => return Err(self.r.error(format!("unknown scope change {b}"))),
            };
            pending.push((scope, effect));
        }
        let span = self.span()?;
        let origin = self.origin()?;
        Syntax::from_parts(node, scopes, pending, span, origin)
            .ok_or_else(|| self.r.error("pending scope changes out of order"))
    }

    fn atom(&mut self) -> Decoded<DatumKind> {
        Ok(match self.r.byte()? {
            0 => {
                let text = self.r.str()?;
                DatumKind::Number(
                    vali_num::parse(text, vali_num::Radix::Decimal)
                        .map_err(|_| self.r.error(format!("{text} is not a number")))?,
                )
            }
            1 => DatumKind::Boolean(self.r.bool()?),
            2 => {
                let n = self.r.u32()?;
                DatumKind::Character(
                    char::from_u32(n)
                        .ok_or_else(|| self.r.error(format!("{n:#x} is not a character")))?,
                )
            }
            3 => DatumKind::String(self.r.str()?.to_owned()),
            4 => DatumKind::Symbol(self.r.str()?.to_owned()),
            5 => DatumKind::Keyword(self.r.str()?.to_owned()),
            6 => DatumKind::Bytevector(self.r.bytes()?.to_vec()),
            b => return Err(self.r.error(format!("unknown atom tag {b}"))),
        })
    }

    fn patterns(&mut self) -> Decoded<Vec<Pattern>> {
        let n = self.r.count(1)?;
        (0..n).map(|_| self.pattern()).collect()
    }

    fn pattern(&mut self) -> Decoded<Pattern> {
        self.nested("a pattern", Self::pattern_here)
    }

    /// Runs `f` one level deeper in patterns and templates, which are
    /// refused past [`MAX_DECODED_NESTING`].
    fn nested<T>(&mut self, what: &str, f: impl FnOnce(&mut Self) -> Decoded<T>) -> Decoded<T> {
        if self.depth >= MAX_DECODED_NESTING {
            return Err(self.r.error(format!(
                "{what} is nested more than {MAX_DECODED_NESTING} levels deep"
            )));
        }
        self.depth += 1;
        let result = stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || f(self));
        self.depth -= 1;
        result
    }

    fn pattern_here(&mut self) -> Decoded<Pattern> {
        Ok(match self.r.byte()? {
            0 => Pattern::Discard,
            1 => {
                let v = self.index()?;
                self.facts.vars.push(v);
                Pattern::Var(v)
            }
            2 => Pattern::Literal(self.syntax()?),
            3 => Pattern::Datum(self.r.str()?.to_owned()),
            4 => {
                let before = self.patterns()?;
                let repeat = self.repeat()?;
                let tail = if self.r.bool()? {
                    Some(Box::new(self.pattern()?))
                } else {
                    None
                };
                Pattern::List {
                    before,
                    repeat,
                    tail,
                }
            }
            5 => {
                let before = self.patterns()?;
                let repeat = self.repeat()?;
                Pattern::Vector { before, repeat }
            }
            b => return Err(self.r.error(format!("unknown pattern tag {b}"))),
        })
    }

    fn repeat(&mut self) -> Decoded<Option<Repeat>> {
        if !self.r.bool()? {
            return Ok(None);
        }
        let start = self.facts.vars.len();
        let each = Box::new(self.pattern()?);
        let end = self.facts.vars.len();
        let vars = self.depths()?;
        self.facts.repeats.push((start, end, vars.clone()));
        let after = self.patterns()?;
        Ok(Some(Repeat { each, vars, after }))
    }

    fn items(&mut self) -> Decoded<Vec<(Template, usize)>> {
        let n = self.r.count(2)?;
        (0..n)
            .map(|_| {
                let t = self.template()?;
                Ok((t, self.index()?))
            })
            .collect()
    }

    fn template(&mut self) -> Decoded<Template> {
        self.nested("a template", Self::template_here)
    }

    fn template_here(&mut self) -> Decoded<Template> {
        Ok(match self.r.byte()? {
            0 => Template::Var(self.index()?),
            1 => Template::Constant(self.syntax()?),
            2 => {
                let items = self.items()?;
                let tail = if self.r.bool()? {
                    Some(Box::new(self.template()?))
                } else {
                    None
                };
                let syntax = self.syntax()?;
                Template::List {
                    items,
                    tail,
                    syntax,
                }
            }
            3 => {
                let items = self.items()?;
                let syntax = self.syntax()?;
                Template::Vector { items, syntax }
            }
            b => return Err(self.r.error(format!("unknown template tag {b}"))),
        })
    }
}

/// One more than the highest pattern variable `p` names, or 0.
fn max_var(p: &Pattern) -> usize {
    let mut most = 0;
    let mut pending = vec![p];
    while let Some(p) = pending.pop() {
        match p {
            Pattern::Var(n) => most = most.max(n.saturating_add(1)),
            Pattern::List {
                before,
                repeat,
                tail,
            } => {
                pending.extend(before);
                pending.extend(tail.as_deref());
                if let Some(r) = repeat {
                    pending.push(&r.each);
                    pending.extend(&r.after);
                    most = most.max(
                        r.vars
                            .iter()
                            .map(|v| v.saturating_add(1))
                            .max()
                            .unwrap_or(0),
                    );
                }
            }
            Pattern::Vector { before, repeat } => {
                pending.extend(before);
                if let Some(r) = repeat {
                    pending.push(&r.each);
                    pending.extend(&r.after);
                    most = most.max(
                        r.vars
                            .iter()
                            .map(|v| v.saturating_add(1))
                            .max()
                            .unwrap_or(0),
                    );
                }
            }
            Pattern::Discard | Pattern::Literal(_) | Pattern::Datum(_) => {}
        }
    }
    most
}

/// One more than the highest pattern variable `t` uses, or 0.
fn max_template_var(t: &Template) -> usize {
    let mut most = 0;
    let mut pending = vec![t];
    while let Some(t) = pending.pop() {
        match t {
            Template::Var(n) => most = most.max(n.saturating_add(1)),
            Template::List { items, tail, .. } => {
                pending.extend(items.iter().map(|(t, _)| t));
                pending.extend(tail.as_deref());
            }
            Template::Vector { items, .. } => pending.extend(items.iter().map(|(t, _)| t)),
            Template::Constant(_) => {}
        }
    }
    most
}

/// What a library's `library` section holds: who it is, what it imports,
/// and what it exports.
pub(super) struct LibraryRecord {
    pub(super) identity: String,
    pub(super) key: String,
    pub(super) imports: Vec<usize>,
    pub(super) interface: HashMap<String, Binding>,
}

/// Encodes a library's record, naming units by their identities.
pub(super) fn encode_library(record: &LibraryRecord, identities: &[String]) -> Result<Vec<u8>> {
    let mut e = Encoder {
        w: Writer::new(),
        sources: &Sources::new(),
        identities,
        units: HashMap::new(),
        source_refs: HashMap::new(),
        origins: HashMap::new(),
    };
    e.w.str(&record.identity);
    e.w.str(&record.key);
    e.w.usize(record.imports.len());
    for &unit in &record.imports {
        e.unit(unit)?;
    }
    let mut names: Vec<&String> = record.interface.keys().collect();
    names.sort();
    e.w.usize(names.len());
    for name in names {
        e.w.str(name);
        e.binding(&record.interface[name])?;
    }
    Ok(e.w.finish())
}

/// Decodes a library's record, resolving the units it names.
pub(super) fn decode_library(
    bytes: &[u8],
    resolve: &dyn Fn(&str) -> Option<usize>,
) -> std::result::Result<LibraryRecord, CompileError> {
    let mut sources = Sources::new();
    let mut d = Decoder {
        r: Reader::new(bytes),
        context: ScopeContext::new(),
        scopes: Vec::new(),
        sets: Vec::new(),
        sources: &mut sources,
        source_ids: Vec::new(),
        resolve,
        units: Vec::new(),
        origins: Vec::new(),
        depth: 0,
        facts: Facts::default(),
    };
    let record = (|| {
        let identity = d.r.str()?.to_owned();
        let key = d.r.str()?.to_owned();
        let n = d.r.count(1)?;
        let mut imports = Vec::with_capacity(n);
        for _ in 0..n {
            imports.push(d.unit()?);
        }
        let n = d.r.count(2)?;
        let mut interface = HashMap::with_capacity(n);
        for _ in 0..n {
            let name = d.r.str()?.to_owned();
            let binding = d.binding()?;
            if interface.insert(name.clone(), binding).is_some() {
                return Err(d.r.error(format!("{name} is exported twice")));
            }
        }
        d.r.end()?;
        Ok(LibraryRecord {
            identity,
            key,
            imports,
            interface,
        })
    })();
    record.map_err(|e: DecodeError| CompileError {
        message: format!("a library record: {e}"),
        span: None,
        location: None,
    })
}
