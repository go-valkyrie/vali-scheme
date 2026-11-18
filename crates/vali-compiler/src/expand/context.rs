//! Scope contexts and syntax exports: each unit expands in a context of
//! its own, and what it exports is remapped into its importers' (ADR
//! 0015 #5).

use super::*;

/// A macro, as one unit's context holds it: its own, or a copy of one it
/// imported, remapped into its context.
#[derive(Clone)]
pub(super) struct Macro {
    pub(super) name: String,
    pub(super) transformer: Transformer,
}

impl Macro {
    /// This macro, defined in `from`, with its syntax remapped into `to`.
    pub(super) fn remap(
        &self,
        remap: &mut Remap,
        from: &ScopeContext,
        to: &mut ScopeContext,
    ) -> std::result::Result<Macro, TooManyScopes> {
        let transformer = match &self.transformer {
            Transformer::Rules(clauses) => Transformer::Rules(
                clauses
                    .iter()
                    .map(|c| {
                        Ok(Clause {
                            pattern: c.pattern.remap(remap, from, to)?,
                            template: c.template.remap(remap, from, to)?,
                            depths: c.depths.clone(),
                        })
                    })
                    .collect::<std::result::Result<_, TooManyScopes>>()?,
            ),
            // Its syntax is in its unit's compiled table, remapped with it.
            Transformer::Procedure(p) => Transformer::Procedure(Procedure {
                ir: Arc::clone(&p.ir),
                code: Arc::clone(&p.code),
                phase: p.phase,
                made: None,
            }),
        };
        Ok(Macro {
            name: self.name.clone(),
            transformer,
        })
    }
}

/// Each unit's compiled syntax, remapped from `from` into `to`.
pub(super) fn remap_compiled(
    compiled: &Compiled,
    remap: &mut Remap,
    from: &ScopeContext,
    to: &mut ScopeContext,
) -> std::result::Result<Compiled, TooManyScopes> {
    Ok(Compiled {
        quoted: compiled
            .quoted
            .iter()
            .map(|s| s.remap(remap, from, to))
            .collect::<std::result::Result<_, _>>()?,
        patterns: compiled
            .patterns
            .iter()
            .map(|(p, n)| Ok((p.remap(remap, from, to)?, *n)))
            .collect::<std::result::Result<_, TooManyScopes>>()?,
        templates: compiled
            .templates
            .iter()
            .map(|(t, d)| Ok((t.remap(remap, from, to)?, d.clone())))
            .collect::<std::result::Result<_, TooManyScopes>>()?,
    })
}

/// Binds in `table`, in `to`, every binding of `from_table`, in `from`,
/// whose scopes `remap` has all met: no other could match an identifier
/// remapped so far. Taken in a fixed order, so the sets made are numbered
/// the same every time (ADR 0001 #4).
pub(super) fn remap_bindings(
    from_table: &BindingTable<String, Binding>,
    from: &ScopeContext,
    remap: &mut Remap,
    table: &mut BindingTable<String, Binding>,
    to: &mut ScopeContext,
) -> std::result::Result<(), TooManyScopes> {
    let mut entries: Vec<_> = from_table
        .entries()
        .filter(|(_, _, _, binding)| {
            !matches!(binding, Binding::Local(_) | Binding::PatternVar(..))
        })
        .collect();
    entries.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    for (symbol, scopes, phases, binding) in entries {
        if !remap.has_met_all(from, scopes) {
            continue;
        }
        let scopes = remap.set(from, to, scopes)?;
        // Bound already, the same way, through another import.
        let _ = table.bind(to, symbol.clone(), scopes, phases.clone(), binding.clone());
    }
    Ok(())
}

/// Remaps `export`, a syntax export, into a context whose core forms are
/// bound in `core_scope`: its macros and compiled syntax not there
/// already, then the bindings they reach. Taken in a fixed order, so the
/// scopes made are numbered the same every time (ADR 0001 #4).
/// Only what `own` keeps of the macros and compiled syntax is remapped,
/// by the macro's unit and the compiled syntax's key.
pub(super) fn remap_export(
    export: &ContextState,
    core_scope: Scope,
    context: &mut ScopeContext,
    macros: &mut HashMap<MacroId, Macro>,
    compiled: &mut phase::CompiledByUnit,
    table: &mut BindingTable<String, Binding>,
    own: Option<(u32, &str)>,
) -> std::result::Result<(), TooManyScopes> {
    let from = &export.context;
    let mut remap = Remap::new();
    remap.identify(export.core_scope, core_scope);
    let mut ids: Vec<MacroId> = export
        .macros
        .keys()
        .copied()
        .filter(|id| own.is_none_or(|(unit, _)| id.unit == unit))
        .collect();
    ids.sort();
    for id in ids {
        if let std::collections::hash_map::Entry::Vacant(entry) = macros.entry(id) {
            entry.insert(export.macros[&id].remap(&mut remap, from, context)?);
        }
    }
    let mut names: Vec<&String> = export
        .compiled
        .keys()
        .filter(|name| own.is_none_or(|(_, key)| *name == key))
        .collect();
    names.sort();
    for name in names {
        if !compiled.contains_key(name) {
            let copy = remap_compiled(&export.compiled[name], &mut remap, from, context)?;
            compiled.insert(name.clone(), copy);
        }
    }
    remap_bindings(&export.table, from, &mut remap, table, context)
}

/// Whether the optimiser runs: off when the `VALI_NO_OPT` environment
/// variable is set, so the test suite can check that it changes nothing a
/// program can see.
pub(super) fn optimising() -> bool {
    static OPTIMISING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OPTIMISING.get_or_init(|| std::env::var_os("VALI_NO_OPT").is_none())
}

/// Whether every library's syntax export, and later its whole artifact, is
/// encoded and decoded as it is finished, so the test suite runs on what
/// artifacts hold: set by the `VALI_ROUND_TRIP` environment variable.
pub(super) fn round_trip() -> bool {
    static ROUND_TRIP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ROUND_TRIP.get_or_init(|| std::env::var_os("VALI_ROUND_TRIP").is_some())
}

/// Merges `from`, a program's syntax data for run time, into `into`, the
/// isolate's, so syntax objects of programs loaded before keep their
/// meaning: its scopes, compiled syntax and sources are remapped into
/// `into`'s. Compiled syntax under a key `into` holds already is the same
/// unit's, and is kept (`Unit::key`).
pub(crate) fn merge_syntax(
    into: &mut Shared,
    from: Shared,
) -> std::result::Result<(), TooManyScopes> {
    let mut remap = Remap::new();
    remap.identify(from.core_scope, into.core_scope);
    remap.move_sources(into.sources.absorb(&from.sources));
    let mut keys: Vec<&String> = from.compiled.keys().collect();
    keys.sort();
    for key in keys {
        if !into.compiled.contains_key(key) {
            let copy = remap_compiled(
                &from.compiled[key],
                &mut remap,
                &from.context,
                &mut into.context,
            )?;
            into.compiled.insert(key.clone(), copy);
        }
    }
    remap_bindings(
        &from.table,
        &from.context,
        &mut remap,
        &mut into.table,
        &mut into.context,
    )
}

/// The scope context a unit expands in, and what is bound there: its own
/// definitions, and what its imports brought, remapped into it (ADR 0015
/// #5). A finished unit's state is its syntax export.
#[derive(Clone)]
pub(super) struct ContextState {
    pub(super) context: ScopeContext,
    pub(super) table: BindingTable<String, Binding>,
    /// The scope of the core forms, and its set.
    pub(super) core_scope: Scope,
    pub(super) core: ScopeSetId,
    pub(super) macros: HashMap<MacroId, Macro>,
    pub(super) compiled: phase::CompiledByUnit,
    /// The units whose syntax exports are remapped in.
    pub(super) imported: HashSet<usize>,
    /// The number of the next macro the unit defines.
    pub(super) next_macro: u32,
}

impl ContextState {
    /// A fresh context, with the core forms bound.
    pub(super) fn fresh() -> Result<ContextState> {
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
        Ok(ContextState {
            context,
            table,
            core_scope,
            core,
            macros: HashMap::new(),
            compiled: Default::default(),
            imported: HashSet::new(),
            next_macro: 0,
        })
    }
}

/// Who the live context belongs to.
#[derive(Clone, Copy, Debug)]
pub(super) enum ContextOwner {
    Unit(usize),
    /// The interaction environment, whose inputs share one context.
    Repl,
}

impl Expander {
    /// Makes `state` the live context, returning the one it replaces.
    pub(super) fn swap_state(&mut self, state: ContextState) -> ContextState {
        ContextState {
            context: std::mem::replace(&mut self.context, state.context),
            table: std::mem::replace(&mut self.table, state.table),
            core_scope: std::mem::replace(&mut self.core_scope, state.core_scope),
            core: std::mem::replace(&mut self.core, state.core),
            macros: std::mem::replace(&mut self.macros, state.macros),
            compiled: std::mem::replace(&mut self.compiled, state.compiled),
            imported: std::mem::replace(&mut self.imported_syntax, state.imported),
            next_macro: std::mem::replace(&mut self.next_macro, state.next_macro),
        }
    }

    /// Remaps the syntax export of `unit`, which the current unit imports,
    /// into the current context, once: its macros and compiled syntax,
    /// then every binding whose scopes all came across with them. A binding
    /// with any other scope could never match an identifier the importer
    /// sees. Everything is taken in a fixed order, so the scopes it makes
    /// are numbered the same every time (ADR 0001 #4).
    pub(super) fn import_syntax(&mut self, unit: usize) -> Result<()> {
        if !self.imported_syntax.insert(unit) {
            return Ok(());
        }
        self.import_dependencies(unit)?;
        let Some(export) = self.units[unit].syntax.clone() else {
            return Ok(());
        };
        remap_export(
            &export,
            self.core_scope,
            &mut self.context,
            &mut self.macros,
            &mut self.compiled,
            &mut self.table,
            None,
        )
        .map_err(too_many)
    }

    /// Imports the syntax of what `unit` imports, if its export holds only
    /// its own macros and compiled syntax, as one loaded from an artifact
    /// does: the macros its own expand into are its imports'.
    pub(super) fn import_dependencies(&mut self, unit: usize) -> Result<()> {
        if self.units[unit].pruned {
            for dependency in self.units[unit].imports.clone() {
                self.import_syntax(dependency)?;
            }
        }
        Ok(())
    }

    /// The bytes of `unit`'s syntax export `state` as an importer would
    /// see it: remapped into a context of its own first, with only its own
    /// macros and compiled syntax, and the bindings they reach. Its
    /// imports' macros come from their own exports.
    pub(super) fn encode_syntax(&self, unit: usize, state: &ContextState) -> Result<Vec<u8>> {
        let mut fresh = ContextState::fresh()?;
        let own = (
            u32::try_from(unit).expect("at most 2^32 units"),
            self.units[unit].key.as_str(),
        );
        remap_export(
            state,
            fresh.core_scope,
            &mut fresh.context,
            &mut fresh.macros,
            &mut fresh.compiled,
            &mut fresh.table,
            Some(own),
        )
        .map_err(too_many)?;
        fresh.next_macro = state.next_macro;
        let identities: Vec<String> = self.units.iter().map(|u| u.identity.clone()).collect();
        artifact::encode(&fresh, &identities, &self.sources).map_err(|message| CompileError {
            message: format!("encoding a syntax export: {message}"),
            span: None,
            location: None,
        })
    }

    /// Notes `m`, a macro the current unit defines, and returns its name.
    pub(super) fn add_macro(&mut self, m: Macro) -> MacroId {
        let id = MacroId {
            unit: u32::try_from(self.units.len() - 1).expect("at most 2^32 units"),
            index: self.next_macro,
        };
        self.next_macro += 1;
        self.macros.insert(id, m);
        id
    }
}
