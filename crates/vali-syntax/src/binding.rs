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
use std::sync::Mutex;

use crate::scope::{Scope, ScopeContext, ScopeSetId};

/// The fewest keys a symbol has before its results are memoised. A symbol
/// bound in few places is cheap to resolve outright, and so costs nothing
/// extra; a test can lower this to run every symbol through the memo.
#[cfg(not(test))]
const MEMO_MIN_KEYS: usize = 16;
#[cfg(test)]
const MEMO_MIN_KEYS: usize = 1;

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
    entries: HashMap<S, Symbol<B>>,
}

/// Where a candidate is: its key, and its place among that key's entries.
type At = (Option<Scope>, usize);

/// One symbol's entries, and what resolving it has found.
#[derive(Debug)]
struct Symbol<B> {
    by_key: HashMap<Option<Scope>, Vec<Entry<B>>>,
    /// The maximal candidates (those no other candidate's set contains) for
    /// an identifier's set at a phase. [`BindingTable::resolve`] takes
    /// `&self`, and the table is shared between threads (the evaluator that
    /// holds one is `Send`, and expansion contexts are `Sync`), so the memo
    /// sits behind a lock, which is never contended: one expansion uses a
    /// table at a time.
    memo: Mutex<Memo>,
}

#[derive(Debug, Default)]
struct Memo {
    by_set: HashMap<(ScopeSetId, Phase), Vec<At>>,
    /// The newest scope any memoised set has.
    newest: Option<Scope>,
}

impl<B> Default for Symbol<B> {
    fn default() -> Self {
        Symbol {
            by_key: HashMap::new(),
            memo: Mutex::default(),
        }
    }
}

impl<B: Clone> Clone for Symbol<B> {
    fn clone(&self) -> Self {
        Symbol {
            by_key: self.by_key.clone(),
            memo: Mutex::default(),
        }
    }
}

impl<B> Symbol<B> {
    /// Forgets what is memoised, if adding or replacing an entry kept by
    /// `key` could change it.
    ///
    /// A memoised result is for a set, and an entry is a candidate for a
    /// set only if its scopes are a subset of the set's, which puts `key`,
    /// the newest scope of the entry's set, among the set's scopes. Scopes
    /// are numbered in the order they are made, so a set's newest scope is
    /// its largest. A `key` above the newest scope of every memoised set
    /// is therefore in none of them, and none of their results change.
    /// Entries are bound in the order their scopes are made, as nested
    /// code does, so this keeps the memo; a binding made later in an
    /// outer scope, such as an internal definition, clears it.
    fn touch(&mut self, key: Option<Scope>) {
        let memo = self.memo.get_mut().expect("not poisoned");
        if key <= memo.newest {
            *memo = Memo::default();
        }
    }
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
        let k = key(context, scopes);
        let sym = self.entries.entry(symbol).or_default();
        sym.touch(k);
        let entries = sym.by_key.entry(k).or_default();
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
        let k = key(context, scopes);
        let sym = self.entries.entry(symbol).or_default();
        // Replacing shifts the entries of a key, which memoised results
        // point at, and a REPL redefines top-level names: start over.
        sym.touch(None);
        let entries = sym.by_key.entry(k).or_default();
        entries.retain(|e| !(e.scopes == scopes && e.phases.overlaps(&phases)));
        entries.push(Entry {
            scopes,
            phases,
            binding,
        });
    }

    /// Drops what resolving has memoised, which only saves time, so a
    /// finished table that is kept for its entries does not keep it too.
    pub fn clear_caches(&mut self) {
        for sym in self.entries.values_mut() {
            *sym.memo.get_mut().expect("memo") = Memo::default();
        }
    }

    /// Every entry: its symbol, scope set, phases and binding, in no
    /// particular order.
    pub fn entries(&self) -> impl Iterator<Item = (&S, ScopeSetId, &Phases, &B)> {
        self.entries.iter().flat_map(|(symbol, sym)| {
            sym.by_key
                .values()
                .flatten()
                .map(move |e| (symbol, e.scopes, &e.phases, &e.binding))
        })
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
        let Some(sym) = self.entries.get(symbol) else {
            return Resolution::Unbound;
        };
        let maximal = self.maximal(context, sym, scopes, phase);
        let at = |&(k, i): &At| &sym.by_key[&k][i].binding;
        match maximal.as_slice() {
            [] => Resolution::Unbound,
            [one] => Resolution::Bound(at(one)),
            many => Resolution::Ambiguous(many.iter().map(at).collect()),
        }
    }

    /// The maximal candidates for `scopes` at `phase`.
    fn maximal(
        &self,
        context: &ScopeContext,
        sym: &Symbol<B>,
        scopes: ScopeSetId,
        phase: Phase,
    ) -> Vec<At> {
        // Without many keys, finding the candidates is cheap.
        if sym.by_key.len() < MEMO_MIN_KEYS {
            return Self::candidates_maximal(context, sym, scopes, phase);
        }
        if let Some(m) = sym.memo.lock().expect("memo").by_set.get(&(scopes, phase)) {
            return m.clone();
        }
        // The candidates of a set are those of its parent (the set without
        // its newest scope) plus the entries kept by that scope: an entry
        // whose set is a subset of this one and has the newest scope is kept
        // by it, and any other entry's set lacks it, so is a subset of the
        // parent. And the maximal candidates of the set are the maximal ones
        // among the parent's maximal candidates and the new entries, since
        // a candidate under another stays under it. So where the parent's
        // result is memoised, or further up, build on it.
        let mut chain = vec![scopes];
        let mut base = None;
        // Each step up is a lookup of the entries kept by one scope, which
        // is cheaper than finding the candidates outright, and every set
        // passed is memoised, so a walk is paid for once. Code that
        // expands inner forms first, as a `lambda` applied to an argument
        // does, resolves the deepest set before any above it.
        loop {
            let cur = *chain.last().expect("not empty");
            let Some(parent) = context.parent(cur) else {
                break;
            };
            if let Some(m) = sym.memo.lock().expect("memo").by_set.get(&(parent, phase)) {
                base = Some(m.clone());
                break;
            }
            chain.push(parent);
        }
        let mut result = match base {
            Some(m) => m,
            None => {
                // No memoised set above this one to build on.
                let last = chain.pop().expect("not empty");
                let m = Self::candidates_maximal(context, sym, last, phase);
                Self::remember(context, sym, last, phase, &m);
                m
            }
        };
        // `chain` now holds the sets above the base, newest first.
        while let Some(set) = chain.pop() {
            let newest = *context.scopes(set).last().expect("not empty");
            if let Some(entries) = sym.by_key.get(&Some(newest)) {
                let mut all = result;
                let fresh = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.phases.contains(phase) && context.is_subset(e.scopes, set));
                all.extend(fresh.map(|(i, _)| (Some(newest), i)));
                result = Self::maximal_of(context, sym, all);
            }
            Self::remember(context, sym, set, phase, &result);
        }
        result
    }

    fn remember(
        context: &ScopeContext,
        sym: &Symbol<B>,
        set: ScopeSetId,
        phase: Phase,
        maximal: &[At],
    ) {
        let mut memo = sym.memo.lock().expect("memo");
        memo.by_set.insert((set, phase), maximal.to_vec());
        let newest = context.scopes(set).last().copied();
        if newest > memo.newest {
            memo.newest = newest;
        }
    }

    /// The candidates in `all` that no other candidate's set contains.
    fn maximal_of(context: &ScopeContext, sym: &Symbol<B>, all: Vec<At>) -> Vec<At> {
        let scopes = |&(k, i): &At| sym.by_key[&k][i].scopes;
        all.iter()
            .filter(|c| {
                !all.iter()
                    .any(|o| scopes(o) != scopes(c) && context.is_subset(scopes(c), scopes(o)))
            })
            .copied()
            .collect()
    }

    /// Finds every candidate for `scopes`, and keeps the maximal ones.
    fn candidates_maximal(
        context: &ScopeContext,
        sym: &Symbol<B>,
        scopes: ScopeSetId,
        phase: Phase,
    ) -> Vec<At> {
        let by_key = &sym.by_key;
        let own = context.scopes(scopes);
        let keep = |e: &Entry<B>| e.phases.contains(phase) && context.is_subset(e.scopes, scopes);
        let mut keyed: Vec<At> = if by_key.len() <= own.len() {
            by_key
                .iter()
                .filter(|(k, _)| k.is_none_or(|k| own.binary_search(&k).is_ok()))
                .flat_map(|(&k, entries)| entries.iter().enumerate().map(move |(i, e)| (k, i, e)))
                .filter(|(_, _, e)| keep(e))
                .map(|(k, i, _)| (k, i))
                .collect()
        } else {
            std::iter::once(None)
                .chain(own.iter().copied().map(Some))
                .filter_map(|k| by_key.get(&k).map(|v| (k, v)))
                .flat_map(|(k, entries)| entries.iter().enumerate().map(move |(i, e)| (k, i, e)))
                .filter(|(_, _, e)| keep(e))
                .map(|(k, i, _)| (k, i))
                .collect()
        };
        keyed.sort_by_key(|&(k, _)| k);
        let scopes = |&(k, i): &At| sym.by_key[&k][i].scopes;
        // The largest set wins if it holds all the others, which is one
        // test each; only otherwise is the quadratic search needed.
        let Some(best) = keyed.iter().max_by_key(|c| context.scopes(scopes(c)).len()) else {
            return keyed;
        };
        if keyed
            .iter()
            .all(|c| context.is_subset(scopes(c), scopes(best)))
        {
            return vec![*best];
        }
        Self::maximal_of(context, sym, keyed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::ScopeKind;

    struct Fixture {
        cx: ScopeContext,
        table: BindingTable<&'static str, u32>,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                cx: ScopeContext::new(),
                table: BindingTable::new(),
            }
        }

        fn scope(&mut self) -> Scope {
            self.cx.new_scope(ScopeKind::Binder).unwrap()
        }

        fn set(&mut self, scopes: &[Scope]) -> ScopeSetId {
            self.cx.intern(scopes).unwrap()
        }

        fn bind(&mut self, scopes: ScopeSetId, binding: u32) {
            self.table
                .bind(&self.cx, "x", scopes, Phases::All, binding)
                .unwrap();
        }

        /// What `x` resolves to at `scopes`, which must agree with finding
        /// the candidates outright, as a symbol with few keys does.
        fn resolve(&self, scopes: ScopeSetId) -> Vec<u32> {
            let sym = &self.table.entries["x"];
            let mut direct: Vec<u32> =
                BindingTable::<&str, u32>::candidates_maximal(&self.cx, sym, scopes, Phase::RUN)
                    .iter()
                    .map(|(k, i)| sym.by_key[k][*i].binding)
                    .collect();
            let mut got = match self.table.resolve(&self.cx, "x", scopes, Phase::RUN) {
                Resolution::Bound(b) => vec![*b],
                Resolution::Unbound => vec![],
                Resolution::Ambiguous(bs) => bs.into_iter().copied().collect(),
            };
            direct.sort();
            got.sort();
            assert_eq!(got, direct, "the memo and the candidates agree");
            got
        }
    }

    #[test]
    fn nested_binders_build_on_their_parents() {
        let mut f = Fixture::new();
        let mut sets = vec![];
        let mut scopes = vec![];
        for level in 0..40u32 {
            scopes.push(f.scope());
            let set = f.set(&scopes);
            let before = sets.last().copied().unwrap_or(ScopeSetId::EMPTY);
            assert_eq!(f.cx.parent(set), Some(before));
            f.bind(set, level);
            sets.push(set);
        }
        // Resolved in a scattered order, so some sets build on a memoised
        // parent and some on one a few levels up.
        for level in [39, 3, 20, 21, 0, 38, 10, 11, 12, 13, 14, 15, 16, 17, 30] {
            assert_eq!(f.resolve(sets[level]), vec![level as u32]);
        }
    }

    #[test]
    fn a_later_binding_in_an_outer_scope_is_seen() {
        let mut f = Fixture::new();
        let [a, b] = [f.scope(), f.scope()];
        let (ab, a_only) = (f.set(&[a, b]), f.set(&[a]));
        let c = f.scope();
        let elsewhere = f.set(&[c]);
        f.bind(elsewhere, 0);
        assert_eq!(f.resolve(ab), Vec::<u32>::new());
        // Bound after `ab` was resolved, with a key older than its newest.
        f.bind(a_only, 1);
        assert_eq!(f.resolve(ab), vec![1]);
        // And a binding for `ab` itself beats it.
        f.bind(ab, 2);
        assert_eq!(f.resolve(ab), vec![2]);
        assert_eq!(f.resolve(a_only), vec![1]);
    }

    #[test]
    fn two_maximal_candidates_are_ambiguous() {
        let mut f = Fixture::new();
        let [a, b, c] = [f.scope(), f.scope(), f.scope()];
        let (ab, ac, abc) = (f.set(&[a, b]), f.set(&[a, c]), f.set(&[a, b, c]));
        f.bind(ab, 1);
        f.bind(ac, 2);
        assert_eq!(f.resolve(abc), vec![1, 2]);
        // Again, from the memo, and with a parent resolved.
        assert_eq!(f.resolve(abc), vec![1, 2]);
        assert_eq!(f.resolve(ab), vec![1]);
        // A binding with a larger set settles it.
        f.bind(abc, 3);
        assert_eq!(f.resolve(abc), vec![3]);
    }

    #[test]
    fn rebinding_replaces_what_was_memoised() {
        let mut f = Fixture::new();
        let [a, b] = [f.scope(), f.scope()];
        let (a_only, ab) = (f.set(&[a]), f.set(&[a, b]));
        f.bind(a_only, 1);
        assert_eq!(f.resolve(ab), vec![1]);
        f.table.rebind(&f.cx, "x", a_only, Phases::All, 2);
        assert_eq!(f.resolve(ab), vec![2]);
        assert_eq!(f.resolve(a_only), vec![2]);
    }

    #[test]
    fn phases_are_memoised_apart() {
        let mut f = Fixture::new();
        let a = f.scope();
        let set = f.set(&[a]);
        f.table
            .bind(&f.cx, "x", set, Phases::Only(vec![Phase::EXPAND]), 7)
            .unwrap();
        assert_eq!(
            f.table.resolve(&f.cx, "x", set, Phase::RUN),
            Resolution::Unbound
        );
        assert_eq!(
            f.table.resolve(&f.cx, "x", set, Phase::EXPAND),
            Resolution::Bound(&7)
        );
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    use crate::scope::ScopeKind;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// The maximal candidates, found by testing every entry.
    fn oracle(
        table: &BindingTable<u8, u32>,
        cx: &ScopeContext,
        s: u8,
        set: ScopeSetId,
        p: Phase,
    ) -> Vec<u32> {
        let cands: Vec<(ScopeSetId, u32)> = table
            .entries()
            .filter(|(sym, scopes, phases, _)| {
                **sym == s && phases.contains(p) && cx.is_subset(*scopes, set)
            })
            .map(|(_, scopes, _, b)| (scopes, *b))
            .collect();
        let mut maximal: Vec<u32> = cands
            .iter()
            .filter(|(c, _)| !cands.iter().any(|(o, _)| o != c && cx.is_subset(*c, *o)))
            .map(|&(_, b)| b)
            .collect();
        maximal.sort();
        maximal
    }

    fn got(
        table: &BindingTable<u8, u32>,
        cx: &ScopeContext,
        s: u8,
        set: ScopeSetId,
        p: Phase,
    ) -> Vec<u32> {
        match table.resolve(cx, &s, set, p) {
            Resolution::Unbound => vec![],
            Resolution::Bound(b) => vec![*b],
            Resolution::Ambiguous(bs) => {
                let mut v: Vec<u32> = bs.into_iter().copied().collect();
                v.sort();
                v
            }
        }
    }

    fn run(seed: u64, steps: usize) {
        let mut rng = Rng(seed);
        let mut cx = ScopeContext::new();
        let mut table: BindingTable<u8, u32> = BindingTable::new();
        let mut sets = vec![ScopeSetId::EMPTY];
        let mut scopes: Vec<Scope> = Vec::new();
        let mut next = 0u32;
        for step in 0..steps {
            match rng.below(100) {
                // A nested set: an existing set and a fresh scope.
                0..=24 => {
                    let base = sets[rng.below(sets.len())];
                    let s = cx.new_scope(ScopeKind::Binder).unwrap();
                    scopes.push(s);
                    let mut v = cx.scopes(base).to_vec();
                    v.push(s);
                    sets.push(cx.intern(&v).unwrap());
                }
                // An existing set with an older scope added, or one removed.
                25..=29 if !scopes.is_empty() => {
                    let mut v = cx.scopes(sets[rng.below(sets.len())]).to_vec();
                    if rng.below(2) == 0 && !v.is_empty() {
                        let i = rng.below(v.len());
                        v.remove(i);
                    } else {
                        v.push(scopes[rng.below(scopes.len())]);
                        v.sort();
                        v.dedup();
                    }
                    sets.push(cx.intern(&v).unwrap());
                }
                30..=44 => {
                    let s = rng.below(3) as u8;
                    let set = sets[rng.below(sets.len())];
                    let phases = match rng.below(4) {
                        0 => Phases::Only(vec![Phase(0)]),
                        1 => Phases::Only(vec![Phase(1)]),
                        _ => Phases::All,
                    };
                    next += 1;
                    if rng.below(10) == 0 {
                        table.rebind(&cx, s, set, phases, next);
                    } else {
                        let _ = table.bind(&cx, s, set, phases, next);
                    }
                }
                _ => {
                    let s = rng.below(3) as u8;
                    let set = sets[rng.below(sets.len())];
                    let p = Phase(rng.below(2) as i32);
                    assert_eq!(
                        got(&table, &cx, s, set, p),
                        oracle(&table, &cx, s, set, p),
                        "seed {seed}, step {step}, symbol {s}, set {:?}, phase {p:?}",
                        cx.scopes(set)
                    );
                }
            }
        }
    }

    #[test]
    fn the_memo_agrees_with_testing_every_entry() {
        for seed in 1..=300u64 {
            run(seed.wrapping_mul(0x9e3779b97f4a7c15) | 1, 1500);
        }
    }
}
