# 0025. Weak references and ephemerons

- **Status:** Proposed
- **Date:** 2026-11-19
- **Related:** 0001 (determinism), 0002 (value representation), 0003 (memory management), 0004 (continuations), 0007 (language scope), 0008 (threads), 0016 (Rust objects and the finalize hook), 0020 (constants outside the heap), 0023 (resource limits), 0024 (objects, shapes and dispatch)

## Context

A weak reference points at an object without keeping it alive. An **ephemeron** is a weak reference with a value attached: the value stays alive only while the key does, so a value that refers back to its key does not keep the key alive. Neither can be written in Scheme, since both need the memory manager to know about the edge. Five things want them:

- **SRFI 124 and its successor SRFI 254** (#37). SRFI 124 was withdrawn in June 2024 and SRFI 254, which supersedes it, was finalised in June 2026. The two share `make-ephemeron`, `ephemeron?`, `ephemeron-key`, `ephemeron-broken?` and `reference-barrier`. SRFI 254 renames the datum a value, adds `ephemeron-ref`, and adds guardians (see Guardians below). Both let an implementation never break an ephemeron: the SRFI 124 reference implementation `ephemerons-trivial.scm` holds everything strongly and is conforming.
- **SRFI 125's weak and ephemeral tables** (#30). ADR 0007 #13 accepts the `weak-keys`, `weak-values`, `ephemeral-keys` and `ephemeral-values` hints and ignores them, so such a table keeps its keys and values alive.
- **ADR 0024's collectable shapes**, whose parent-to-child transition edges are weak so that an unused branch of the shape tree can go, and which the planned hot module replacement needs so that an old class's shapes are reclaimed once its instances are gone.
- **A weak symbol table** (#46), so that symbols interned at run time are freed and ADR 0023 #7's caps count live entries.
- **Host code**: caches keyed by Scheme objects, and ADR 0016's left-open weak handles.

ADR 0002 #12 already reserves the type codes `WeakPair` and `Ephemeron`, and ADR 0003's contract lists "weak references, ephemerons, guardians and finalisation, with timing unspecified" as required of every strategy, with "under no collection, finalisers never run". Nothing implements them. What exists is the finalize hook: when a counting strategy frees an object, `Cx::finalize_in` cuts it off from the isolate's Rust tables, so a Rust object's value is dropped and a syntax object's slot is emptied. It runs on both ways an object dies, the epoch's free loop and the cycle collector, before the memory is reclaimed. A weak reference is the same kind of table, with a reader.

What makes it hard is that the strategies differ in when "the target dies" happens, and that counting has no marking phase to hang ephemeron processing on:

- **Counting frees targets one at a time,** so a weak reference needs a hook at the moment of the free, and the target must be able to find its weak holders.
- **An ephemeron's value is conditional.** Under counting an ordinary reference from the ephemeron to its value is a counted edge, and when the value refers to the key, the key's count never reaches zero. That is the leak CPython's `WeakKeyDictionary` documents.
- **Trial deletion works on counts** (ADR 0003): every edge the collector subtracts must be one the count includes, and every counted edge must be one the object's description reports.
- **Constants never die** (ADR 0020): they are written to no more once built, and a weak reference to one never clears.

### How other runtimes do it

| System | Weak reference | Ephemeron | Cost to an object that is never weakly referenced |
|---|---|---|---|
| **CPython** | `weakref.ref` objects on a per-object list; the target's deallocation clears them and calls their callbacks. The cycle collector clears weak references to unreachable objects first, and calls a callback only if the weak reference itself is reachable | None. `WeakKeyDictionary` holds values strongly, so a value that refers to its key keeps it alive; the documentation warns of it | A list-head slot (8 bytes) in each instance of a type that opts in. Built-in `int`, `tuple`, `list` and `dict` are not weakly referenceable, so the cost is paid by types, not by use |
| **Swift** | `weak` is a side table allocated lazily when the first weak reference to an object is formed; the object's inline count word then points to it. Loading a weak reference retains the object if its strong count is positive, else yields `nil`. `unowned` keeps the object's memory (not its value) until the unowned count drops | None | Nothing until the first weak reference; the count word's layout changes then |
| **Rust `Rc`/`Weak`** | A second count keeps the allocation, not the value; `upgrade` fails once the strong count is zero | None | One extra count word in every `Rc` |
| **Perl** | `weaken` flags the reference and records it in the target's back-reference list; freeing the target sets each to `undef` | None | A magic slot only on weakly referenced targets |
| **PHP** | A flag on the target, and one process-wide table from target address to its `WeakReference`s and `WeakMap`s, notified when the target is freed | `WeakMap` holds values strongly | One flag bit |
| **Chez Scheme and Racket CS** | Weak pairs (the car is weak); guardians (Dybvig, Bruggeman and Eby) return an object to its owner after the collector finds it unreachable | `ephemeron-cons` (Chez 9.5); in Racket, ephemerons and ephemeron hash tables beside weak ones. A collection marks, then scans the pending ephemerons again whenever a key becomes marked, until none progresses | None: weak and ephemeron objects are scanned specially by type |
| **Guile** (Boehm collector) | Weak pairs, weak-key and weak-value tables, guardians | Weak-key tables over disappearing links, which do not give ephemeron semantics: the manual documents that a value that refers to its key keeps it | None |
| **V8** | `WeakRef` keeps its target alive until the end of the current job; `FinalizationRegistry` callbacks run as later tasks | `WeakMap` is an ephemeron table: the marker keeps a worklist and keeps marking until it finds no more | None |
| **Lua 5.2** | Weak keys and values in tables | Ephemeron tables, with a convergence loop at the end of marking (Hayes' algorithm) | None |

Three lessons carry over. **Where the cost goes:** every system that counts keeps the cost on the weakly referenced targets (a lazily made side table in Swift, a back-reference list in Perl and PHP) or on the types that opt in (CPython), never on all objects. **Ephemerons are a tracing idea:** the only counting runtimes that get true ephemerons are the ones that also trace. Those without a trace leave a value-to-key cycle to leak (CPython, PHP). **A loaded weak reference is a strong one:** V8's keep-alive and SRFI 124's `reference-barrier` exist because the program may hold the target in a register while the collector decides.

## Options

**Meaning under the five strategies.** None of these options changes `NoCollection`.

- **`NoCollection` never frees,** so a weak reference never clears and an ephemeron never breaks. It holds key and value strongly, as `ephemerons-trivial.scm` does, and takes no side table. This is the SRFI's own fallback and is conforming. A program must not depend on a break, and the test suite must say so (see slices).
- **The counting strategies** (`DeferredHeadered`, `DeferredSideCounts`, `PreciseHeadered`, `PreciseSideCounts`) free at an epoch, in a cycle collection, and, once ownership inference lands (ADR 0003, ADR 0017), at a last use. The options below differ in how a weak reference finds out.
- **A tracing strategy** (an MMTk binding, if one is built) does what Chez does: it does not trace a weak slot, runs the ephemeron fixed point after marking, and clears what is left. An ephemeron object is scanned specially by type. Nothing in this record's object layout stands in its way.

**How a counting strategy clears a weak reference:**

1. **A hook at free, and a side table from target to holders (recommended).** A weak object holds its target in a field the description does not report and the count does not include. The isolate keeps a table from a target's address to the weak objects that hold it, and a flag on the target says it has an entry. The free path (`finalize_in`) tests the flag, removes the entry and clears each holder. The cost is the flag and, for each weakly referenced object, a table entry.
2. **A weak count that keeps the memory** (`Rc::Weak`, Swift `unowned`). The target's value is dropped when the strong count reaches zero, but its memory stays until the weak count does. Rejected: this heap hands out two-word pairs from page free lists, an object's memory cannot wait for a stranger, and a count in every object is the cost the strategies are trying to avoid.
3. **No hook: clear at an epoch by scanning every weak object.** An epoch cannot tell whether a target's address now holds a different object, since the page heap reuses memory at once. Rejected.

**How a counting strategy handles an ephemeron's value:**

1. **The key owns the value (recommended).** The ephemeron's value field is not a counted edge. The entry in the target's table record holds the value's one counted reference, and `describe` reports that value as a child of the key. The value is then alive while both the key and the ephemeron are, and a value that refers to its key makes an ordinary cycle of key and value, which trial deletion finds, with the ephemeron outside it. When the ephemeron is freed first, its free hook removes its entry and releases the value. This is how Objective-C's associated objects work, whose values are released when their object is deallocated, and it needs no new collector algorithm.
2. **The ephemeron owns the value, with an ephemeron pass in the cycle collector.** Trial deletion would have to decide whether a key is live before it knows which edges to subtract, which is the fixed point of a marking collector. The counts do not give it. Rejected unless a backup tracer is built for another reason (ADR 0003's sticky counts, for one), when it could be revisited.
3. **The CPython answer: the ephemeron holds its value strongly.** A value that refers to its key leaks it. The weak table the SRFI 125 hints ask for would then be a weak-key table whose entries leak whenever a value mentions its key, which is the common case (a record in a table keyed by that record's identity).

**Where the "has weak holders" flag lives:**

1. **A bit in the object (recommended).** In a headered object, the strategy's half is full: a 28-bit count, two colour bits, the buffered flag and the zero-count flag. The flag takes one bit from the count (27 bits, sticky at 134 million references, which the stuck-count rule already handles). In a side-counted pair the flags byte has four spare bits. Types that have no header under no collection need none, since nothing is freed.
2. **A runtime-data bit in the header.** A closure's data holds a 23-bit free-variable count and a flag, so closures would lose a bit of range, and the bit is the runtime's and not the strategy's. A side-counted pair has no header at all.
3. **A flag on the page, and a table probe for each object freed on a flagged page.** No bit in any object and no layout change, but `describe` runs for every candidate of a collection and would probe for each object on a flagged page, and a few weakly referenced objects flag many pages.

**Guardians and finalisation** (SRFI 254's `make-guardian`, Racket's will executors, Chez's guardians). A guardian gives an object back after the collector finds it unreachable, so the object must be *resurrected*: not freed, with its children still counted. The free path releases an object's children before `finalize` runs (`free_all`), and a collection frees all its white objects together, so resurrection needs a hook before the children are released and a decision on what counts mean for an object that has come back. SRFI 254's guardians take a *representative* to return in place of the object, which does not need resurrection when the representative is a different object, but a guardian may name the object itself. Options: (1) leave guardians out of this record and build ephemerons first; (2) build them with resurrection in the same slices. **Recommended: (1).** Rust objects already have a finaliser, `Drop`, which cannot call Scheme (ADR 0016).

## Recommendation

1. **One weak mechanism for the counting strategies: a side table with a flag, a hook in the free path, key-owned values.** The rest of this section says how.
2. **Objects.** Two types, with the type codes ADR 0002 already reserves. A *weak pair* has a weak `car` and a counted `cdr`; it is what a weak list, a weak box and a table's weak-value entry need. Both carry header data bits: `BROKEN`, so that a broken reference is not confused with a stored `#f`, and `STRONG` (item 8). An *ephemeron* has a key and a value. A SRFI 124 weak reference is `(make-ephemeron k k)`, which item 7 makes cost what a weak pair does. A weak table is a record (ADR 0002 #13) made in Scheme from these.
3. **The weak field is not counted and `describe` does not report it,** so freeing an object and trial deletion both ignore it. Nothing follows a weak field except the runtime's own reads and the clearing at free.
4. **The table.** The isolate keeps `Cx::weak`, a table beside `objects`, `instances` and `syntaxes`, from a target's address to its holders. The first weak reference to an object sets its flag and makes an entry. An entry holds each holder, and for an ephemeron whose value is a heap object other than its key, one counted reference to that value. Making an entry is charged to the heap like a syntax slot (`charge_external`, ADR 0023 #3), so `max_bytes` counts it, and no new cap field is needed: each entry is paired with a heap object that the limit already counts.
5. **Reading.** `ephemeron-key`, `ephemeron-value` and a weak pair's `car` load the field like any heap load, so the value in a register or slot is a counted or stack-scanned reference. A target that has reached zero and still waits in the zero-count table is not freed until an epoch has looked at the stack, so a load resurrects it and an epoch counts the loaded reference in (ADR 0003). **A weak load is safe because a break happens only in an epoch or a cycle collection**, at a safepoint, never in the middle of a Rust procedure.
6. **Breaking.** `finalize_in` tests the freed object's flag. If it is set, it removes the entry, sets each holder's field to `#f` and its header `BROKEN` bit (an ephemeron's value field too). The entry's value needs no release here: the free loop released it as a child of the target, since `describe` reports it. When a holder is freed, its `finalize_in` removes itself from its target's entry before its memory is reclaimed, so the table never holds a freed holder, and releases the value through `S::release` (which queues it at zero), after the collection if one is running, since a cycle collection's counts must not move under it.
7. **Ephemerons and cycles.** The entry's value counts as a child of the key. A value that refers to its key makes a cycle of the two, with the ephemeron outside it, so trial deletion frees both in a collection and the free hook breaks the ephemeron. Because the value is owned by the key, **`describe` must see the table**: it is a plain function of one object today and becomes a function of the object and the isolate's weak table, consulted only when the object's flag is set. An ephemeron whose value is its key, or an immediate, or a constant, owns nothing and makes no cycle: `(make-ephemeron k k)` is a weak pair in cost, and frees `k` at an epoch, not at a collection.
8. **Constants and immediates never clear** (ADR 0020). A fixnum, character, boolean or other immediate is not an object, and a pool object's count is stuck and it is never freed. `make-ephemeron` and `weak-cons` with such a key set `STRONG` and keep the key and value as ordinary counted fields, so the reference never breaks and a collection never writes to a constant. No entry is made, and a frozen page's header is never touched. A heap object whose count has stuck is the same: it is never freed, so no entry.
9. **Continuations** (ADR 0004). A frame copied or sealed copies the reference to the ephemeron, not its key, and a key held in a frame slot is a strong reference like any: the frame values are children of the continuation, so a key a captured continuation holds stays alive for as long as the continuation does. `reference-barrier` is a primitive call whose argument is a use, so the ownership pass (ADR 0003) keeps the key's slot through it; the optimiser must treat it as effectful and not remove it (ADR 0017).
10. **Threads** (ADR 0008). A weak reference and its target are in one isolate's heap. A channel copies values, and an ephemeron or weak pair that is sent is copied as a **strong** pair of key and value, since the receiving isolate has no way to make the sender's target die; the question list asks whether that is right. Shared (cross-isolate) objects are outside this record.
11. **Determinism** (ADR 0001 #4). A break happens only in an epoch or a collection, which are triggered by allocation volume, so the same program breaks the same references at the same points in one strategy. Strategies differ, and that is allowed, since a program may not rely on a break. The expander and the artifact cache must not either: no output of a build may depend on whether a weak reference had broken.
12. **Resource limits** (ADR 0023). The count of weak entries is bounded by heap size and charged to `max_bytes`. Under `NoCollection` no entries are made, so nothing is charged and nothing is freed, and the existing caps are the bound there. When #46 makes symbols weakly interned, `max_symbols` counts live symbols, as ADR 0023 #7 says.
13. **A weak symbol table is a Rust-side table, not ephemerons.** `cx.symbols` and `cx.keywords` are Rust tables from a name to a symbol, like `cx.syntaxes`. Slice 4 gives them the flag and the free hook, so freeing a symbol removes its table entry and returns its charge, as freeing a syntax object does now. A symbol that a pool constant refers to (ADR 0003 says the pool's symbols are never freed while the isolate lives) keeps a stuck count, so the hook never sees it.
14. **Libraries.** SRFI 254 is the primary library (`ephemeron-value`, `ephemeron-ref`), and `(srfi 124)` is a thin library over the same primitives, with `ephemeron-datum`. SRFI 125's four hints build weak, ephemeral and ordinary tables from the same pieces: a table's buckets hold ephemerons, `eq?`-keyed tables use the identity hash (all five strategies hash the address; no strategy moves objects), and broken entries are dropped when a table is next resized or walked. An isolate-wide counter of breaks tells a table when to look. ADR 0007 #13 then drops "accepted and ignored".

### Cost to objects

| Object | Added cost |
|---|---|
| Any object never weakly referenced | One bit: taken from the count in a headered object (27-bit count), from the flags byte in a side-counted pair. Nothing under `NoCollection` |
| A weakly referenced object | One entry in the isolate's table; its flag set |
| A weak pair | Two words and a header (a headered pair); one more than a pair |
| An ephemeron | Header, key, value |
| `describe` | One bit test per object; a table lookup only when it is set |

### Slices

1. **Weak pairs under the counting strategies:** the flag, the table, the hook in `finalize_in` (both free paths), `weak-cons`, `weak-car`, `weak-pair-broken?` and a `collect-garbage` the tests can call, with the five-strategy tests. A scheme test that asserts a break fails under `NoCollection`, so the test header needs a way to say it (the questions list it). This slice also measures the bit's cost on `churn` and `rings` against the page-flag option.
2. **Ephemerons:** key-owned values, `describe` seeing the table, the holder-first ordering, the value-refers-to-key test, the `STRONG` degradations (constant and immediate keys, `(make-ephemeron k k)`), `reference-barrier`, and the libraries (SRFI 254 and 124).
3. **Weak and ephemeral tables** for SRFI 125, and the live-entry count they need.
4. **The weak symbol table** (#46), with ADR 0023 #7's cap change.
5. **ADR 0024's collectable shapes** (its slice f), which use weak pairs for transition edges and the table's flag for the dependents-list clearing.
6. **Guardians**, if wanted, and a tracing strategy's ephemeron processing, whenever either exists.

## Consequences

- Programs may use weak references and ephemerons under every strategy, and may depend on a break under none. Under `NoCollection` they behave as strong.
- Counting keeps one new bit and one table, and pays the table only for objects that have weak holders. The free path gains one test.
- A value that refers to its key is collected as a cycle, so such an ephemeron breaks at a collection and not at an epoch, which can be seconds later. A weak pair, or `(make-ephemeron k k)`, breaks at an epoch.
- `describe` and the collector's `Describe` callback gain a way to read the isolate's table, and the cycle collector's `finalize` callback must now release values that a holder returns, after the collection. These touch the epoch, the cycle collector and `cx.rs`, which several in-flight changes share.
- A moving or concurrent tracing strategy is unaffected: it ignores the table and the flag, scans the two types by their layout and runs its own fixed point.
- ADR 0003's contract row for weak references gains a pointer to this record, and ADR 0007 #13's sentence about ignored hints changes when slice 3 lands. Neither is edited by this proposal.
- `Strategy` needs a constant that says whether it ever frees, so that `make-ephemeron` under `NoCollection` does not build entries.

## Questions to settle before accepting

- **SRFI 124 or 254?** Issue #37 asks for 124, which was withdrawn in favour of 254. Is 254 primary, with 124 as aliases, or is only 254 built? They differ in `ephemeron-datum` against `ephemeron-value`, `ephemeron-ref`, and in what a broken ephemeron returns.
- **Key-owned values or a plainer fallback?** The recommended design makes `describe` depend on a table and gives every ephemeron value a second owner. Is it worth it for the value-refers-to-key case, or should slice 2 hold values strongly (CPython's answer) until a tracer exists?
- **Where does the flag live?** A bit from the count, a runtime-data bit, or a page flag. Slice 1 measures `churn` and `rings` with the bit and with the page flag; who decides on the numbers?
- **Does a break ever happen before an epoch?** After ownership inference, an object can be freed at its last use, so a weak reference could break between two statements. SRFI 254 allows it. Is that visible behaviour worth specifying, say by a guarantee that a break happens only at a call, backward jump or return, as an epoch's does now?
- **How do tests assert a break under five strategies?** `NoCollection` never breaks. A header such as `;;; weak: breaks`, which skips the assertion under `NoCollection` (and anything else that never frees), or a runtime query `(weak-references-clear?)`.
- **Guardians:** in a later record or this one? A guardian needs a hook before children are released, and a rule for what a resurrected object's count is.
- **A sent ephemeron:** copy as a strong pair, as recommended, raise an error, or copy as broken?
- **Does the pool pin symbols?** #46 needs the symbols a module's constants refer to to have stuck counts, or a pool reference that counts.
- **Should `weak-cons` and the weak pair type exist at all,** or only ephemerons, with a weak pair spelled `(make-ephemeron car car)` and the cdr in a second object? Weak pairs are cheaper and ADR 0002 reserves the code; are they worth a second type to test under five strategies?

## References

- [SRFI 124: Ephemerons](https://srfi.schemers.org/srfi-124/srfi-124.html), John Cowan, finalised 2015, withdrawn 2024.
- [SRFI 254: Ephemerons and Guardians](https://srfi.schemers.org/srfi-254/srfi-254.html), finalised 2026.
- [SRFI 125: Intermediate hash tables](https://srfi.schemers.org/srfi-125/srfi-125.html), whose weakness hints this record gives meaning.
- Barry Hayes, *Ephemerons: a New Finalization Mechanism*, OOPSLA 1997.
- R. Kent Dybvig, Carl Bruggeman and David Eby, *Guardians in a Generation-Based Garbage Collector*, PLDI 1993.
- Abdulaziz Ghuloum and R. Kent Dybvig, *Generation-Friendly Eq Hash Tables*, Scheme Workshop 2007.
- Bruno Haible, *Weak References: Data Types and Implementation*, 2005.
- Roberto Ierusalimschy, Luiz Henrique de Figueiredo and Waldemar Celes, the Lua 5.2 reference manual, on ephemeron tables and the convergence step of the collector.
- The Python documentation for [`weakref`](https://docs.python.org/3/library/weakref.html), and PEP 442 for finalisation of objects in cycles.
- The Swift documentation on automatic reference counting, and the runtime's side tables for weak references (`HeapObject` and `RefCount`).
- The Rust standard library's `Rc` and `Weak`.
- The Perl documentation for `Scalar::Util::weaken`; the PHP manual for `WeakReference` and `WeakMap`.
- The Racket reference on weak boxes, ephemerons and wills; the Chez Scheme User's Guide on weak pairs, ephemeron pairs and guardians; the Guile manual on weak hash tables and guardians.
- The ECMAScript specification on `WeakRef` (`KeepDuringJob`), `WeakMap` and `FinalizationRegistry`; V8's blog posts on weak references and on concurrent marking.
- The MMTk documentation on weak reference processing.
- ADR 0003's trial-deletion collector: David F. Bacon and V. T. Rajan, *Concurrent Cycle Collection in Reference Counted Systems*, ECOOP 2001.
