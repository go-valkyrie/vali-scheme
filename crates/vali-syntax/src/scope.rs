//! Scopes and interned scope sets (ADR 0015).
//!
//! A [`ScopeContext`] numbers scopes and scope sets for one expansion, from
//! zero, so the numbers depend only on what is being expanded. Scope sets
//! are interned: equal sets get the same [`ScopeSetId`], so comparing two
//! sets compares two integers. Adding, removing and flipping a scope are
//! memoised.

use std::collections::HashMap;
use std::fmt;

/// A scope: an opaque identity, numbered within its [`ScopeContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scope(u32);

impl Scope {
    /// The scope's number within its context.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// What made a scope. Only tools look at it: resolution never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    /// Flipped on a macro's input and output, for one macro step.
    Macro,
    /// Added to a macro's input when it is used in a definition context.
    UseSite,
    /// Added by a binding form, such as `lambda` or `let`, to its body.
    Binder,
    /// Added to a library's body.
    Library,
    /// Anything else, such as a scope a host creates.
    Other,
}

/// An interned set of scopes, numbered within its [`ScopeContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeSetId(u32);

impl ScopeSetId {
    /// The empty set, which every context interns first.
    pub const EMPTY: ScopeSetId = ScopeSetId(0);

    /// The set's number within its context.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// A context ran out of numbers for scopes or scope sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyScopes {
    /// The limit that was reached.
    pub limit: u32,
}

impl fmt::Display for TooManyScopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "too many scopes in one expansion (the limit is {})",
            self.limit
        )
    }
}

impl std::error::Error for TooManyScopes {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Op {
    Add,
    Remove,
    Flip,
}

/// Numbers scopes and interns scope sets for one expansion.
#[derive(Clone, Debug)]
pub struct ScopeContext {
    limit: u32,
    kinds: Vec<ScopeKind>,
    sets: Vec<Box<[Scope]>>,
    interned: HashMap<Box<[Scope]>, ScopeSetId>,
    memo: HashMap<(ScopeSetId, Scope, Op), ScopeSetId>,
}

impl Default for ScopeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeContext {
    /// A context that can number up to `u32::MAX` scopes and as many sets.
    pub fn new() -> Self {
        Self::with_limit(u32::MAX)
    }

    /// A context that can number up to `limit` scopes and `limit` sets
    /// (at least one, since the empty set is always there). Hosts can use
    /// a lower limit to bound the work one expansion does.
    pub fn with_limit(limit: u32) -> Self {
        let empty: Box<[Scope]> = Box::new([]);
        ScopeContext {
            limit: limit.max(1),
            kinds: Vec::new(),
            sets: vec![empty.clone()],
            interned: HashMap::from([(empty, ScopeSetId::EMPTY)]),
            memo: HashMap::new(),
        }
    }

    /// A fresh scope.
    pub fn new_scope(&mut self, kind: ScopeKind) -> Result<Scope, TooManyScopes> {
        let index = self.next_index(self.kinds.len())?;
        self.kinds.push(kind);
        Ok(Scope(index))
    }

    /// What made `scope`.
    pub fn kind(&self, scope: Scope) -> ScopeKind {
        self.kinds[scope.0 as usize]
    }

    /// How many scopes this context has made.
    pub fn scope_count(&self) -> usize {
        self.kinds.len()
    }

    /// How many distinct sets this context has interned, the empty set
    /// included.
    pub fn set_count(&self) -> usize {
        self.sets.len()
    }

    /// The scopes in `set`, in increasing order.
    pub fn scopes(&self, set: ScopeSetId) -> &[Scope] {
        &self.sets[set.0 as usize]
    }

    /// Whether `set` contains `scope`.
    pub fn contains(&self, set: ScopeSetId, scope: Scope) -> bool {
        self.scopes(set).binary_search(&scope).is_ok()
    }

    /// Whether every scope in `a` is in `b`.
    pub fn is_subset(&self, a: ScopeSetId, b: ScopeSetId) -> bool {
        if a == b || a == ScopeSetId::EMPTY {
            return true;
        }
        let (a, b) = (self.scopes(a), self.scopes(b));
        if a.len() > b.len() {
            return false;
        }
        let mut rest = b.iter();
        a.iter().all(|x| rest.by_ref().any(|y| y == x))
    }

    /// The set of `scopes`, interned.
    pub fn intern(&mut self, scopes: &[Scope]) -> Result<ScopeSetId, TooManyScopes> {
        let mut sorted = scopes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        self.intern_sorted(sorted)
    }

    /// `set` with `scope` added.
    pub fn add(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Add)
    }

    /// `set` with `scope` removed.
    pub fn remove(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Remove)
    }

    /// `set` with `scope` added if it was absent, and removed if it was
    /// present.
    pub fn flip(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Flip)
    }

    fn apply(
        &mut self,
        set: ScopeSetId,
        scope: Scope,
        op: Op,
    ) -> Result<ScopeSetId, TooManyScopes> {
        if let Some(&result) = self.memo.get(&(set, scope, op)) {
            return Ok(result);
        }
        let scopes = self.scopes(set);
        let result = match (scopes.binary_search(&scope), op) {
            (Ok(_), Op::Add) | (Err(_), Op::Remove) => set,
            (Err(at), Op::Add | Op::Flip) => {
                let mut sorted = Vec::with_capacity(scopes.len() + 1);
                sorted.extend_from_slice(&scopes[..at]);
                sorted.push(scope);
                sorted.extend_from_slice(&scopes[at..]);
                self.intern_sorted(sorted)?
            }
            (Ok(at), Op::Remove | Op::Flip) => {
                let mut sorted = scopes.to_vec();
                sorted.remove(at);
                self.intern_sorted(sorted)?
            }
        };
        self.memo.insert((set, scope, op), result);
        Ok(result)
    }

    fn intern_sorted(&mut self, sorted: Vec<Scope>) -> Result<ScopeSetId, TooManyScopes> {
        if let Some(&id) = self.interned.get(sorted.as_slice()) {
            return Ok(id);
        }
        let id = ScopeSetId(self.next_index(self.sets.len())?);
        let sorted = sorted.into_boxed_slice();
        self.sets.push(sorted.clone());
        self.interned.insert(sorted, id);
        Ok(id)
    }

    fn next_index(&self, count: usize) -> Result<u32, TooManyScopes> {
        u32::try_from(count)
            .ok()
            .filter(|&n| n < self.limit)
            .ok_or(TooManyScopes { limit: self.limit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_sets_are_interned_once() {
        let mut cx = ScopeContext::new();
        let [a, b] = [0; 2].map(|_| cx.new_scope(ScopeKind::Other).unwrap());
        let ab = cx.intern(&[b, a, b]).unwrap();
        assert_eq!(cx.scopes(ab), &[a, b]);
        let a_then_b = cx.add(ScopeSetId::EMPTY, a).unwrap();
        let a_then_b = cx.add(a_then_b, b).unwrap();
        assert_eq!(a_then_b, ab);
        assert_eq!(cx.intern(&[]).unwrap(), ScopeSetId::EMPTY);
        assert_eq!(cx.set_count(), 3);
    }

    #[test]
    fn add_remove_and_flip() {
        let mut cx = ScopeContext::new();
        let [a, b] = [0; 2].map(|_| cx.new_scope(ScopeKind::Macro).unwrap());
        let s = cx.intern(&[a]).unwrap();
        assert_eq!(cx.add(s, a).unwrap(), s);
        assert_eq!(cx.remove(s, b).unwrap(), s);
        let flipped = cx.flip(s, b).unwrap();
        assert_eq!(cx.scopes(flipped), &[a, b]);
        assert_eq!(cx.flip(flipped, b).unwrap(), s);
        assert_eq!(cx.remove(flipped, a).unwrap(), cx.intern(&[b]).unwrap());
        // Memoised: the same operation finds the same set without interning.
        let count = cx.set_count();
        assert_eq!(cx.flip(s, b).unwrap(), flipped);
        assert_eq!(cx.set_count(), count);
    }

    #[test]
    fn subsets() {
        let mut cx = ScopeContext::new();
        let [a, b, c] = [0; 3].map(|_| cx.new_scope(ScopeKind::Binder).unwrap());
        let ac = cx.intern(&[a, c]).unwrap();
        let abc = cx.intern(&[a, b, c]).unwrap();
        let bc = cx.intern(&[b, c]).unwrap();
        assert!(cx.is_subset(ac, abc));
        assert!(cx.is_subset(ScopeSetId::EMPTY, ac));
        assert!(cx.is_subset(ac, ac));
        assert!(!cx.is_subset(abc, ac));
        assert!(!cx.is_subset(ac, bc));
        assert!(cx.contains(ac, c) && !cx.contains(ac, b));
        assert_eq!(cx.kind(b), ScopeKind::Binder);
    }

    #[test]
    fn running_out_is_an_error() {
        let mut cx = ScopeContext::with_limit(3);
        let scopes: Vec<_> = (0..3)
            .map(|_| cx.new_scope(ScopeKind::Other).unwrap())
            .collect();
        let error = cx.new_scope(ScopeKind::Other).unwrap_err();
        assert_eq!(error, TooManyScopes { limit: 3 });
        assert_eq!(
            error.to_string(),
            "too many scopes in one expansion (the limit is 3)"
        );
        // Sets run out too: the empty set, {0} and {1} fill three slots.
        let one = cx.add(ScopeSetId::EMPTY, scopes[0]).unwrap();
        cx.add(ScopeSetId::EMPTY, scopes[1]).unwrap();
        assert!(cx.add(one, scopes[1]).is_err());
        assert_eq!(cx.scope_count(), 3);
    }
}

/// Maps the scopes of one context into another, each to a fresh scope of
/// the same kind the first time it is met, so syntax made in one
/// expansion context can be used in another (ADR 0015 #5): a library's
/// macros in an importer's context, or stored syntax as it loads.
#[derive(Debug, Default)]
pub struct Remap {
    scopes: HashMap<Scope, Scope>,
    sets: HashMap<ScopeSetId, ScopeSetId>,
    /// Where spans' sources are in the target, when they move too: syntax
    /// merged with its sources into another table (`Sources::absorb`).
    sources: HashMap<crate::SourceId, crate::SourceId>,
}

impl Remap {
    /// A remapping that has met no scope yet.
    pub fn new() -> Remap {
        Remap::default()
    }

    /// Maps `from` to `to`, an existing scope, rather than a fresh one: the
    /// scope of the core forms, which every context has.
    pub fn identify(&mut self, from: Scope, to: Scope) {
        self.scopes.insert(from, to);
    }

    /// The scope `scope` of `from` maps to in `to`.
    pub fn scope(
        &mut self,
        from: &ScopeContext,
        to: &mut ScopeContext,
        scope: Scope,
    ) -> Result<Scope, TooManyScopes> {
        if let Some(&mapped) = self.scopes.get(&scope) {
            return Ok(mapped);
        }
        let mapped = to.new_scope(from.kind(scope))?;
        self.scopes.insert(scope, mapped);
        Ok(mapped)
    }

    /// The set `set` of `from` maps to in `to`.
    pub fn set(
        &mut self,
        from: &ScopeContext,
        to: &mut ScopeContext,
        set: ScopeSetId,
    ) -> Result<ScopeSetId, TooManyScopes> {
        if let Some(&mapped) = self.sets.get(&set) {
            return Ok(mapped);
        }
        let scopes = from
            .scopes(set)
            .iter()
            .map(|&s| self.scope(from, to, s))
            .collect::<Result<Vec<_>, _>>()?;
        let mapped = to.intern(&scopes)?;
        self.sets.insert(set, mapped);
        Ok(mapped)
    }

    /// Moves spans to the sources `map` says, as syntax is remapped.
    pub fn move_sources(&mut self, map: HashMap<crate::SourceId, crate::SourceId>) {
        self.sources = map;
    }

    /// `span`, in the sources syntax is remapped into.
    pub fn span(&self, span: crate::Span) -> crate::Span {
        match self.sources.get(&span.source) {
            Some(&source) => crate::Span { source, ..span },
            None => span,
        }
    }

    /// Whether spans move to other sources.
    pub fn moves_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    /// Whether every scope of `set` has been met: only then can an
    /// identifier mapped so far have a set that holds it.
    pub fn has_met_all(&self, from: &ScopeContext, set: ScopeSetId) -> bool {
        from.scopes(set).iter().all(|s| self.scopes.contains_key(s))
    }
}
