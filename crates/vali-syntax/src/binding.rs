//! The binding table (ADR 0015 #2 and #3).
//!
//! The table maps a symbol and a scope set to a binding, with the phases
//! where the binding is visible. An identifier refers to the entry, among
//! those visible at its phase whose scope sets are subsets of its own, whose
//! set contains all the others.
//!
//! Each symbol's entries are kept by the newest scope in their sets, as
//! Racket keeps bindings in their scopes: an entry whose set is a subset of
//! an identifier's has that scope among the identifier's, so resolving
//! looks only at the identifier's own scopes' entries. A symbol bound in
//! a thousand bodies, such as a common parameter name, costs a lookup no
//! more than one bound once, so expansion stays linear in the program.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

use crate::scope::{Scope, ScopeContext, ScopeSetId};

/// A phase: 0 for run time, 1 for the expansion of phase-0 code, and so on.
/// R6RS `meta` levels can be negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Phase(pub i32);

impl Phase {
    /// Run time.
    pub const RUN: Phase = Phase(0);
    /// The expansion of run-time code.
    pub const EXPAND: Phase = Phase(1);
}

/// The phases where a binding is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phases {
    /// Every phase: implicit phasing, and local bindings.
    All,
    /// Only these, as an R6RS `for` import lists them.
    Only(Vec<Phase>),
}

impl Phases {
    /// Whether `phase` is one of these.
    pub fn contains(&self, phase: Phase) -> bool {
        match self {
            Phases::All => true,
            Phases::Only(phases) => phases.contains(&phase),
        }
    }

    /// Whether some phase is in both.
    pub fn overlaps(&self, other: &Phases) -> bool {
        match (self, other) {
            (Phases::All, _) | (_, Phases::All) => true,
            (Phases::Only(a), Phases::Only(_)) => a.iter().any(|&p| other.contains(p)),
        }
    }
}

#[derive(Clone, Debug)]
struct Entry<B> {
    scopes: ScopeSetId,
    phases: Phases,
    binding: B,
}

/// The binding table for one expansion context. `S` is the symbol type and
/// `B` whatever the expander binds identifiers to.
#[derive(Clone, Debug)]
pub struct BindingTable<S, B> {
    /// Each symbol's entries, by the newest scope of their sets, `None` for
    /// the empty set.
    entries: HashMap<S, HashMap<Option<Scope>, Vec<Entry<B>>>>,
}

/// The scope an entry with `scopes` is kept by.
fn key(context: &ScopeContext, scopes: ScopeSetId) -> Option<Scope> {
    context.scopes(scopes).last().copied()
}

impl<S, B> Default for BindingTable<S, B> {
    fn default() -> Self {
        BindingTable {
            entries: HashMap::new(),
        }
    }
}

/// What an identifier refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution<'a, B> {
    /// One binding.
    Bound(&'a B),
    /// No binding is visible.
    Unbound,
    /// No candidate's scope set contains all the others'. These are the
    /// candidates whose sets no other candidate's set contains.
    Ambiguous(Vec<&'a B>),
}

/// [`BindingTable::bind`] found the symbol already bound with the same scope
/// set at a phase in common.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlreadyBound<'a, B> {
    /// The existing binding.
    pub existing: &'a B,
}

impl<S: Eq + Hash, B> BindingTable<S, B> {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `symbol` with `scopes` to `binding` at `phases`. Refuses if the
    /// same symbol and set are already bound at a phase in common, since
    /// every reference there would be ambiguous; the expander decides
    /// whether that is an error.
    pub fn bind(
        &mut self,
        context: &ScopeContext,
        symbol: S,
        scopes: ScopeSetId,
        phases: Phases,
        binding: B,
    ) -> Result<(), AlreadyBound<'_, B>> {
        let entries = self
            .entries
            .entry(symbol)
            .or_default()
            .entry(key(context, scopes))
            .or_default();
        if let Some(at) = entries
            .iter()
            .position(|e| e.scopes == scopes && e.phases.overlaps(&phases))
        {
            return Err(AlreadyBound {
                existing: &entries[at].binding,
            });
        }
        entries.push(Entry {
            scopes,
            phases,
            binding,
        });
        Ok(())
    }

    /// Binds `symbol` with `scopes` to `binding` at `phases`, replacing
    /// whatever was bound with the same set at a phase in common: a REPL's
    /// redefinitions.
    pub fn rebind(
        &mut self,
        context: &ScopeContext,
        symbol: S,
        scopes: ScopeSetId,
        phases: Phases,
        binding: B,
    ) {
        let entries = self
            .entries
            .entry(symbol)
            .or_default()
            .entry(key(context, scopes))
            .or_default();
        entries.retain(|e| !(e.scopes == scopes && e.phases.overlaps(&phases)));
        entries.push(Entry {
            scopes,
            phases,
            binding,
        });
    }

    /// What `symbol` with `scopes` refers to at `phase`.
    pub fn resolve<Q>(
        &self,
        context: &ScopeContext,
        symbol: &Q,
        scopes: ScopeSetId,
        phase: Phase,
    ) -> Resolution<'_, B>
    where
        S: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let Some(by_key) = self.entries.get(symbol) else {
            return Resolution::Unbound;
        };
        // Only entries kept by one of the identifier's own scopes can have
        // sets that are subsets of its set.
        let keys = std::iter::once(None).chain(context.scopes(scopes).iter().copied().map(Some));
        let candidates: Vec<&Entry<B>> = keys
            .filter_map(|k| by_key.get(&k))
            .flatten()
            .filter(|e| e.phases.contains(phase) && context.is_subset(e.scopes, scopes))
            .collect();
        let Some(best) = candidates
            .iter()
            .max_by_key(|e| context.scopes(e.scopes).len())
        else {
            return Resolution::Unbound;
        };
        if candidates
            .iter()
            .all(|e| context.is_subset(e.scopes, best.scopes))
        {
            return Resolution::Bound(&best.binding);
        }
        let maximal = candidates
            .iter()
            .filter(|e| {
                !candidates
                    .iter()
                    .any(|o| o.scopes != e.scopes && context.is_subset(e.scopes, o.scopes))
            })
            .map(|e| &e.binding)
            .collect();
        Resolution::Ambiguous(maximal)
    }
}
