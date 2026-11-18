//! Scopes and interned scope sets (ADR 0015).
//!
//! A [`ScopeContext`] numbers scopes and scope sets for one expansion, from
//! zero, so the numbers depend only on what is being expanded. Scope sets
//! are interned: equal sets get the same [`ScopeSetId`], so comparing two
//! sets compares two integers. Adding, removing and flipping a scope are
//! memoised.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use crate::syntax::Effect;

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

/// The fewest scopes in a set that is tested as a bit set.
const BIT_SET_MIN: usize = 64;

/// The most bytes of bit sets a context makes.
const BIT_SET_BUDGET: usize = 256 << 20;

/// A list of changes made to a set: the result, and the list, to keep its
/// address its own.
type Batch = (ScopeSetId, Arc<[(Scope, Effect)]>);

/// Numbers scopes and interns scope sets for one expansion.
///
/// Two caches speed up the work on large sets: bit sets, and the results
/// of applying long lists of changes. They are this context's own and
/// bounded (the bit sets by 256 MiB); a clone starts with empty ones, and
/// [`ScopeContext::clear_caches`] drops them, so a finished context that is
/// kept for its sets does not keep them too.
#[derive(Debug)]
pub struct ScopeContext {
    limit: u32,
    kinds: Vec<ScopeKind>,
    sets: Vec<Box<[Scope]>>,
    /// Each set without its newest scope, if that was interned already.
    parents: Vec<Option<ScopeSetId>>,
    /// Each large set as a bit set, made when a subset test first needs it.
    bits: Vec<OnceLock<Box<[u64]>>>,
    /// How many bytes of `bits` this context has made, against its own
    /// budget.
    bits_bytes: AtomicUsize,
    interned: HashMap<Box<[Scope]>, ScopeSetId>,
    memo: HashMap<(ScopeSetId, Scope, Op), ScopeSetId>,
    /// Long lists of changes, applied whole, by the list's address. The
    /// list is kept, so its address stays its own.
    batches: HashMap<(ScopeSetId, usize), Batch>,
}

impl Clone for ScopeContext {
    /// A copy of the scopes and sets, with empty caches.
    fn clone(&self) -> Self {
        ScopeContext {
            limit: self.limit,
            kinds: self.kinds.clone(),
            sets: self.sets.clone(),
            parents: self.parents.clone(),
            bits: self.sets.iter().map(|_| OnceLock::new()).collect(),
            bits_bytes: AtomicUsize::new(0),
            interned: self.interned.clone(),
            memo: self.memo.clone(),
            batches: HashMap::new(),
        }
    }
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
            parents: vec![None],
            bits: vec![OnceLock::new()],
            bits_bytes: AtomicUsize::new(0),
            interned: HashMap::from([(empty, ScopeSetId::EMPTY)]),
            memo: HashMap::new(),
            batches: HashMap::new(),
        }
    }

    /// Drops the bit sets and the remembered lists of changes, which are
    /// only there to save time. What the context numbers and interns is
    /// untouched.
    pub fn clear_caches(&mut self) {
        self.bits
            .iter_mut()
            .for_each(|cell| *cell = OnceLock::new());
        self.bits_bytes = AtomicUsize::new(0);
        self.batches = HashMap::new();
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

    /// Every scope this context has made, in the order it made them.
    pub fn all_scopes(&self) -> impl Iterator<Item = Scope> {
        (0..self.kinds.len() as u32).map(Scope)
    }

    /// Every set this context has interned, in the order it interned them.
    pub fn all_sets(&self) -> impl Iterator<Item = ScopeSetId> {
        (0..self.sets.len() as u32).map(ScopeSetId)
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
        let (sa, sb) = (self.scopes(a), self.scopes(b));
        if sa.len() > sb.len() {
            return false;
        }
        if sa.len() >= BIT_SET_MIN
            && let (Some(x), Some(y)) = (self.bit_set(a), self.bit_set(b))
        {
            let y = |i: usize| y.get(i).copied().unwrap_or(0);
            return x.iter().enumerate().all(|(i, &w)| w & !y(i) == 0);
        }
        let mut rest = sb.iter();
        sa.iter().all(|x| rest.by_ref().any(|y| y == x))
    }

    /// `set` as a bit set, if it is worth one and the context has room:
    /// testing two large sets for a subset is then a word at a time, not
    /// a scope at a time, which is what keeps resolving a name that
    /// every level of deeply nested code binds from costing the square
    /// of the depth in scopes.
    fn bit_set(&self, set: ScopeSetId) -> Option<&[u64]> {
        let cell = &self.bits[set.0 as usize];
        if let Some(bits) = cell.get() {
            return Some(bits);
        }
        let scopes = self.scopes(set);
        let words = scopes.last().map_or(0, |s| s.0 as usize / 64 + 1);
        // A set is sized by its highest scope, not by how many it has. It
        // is dense when that takes no more words than it has scopes, which
        // is when a word-wise test beats the merge's one comparison per
        // scope (deeply nested code adds scopes one after another, so its
        // sets are). A sparse set, say a few dozen scopes late in a unit
        // with many, would cost more words than comparisons, and more
        // memory too.
        if words > scopes.len() {
            return None;
        }
        let bytes = words * 8;
        if self.bits_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes > BIT_SET_BUDGET {
            self.bits_bytes.fetch_sub(bytes, Ordering::Relaxed);
            return None;
        }
        Some(cell.get_or_init(|| {
            let mut bits = vec![0u64; words];
            for s in scopes {
                bits[s.0 as usize / 64] |= 1 << (s.0 % 64);
            }
            bits.into_boxed_slice()
        }))
    }

    /// `set` without its newest scope, if that set was interned by the
    /// time `set` was. Deeply nested code makes each set just after the
    /// one around it, so there it is always known. Resolution uses it to
    /// build on what it found for the smaller set; it is only a hint, and
    /// a set without one is resolved the long way.
    pub fn parent(&self, set: ScopeSetId) -> Option<ScopeSetId> {
        self.parents[set.0 as usize]
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

    /// `set` with every change in `changes`, sorted by scope, made. Short
    /// lists apply change by change. A long one, which is what a deeply
    /// nested form carries to each of its children, is merged in one pass
    /// and remembered per list, so the children that share it share the
    /// work instead of each paying for every change in turn.
    pub(crate) fn apply_changes(
        &mut self,
        set: ScopeSetId,
        changes: &Arc<[(Scope, Effect)]>,
    ) -> Result<ScopeSetId, TooManyScopes> {
        const LONG: usize = 8;
        if changes.len() < LONG {
            let mut set = set;
            for &(scope, effect) in changes.iter() {
                set = effect.apply(self, set, scope)?;
            }
            return Ok(set);
        }
        let key = (set, Arc::as_ptr(changes) as *const () as usize);
        if let Some((result, _)) = self.batches.get(&key) {
            return Ok(*result);
        }
        let old = self.scopes(set);
        let mut merged = Vec::with_capacity(old.len() + changes.len());
        let (mut i, mut j) = (0, 0);
        while i < old.len() || j < changes.len() {
            match (old.get(i), changes.get(j)) {
                (Some(&a), Some(&(b, effect))) if a == b => {
                    i += 1;
                    j += 1;
                    if effect == Effect::Add {
                        merged.push(a);
                    }
                }
                (Some(&a), Some(&(b, _))) if a < b => {
                    i += 1;
                    merged.push(a);
                }
                (Some(&a), None) => {
                    i += 1;
                    merged.push(a);
                }
                (_, Some(&(b, effect))) => {
                    j += 1;
                    if effect != Effect::Remove {
                        merged.push(b);
                    }
                }
                (None, None) => unreachable!(),
            }
        }
        let result = self.intern_sorted(merged)?;
        self.batches.insert(key, (result, Arc::clone(changes)));
        Ok(result)
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
        let parent = sorted
            .split_last()
            .and_then(|(_, rest)| self.interned.get(rest).copied());
        let sorted = sorted.into_boxed_slice();
        self.parents.push(parent);
        self.sets.push(sorted.clone());
        self.bits.push(OnceLock::new());
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

#[cfg(test)]
mod fast_paths {
    use super::*;
    use std::sync::Arc;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn random_set(cx: &mut ScopeContext, rng: &mut Rng, all: &[Scope], max: u64) -> ScopeSetId {
        let n = rng.below(max + 1) as usize;
        let mut v: Vec<Scope> = (0..n)
            .map(|_| all[rng.below(all.len() as u64) as usize])
            .collect();
        v.sort();
        v.dedup();
        cx.intern(&v).unwrap()
    }

    #[test]
    fn apply_changes_matches_one_at_a_time() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let mut cx = ScopeContext::new();
        let all: Vec<Scope> = (0..400)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        for round in 0..20_000 {
            let set = random_set(&mut cx, &mut rng, &all, 200);
            let len = 1 + rng.below(80) as usize;
            let mut scopes: Vec<Scope> = (0..len)
                .map(|_| all[rng.below(all.len() as u64) as usize])
                .collect();
            scopes.sort();
            scopes.dedup();
            let changes: Vec<(Scope, Effect)> = scopes
                .into_iter()
                .map(|s| {
                    (
                        s,
                        [Effect::Add, Effect::Remove, Effect::Flip][rng.below(3) as usize],
                    )
                })
                .collect();
            let changes: Arc<[(Scope, Effect)]> = changes.into();
            let mut expected = set;
            for &(s, e) in changes.iter() {
                expected = e.apply(&mut cx, expected, s).unwrap();
            }
            assert_eq!(
                cx.apply_changes(set, &changes).unwrap(),
                expected,
                "round {round}"
            );
            // And again, from the memo.
            assert_eq!(
                cx.apply_changes(set, &changes).unwrap(),
                expected,
                "memo, round {round}"
            );
        }
    }

    #[test]
    fn is_subset_matches_naive() {
        let mut rng = Rng(0x2545f4914f6cdd1d);
        let mut cx = ScopeContext::new();
        // Few enough scopes that the sets are dense, so the bit sets are
        // used.
        let all: Vec<Scope> = (0..600)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        let mut subsets = 0;
        let mut by_bits = 0;
        for round in 0..20_000 {
            let b = random_set(&mut cx, &mut rng, &all, 400);
            // Half the time a is drawn from b (often a subset), else at
            // random.
            let a = if rng.below(2) == 0 {
                let sb = cx.scopes(b).to_vec();
                let mut v: Vec<Scope> = sb.into_iter().filter(|_| rng.below(8) != 0).collect();
                if rng.below(4) == 0 {
                    v.push(all[rng.below(all.len() as u64) as usize]);
                }
                v.sort();
                v.dedup();
                cx.intern(&v).unwrap()
            } else {
                random_set(&mut cx, &mut rng, &all, 400)
            };
            let naive = cx.scopes(a).iter().all(|s| cx.scopes(b).contains(s));
            subsets += usize::from(naive);
            if cx.scopes(a).len() >= BIT_SET_MIN
                && cx.bit_set(a).is_some()
                && cx.bit_set(b).is_some()
            {
                by_bits += 1;
            }
            assert_eq!(cx.is_subset(a, b), naive, "round {round}");
        }
        assert!(
            subsets > 1000,
            "{subsets} subsets: the true case is barely exercised"
        );
        assert!(
            by_bits > 1000,
            "{by_bits} tests by bit set: the path is barely exercised"
        );
    }

    #[test]
    fn sparse_sets_get_no_bit_set_and_clones_start_empty() {
        let mut cx = ScopeContext::new();
        let all: Vec<Scope> = (0..10_000)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        let dense = cx.intern(&all[..100]).unwrap();
        let sparse = cx.intern(&all[9_900..9_980]).unwrap();
        assert!(cx.bit_set(dense).is_some());
        assert!(cx.bit_set(sparse).is_none());
        // Sparse sets still compare correctly, by the merge.
        assert!(cx.is_subset(sparse, sparse));
        assert!(!cx.is_subset(sparse, dense));
        let clone = cx.clone();
        assert!(clone.bits[dense.0 as usize].get().is_none());
        cx.clear_caches();
        assert!(cx.bits[dense.0 as usize].get().is_none());
        assert_eq!(cx.scopes(dense), &all[..100]);
        assert!(cx.is_subset(dense, dense));
    }
}
