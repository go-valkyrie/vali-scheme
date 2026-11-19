# 0003. Memory management

- **Status:** Proposed
- **Date:** 2026-11-16
- **Related:** 0002 (values), 0004 (frames), 0006 (ABI version), 0008 (threads), 0009 (resource limits), 0012 (distribution format)

## Context

Scheme allocates constantly (pairs, closures, boxed flonums, strings) and makes cycles routinely: mutually recursive closures share an environment, records point back to their parents, and `set-cdr!` can close a loop. Whatever reclaims memory sits on the hot path of every tier.

Reference counting is attractive for an embedded language. Rust can hold Scheme values much as it holds `Rc`s, with no rooting rules, and memory is reclaimed incrementally, with no pause to trace the whole heap. The evaluation measured its cost when done naively. Profiling scheme-rs put count updates at 19–32% of main-thread time: each copy or drop of a heap value was an out-of-line call, a match on the value's tag and a locked atomic instruction, and function entries and exits adjusted the counts of arguments and locals. Those are implementation choices, and under ADR 0008 even atomicity is gone: one thread at a time uses a heap. Modern collectors that use reference counting avoid this:

- **Deferred counting** (Deutsch and Bobrow) counts no references from frames or registers, only heap-to-heap ones. Calls then cost nothing; objects whose count reaches zero are freed at the next scan of the stack.
- **Ownership inference** (Lean 4, Koka's Perceus) counts precisely but lets the compiler place the operations: parameters are owned or borrowed, decrements go at last use, and uniquely held objects are reused in place. Apple's ARC, with borrowed parameters and redundant pairs removed, is the same idea.
- **LXR** (PLDI 2022) combines deferred and coalesced counting with occasional concurrent tracing, and matches tracing collectors on throughput.

No measurement compares these for Scheme. Which strategy is fastest depends on the program: precise counting wins where data is rebuilt (reuse saves allocation), and deferred counting wins where code calls unknown functions or captures continuations. Other hosts may want something else again: no collection at all for short runs, or a tracing collector for throughput or short pauses.

Under ADR 0008, each isolate has its own heap, which one thread at a time uses: the isolate's tasks are concurrent, not parallel. Strategies are therefore single-threaded per heap. Isolates exchange copied messages, and objects explicitly created as shared live outside every isolate heap, under their own rules.

Two earlier decisions help. Frames live in runtime-managed stack records with one layout for every tier (ADR 0004), so the runtime can find every value on the Scheme stack exactly. And generated code is an implementation detail (ADRs 0006 and 0012), so where it places count operations or barriers can differ by strategy without touching any file format.

## Proposed direction: pluggable strategies

Memory management sits behind a **strategy interface**. Vali's object model, embedding API and code generators are written against that interface, and a strategy fills it in.

1. **The runtime is specialised for one strategy at compile time.** Each strategy is a Rust type behind a trait, and the runtime and VM take it as a type parameter (`Runtime<S: Strategy>`, with a default), so every hook is inlined, and a hook a strategy leaves empty compiles to nothing. Choosing at run time would add an indirect call to every store and allocation.
   - **A type parameter, not a Cargo feature.** Cargo merges features across the whole dependency graph, so two crates asking for different strategies would collide silently. A type parameter also lets a host that genuinely needs two strategies instantiate both.
   - **The code generator takes the strategy as a target, like a CPU.** The `vali` tool can therefore generate code for any strategy, whatever its own runtime uses. `vali build -mcms` works from a `vali` built with reference counting (ADR 0013).
2. **One strategy per runtime instance.** Run-format bytecode and native code depend on the strategy, so the strategy is part of the ABI version (ADR 0006) and of the cache key. Code is not shared between instances with different strategies. Distribution artifacts (ADR 0012) work under every strategy.
3. **Strategies never change what a program means.** When objects die, and whether memory is reused in place, are invisible to Scheme code. The test suite runs under every strategy that is kept.

### The contract

Designed for the most demanding strategy under evaluation, which is concurrent compaction, so that adding a strategy never means auditing the runtime:

| Contract item | Needed by | Cost to strategies that don't need it |
|---|---|---|
| **Raw `Value`s in Rust stay valid until control returns to the VM:** no strategy moves objects, and frees wait for an epoch, which runs only at calls, backward jumps and host returns. Anything kept longer is a handle. Raw values are for the runtime's own code; reading the heap through one is `unsafe`. No moving collector is planned. Adding one would first brand raw values with a lifetime tied to the context, so that the compiler rejects one held across an allocation: a change to every primitive | Every strategy built | None |
| **A handle is a Rust object with Rust ownership: `Send`, `Sync`, `Clone` and `'static`.** It points to a *root slot* in its isolate's root table. The slot has its own atomic count, changed only when Rust clones or drops handles; the object has one ordinary Scheme reference, from the table, which the isolate adds and removes. Using a handle takes the isolate's context (`&mut`), which proves the isolate is not running Scheme code on another thread. Dropping a handle takes no context: when the slot's count reaches zero, the slot goes onto the isolate's lock-free release queue, which the isolate empties at its next allocation or safepoint. Any task of the isolate may use the handle. PyO3's `Py<T>`, `Python<'py>` token and reference pool work the same way | Counting strategies, whose counts are not atomic; moving collectors, which treat the root table as roots | Atomic updates only on handle clones and drops, at the rate Rust crosses the boundary; never on Scheme's own counts. A dropped handle's object lives until the isolate's next safepoint |
| **A handle used with another isolate is an error,** naming the remedy: send the value through a channel, which copies it (ADR 0008) | All: another isolate's heap may be in use on another thread | None |
| **Host code mostly reaches Scheme through the isolate,** not through handles: `isolate.call(&procedure, args).await` enters the isolate, runs Scheme until it finishes, lets the isolate's other tasks run while it awaits host futures, and converts the result to Rust; `isolate.with(|cx| …)` gives a closure the context to inspect values | All | None |
| **A pinning API** for Rust code that needs a stable address, such as a bytevector passed to I/O | Moving collectors | Small |
| **Bridged Rust objects hold handles, not Scheme values the collector must trace** | Concurrent collectors, whose tracing would otherwise run Rust code concurrently with its owner | None |
| **The upper 32 bits of every header word belong to the strategy** (ADR 0002 #7): a count, mark bits, age, or the identity hash's state. A moving strategy may replace the whole header with a forwarding pointer | All | None |
| **The strategy chooses whether pairs and flonums carry a header** (ADR 0002 #8), and may keep metadata for them in tables on their pages, such as count bytes | Counting strategies, which need a count for every pair | None: tracing strategies leave pairs at two words |
| **Constants are outside the strategy** (ADR 0020). A module's constants live on frozen pages of the isolate's pool, built when the module loads, and the strategy supplies the pages and a hook that freezes each object (`Allocate::pool_pair_page`, `pool_page`, `pool_large`; `References::freeze`). A pool object is never counted, moved, freed or a root. Under counting its count is at its maximum, **in a header as in a side table: counting up stops there and counting down leaves it, so the first reference a heap object takes to a constant does not carry into the colour bits, and a constant is never a cycle candidate.** **An object with a stuck count is a leaf in trial deletion's `mark_gray`, `scan_black` and `collect_white`: no decrement or increment, no colour, no push.** That is sound because an object that is never freed keeps alive everything it reaches, so its references to its children are never subtracted and the children keep the counts that include it; it changes nothing for a heap object whose count stuck, which trial deletion could never have freed; and it means a collection never writes to a constant, nor walks its graph. A tracing strategy needs no remembered set for a constant, since a pool object points to no heap object (a write into it is an error; the one exception is a symbol or keyword, which are never freed while the isolate lives), and its roots need not include pool pages | Every strategy | A pair constant's page and each pool object's header are the strategy's to lay out as for a heap object. A compare and a branch on a headered count update, at most about 2% on code that does little else |
| **Every space that can hold a pair has a header word at the start of each 32 KiB-aligned region, with no object starting there** (ADR 0020). The write check of a pair loads the first word of its page and refuses to write if it is not zero. This holds for the page heap and for no collection, whose pair pages gain the word. It does not hold for an MMTk Immix space, whose block metadata is on the side and whose objects start at the block's start: a tracing strategy under MMTk replaces the page test with a test of whether the address is in the VM space that holds the constants | Every strategy that holds pairs | One word a page |
| **The runtime describes objects to the strategy:** size, where its pointers are, how to copy it | Tracing and cycle collection | Moderate: pairs without a header need a forwarding scheme, such as Chez's marker in the `car` |
| **A store hook on every write of a pointer into the heap, given the old and new values** | Counting (increment and decrement), generational and concurrent tracing (write barriers) | None when empty |
| **A load hook on every read of a pointer from the heap** | Concurrent compaction (a Shenandoah-style load barrier) | None when empty |
| **A return barrier:** the runtime can redirect a frame's return address to a stub the strategy supplies | Concurrent collectors that fix stack roots incrementally, as HotSpot's stack watermarks do. Frames are native and describe themselves (ADR 0004), so their return addresses can be redirected | None until used |
| **Exact stack maps and safepoints** for every task of the isolate | Every strategy except no collection. Only the running task needs to reach a safepoint; the others are suspended, at an `.await` or a task switch, with their frames already described | Required anyway |
| **Heap words accessed through accessors, as relaxed atomics, in Rust code** | Concurrent collectors, whose threads read a heap while its isolate runs | None at run time: relaxed loads and stores are plain moves on x86-64 and AArch64. It constrains how the runtime is written |
| **Identity hashes come from one hook, `References::identity_hash`,** which `eq?` and `eqv?` hash tables (through `eq-hash` and `eqv-hash`) use for every heap object. No strategy moves objects (the first row of this table), so the default derives the hash from the address, and all five strategies use it. **A moving strategy must override it with a hash stored in the object, not derived from its address:** a lazily added hash field, or a side table where pairs and flonums have no header word. Nothing else may hash an address, so that is the only place that changes | Moving collectors (`eq?` hash tables) | None today; a lazily added hash field under a moving collector |
| **Forwarding installed with an atomic compare-and-swap** | Concurrent compaction, where the mutator and collector race to move an object | None |
| **The allocator belongs to the strategy,** with allocation buffers per isolate, so allocation never takes a lock: bump pointers, free lists, or allocation marked live during concurrent marking | All | None |
| **Weak references, ephemerons, guardians and finalisation, with timing unspecified** | All. Under no collection, finalisers never run | Programs must close ports and Rust resources explicitly |

Shared by several strategies, not part of the contract: **cycle collection** for the counting strategies, by trial deletion or a backup tracer. Each isolate collects its own cycles. Also shared: **escape analysis**, which keeps objects that never leave their function out of the heap under every strategy.

## Options under evaluation

| Strategy | How it works | Strengths | Weaknesses |
|---|---|---|---|
| **No collection** (like the JVM's Epsilon) | Bump allocation, nothing freed; the memory limit (ADR 0009) ends the run | Short CLI runs; the baseline that measures every other strategy's cost; about a day's work | Useless for long-running hosts |
| **Precise counting with ownership inference** (Perceus, Lean 4, ARC) | Owned parameters by default, borrowing inferred for known callees, decrements at last use, drop-reuse, uncounted immediates and immortal objects | Memory freed at last use; uniquely held data reused in place; prompt release of Rust resources | Counts remain for heap stores, projections from live objects, calls to unknown functions, and continuation capture; the compiler needs the inference passes |
| **Deferred counting** (Deutsch–Bobrow, coalesced as in LXR) | Only heap-to-heap references counted; objects at zero freed at the next stack scan (epoch), triggered by allocation and on every return to the host | Calls, returns and register moves cost nothing; continuations cost nothing; no inference needed | Freeing waits for an epoch, which scans the stacks of every task of the isolate; uniqueness can never be proven, so no in-place reuse |
| **Moving tracing** (an MMTk binding: Immix, generational Immix, LXR) | Mark-region or copying collection, generational | High throughput; mature implementations | Pauses, unless concurrent; an MMTk binding is substantial work |
| **Concurrent mark-sweep** | Short pauses to start and finish marking; marking and sweeping run on a collector thread | Short pauses without moving objects | Never compacts, so the heap fragments (HotSpot's CMS was removed in JDK 14 for this reason); memory limits need headroom for garbage made during marking |
| **Concurrent compaction** (Shenandoah-style) | A load barrier follows forwarding pointers while objects move concurrently | Short pauses and no fragmentation | The costliest barriers; the most complex strategy |

Rejected outright: ZGC-style coloured pointers, which put metadata in a pointer's high bits and clash with hardware pointer tagging (ADR 0002), and collectors based on page protection, which need process-wide signal handlers an embedded library cannot own.

## Plan for deciding

1. **Milestone M1:** no collection, plus both counting strategies, measured against that baseline on the benchmark set. The faster counting strategy becomes the default. Each counting strategy is measured with both pair layouts: a header word per pair, and count bytes on pair pages whose counts stick at 255. The faster layout is kept. Trial deletion cannot recover a sticky count, which never returns to zero, so under M1 such a pair is never freed; a backup trace would recover them, and whether the side layout needs one depends on how many pairs stick.
2. **Later, when a host needs it:** an MMTk binding for tracing, and either concurrent strategy.

### How M1 builds them

- **Both counting strategies are built in M1, before the optimising compiler.** Precise counting starts without ownership inference: every reference from a frame slot is counted too, which is its worst case. The inference passes (owned and borrowed parameters, decrements at last use, drop-reuse) come with the optimising compiler (ADR 0017), and precise counting is measured again then.
  - **Precise counting counts every slot store,** the new value in and the old one out, and counts a frame's slots out and clears them as the frame unwinds, tail-calls into a smaller frame or moves to a new stack segment. The words of a callee's header are cleared the same way before it is written over its caller's slots. So every stack word holds a counted value or a non-pointer, and stack segments start zeroed. A waiting `%catch` holds its handler as a reference of its own.
  - **The code moves and drops references itself, so a frame returns without walking its slots.** As an isolate that counts precisely loads a module, it rewrites each function's run-format code (`vali_bytecode::ownership`, `docs/bytecode.md`). A `mov` whose source dies becomes a `move`, which hands the reference over uncounted. A forward analysis finds the slots that may still hold a reference where a call, tail call or return needs them clear, and a `drop` goes right before. A dead value that something writes over first needs none, since the store counts it out, and neither do results that are never references. The VM then counts out only a returning frame's closure, moves the returned value without counting it, and moves a tail call's procedure and arguments down, counting out what they replace. Run-format code stays independent of strategy, and modules may not contain `move` or `drop`, so only the runtime places one. The runtime places them, not conversion (ADR 0017 #6), because conversion runs in the compiler before any strategy is known, and hand-built bytecode needs the operations too. Against the same build without them, the pass makes `fib` and `tak` 10–12% faster under both layouts and side-counted `sum` 9% faster, and leaves the rest within noise. Ownership inference would remove most of what is left.
  - **Under M1, precise counting frees at epochs too.** An object whose count reaches zero waits in the zero-count table until the next epoch, which needs no stack scan. A Rust procedure can then hold a value whose count drops while it runs, so no primitive has to order its stores. Freeing is no more prompt than under deferred counting, but M1 measures what counting costs, and prompt reuse needs inference anyway.
  - **The aim, once the optimising compiler's flow analysis exists (ADR 0017), is deterministic deallocation:** free each object at its last use, and count only where the analysis cannot prove ownership, instead of counting every store.
  - **Its cycle candidates are the objects whose count drops but not to zero.** A new object that drops while it waits in the zero-count table is marked instead, and buffered as it leaves still referred to.
- **A deferred-counting epoch runs at a safepoint once a set volume has been allocated since the last, and whenever Scheme returns to the host.** Safepoints are calls, returns and a loop's backward jumps. Triggering by allocation volume keeps expansion deterministic (ADR 0001 #4), and returning to the host bounds how long garbage waits after a call. An epoch scans the stacks of every task of the isolate, frees what has a zero count and no reference from a stack, and decrements what those objects referred to.
- **Cycles are collected by trial deletion** (Bacon and Rajan, synchronous): an object whose count drops without reaching zero becomes a candidate root, and at an epoch the collector trial-deletes the candidates' internal references to find garbage cycles. It touches only the candidates' subgraphs, never the whole heap.
  - **Under deferred counting, candidates also come from the stack.** A stack reference is never counted down, so an object can become cyclic garbage without a decrement. Two more cases make candidates: an object that leaves the zero-count table still referred to, such as a new pair closed into a loop by `set-cdr!` and then dropped; and an object whose count drops when the epoch counts the stack's references out again. An object still waiting in the zero-count table is not buffered, because it is looked at as it leaves.
  - **Under deferred counting, an epoch clears the words of live frames that no frame scans.** A callee leaves its values in its caller's slots when it returns, uncounted, and a caller is scanned whole when it is the innermost frame. An epoch that ran in a later, smaller callee did not reach those words, so it freed what they referred to, and the next epoch scanning the caller retained a freed object, which corrupted the heap's free list. The epoch clears them as it scans. A new frame clears its own locals as it is entered, so words outside every frame need nothing.
  - **The zero-count table is processed newest first.** A new structure's parents are newer than their children, so each child reaches zero before its turn instead of leaving the table referred to and becoming a candidate. Oldest first runs the `churn` benchmark about 10% faster without cycle collection, but buffers each child of a new structure before its parent frees it.
  - **An object with a stuck count is a leaf** (the contract row for constants, ADR 0020): `mark_gray`, `scan_black` and `collect_white` skip a child whose count is stuck, so a collection that reaches a constant colours nothing in it and walks none of its graph.
  - **Only types that can form cycles are candidates:** pairs, closures, vectors, records, record types and boxes. Strings, symbols, numbers and bytevectors refer only to bytes.
  - **A collection runs inside an epoch, while the stack's references are counted in,** so every count is exact. It runs once the isolate has allocated as much as was live after the last collection, and at least 4 MiB, which keeps its cost proportional to allocation. A host can force one (`collect_garbage`). Between collections, an object that counting frees while buffered keeps its memory until its candidate entry is dropped; each epoch sweeps the entries made since the last, so that garbage waits one epoch, not a collection.
  - **Colours and the buffered flag live beside the count:** in the strategy's half of the header, or in a side-counted pair's flags byte.
- **The allocator keeps a free list per page,** as mimalloc does. Pages are aligned, so an object's page header is its address masked. Each page holds one size class, and allocation drains the current page of a class before taking the next page with free blocks. Freed objects go back on their own page's list, and a page whose objects are all free can take another size class. The side-count pair layout keeps its count bytes in the pair page's header, which this layout already has. The header's first word, `frozen`, is a field of its own, apart from the page's space and class, which `free` indexes: it is nonzero only on the frozen pair pages of a pool, which are in no class, list or count.
- **Objects describe their pointers to the strategy** by type: which fields hold values. Freeing an object, cycle collection and, later, tracing all walk the same description.

### First measurements

On the baseline compiler and VM, best of five runs, in milliseconds; the heap is what the benchmark process holds at its end, in MiB. `fib` and `tak` only call; `churn` allocates short-lived structures; `rings` makes a million garbage cycles; `sum` walks a million-element list; `build` makes one.

| Strategy | fib | tak | churn | rings | sum | build | Heap |
|---|---|---|---|---|---|---|---|
| None | 83 | 94 | 113 | 76 | 30 | 38 | 793 |
| Deferred, headered | 82 | 94 | 189 | 213 | 28 | 66 | 69 |
| Deferred, side counts | 89 | 95 | 215 | 283 | 28 | 82 | 51 |
| Precise, headered | 142 | 139 | 208 | 231 | 44 | 78 | 69 |
| Precise, side counts | 146 | 148 | 263 | 322 | 67 | 109 | 51 |

Deferred counting costs nothing on code that only calls, and is faster than naive precise counting on every benchmark. Side counts take about a quarter less heap, but are slower under both strategies. Absolute times vary by about half between machines; the comparisons within one run hold.

## Questions to settle before accepting

- Is the contract worth committing to as a whole now, including the load hook and the relaxed-atomics rule, which matter only to concurrent collectors?
- Which strategies are kept and maintained long-term, since each one kept must pass the whole test suite.
- How often a deferred-counting epoch and a cycle collection run, and what triggers them. M1 starts with an epoch per MiB allocated and a collection whenever allocation since the last matches what was live after it.
- How objects explicitly created as shared (ADR 0008) are allocated and reclaimed: outside every isolate heap, probably with atomic counts of their own.
- For an MMTk binding: one MMTk instance for all isolates or one per isolate, and how its collector threads fit into the host's async runtime.

## References

- L. Peter Deutsch and Daniel G. Bobrow, *An Efficient, Incremental, Automatic Garbage Collector*, Communications of the ACM, 1976.
- Yossi Levanoni and Erez Petrank, *An On-the-Fly Reference-Counting Garbage Collector for Java*, OOPSLA 2001.
- David F. Bacon and V. T. Rajan, *Concurrent Cycle Collection in Reference Counted Systems*, ECOOP 2001.
- Alex Reinking, Ningning Xie, Leonardo de Moura and Daan Leijen, *Perceus: Garbage Free Reference Counting with Reuse*, PLDI 2021.
- Sebastian Ullrich and Leonardo de Moura, *Counting Immutable Beans: Reference Counting Optimized for Purely Functional Programming*, IFL 2019.
- Rifat Shahriyar, Stephen M. Blackburn, Xi Yang and Kathryn S. McKinley, *Taking Off the Gloves with Reference Counting Immix*, OOPSLA 2013.
- Wenyu Zhao, Stephen M. Blackburn and Kathryn S. McKinley, *Low-Latency, High-Throughput Garbage Collection*, PLDI 2022 (LXR).
- Stephen M. Blackburn and Kathryn S. McKinley, *Immix: A Mark-Region Garbage Collector with Space Efficiency, Fast Collection, and Mutator Performance*, PLDI 2008.
- Christine H. Flood, Roman Kennke, Andrew Dinn, Andrew Haley and Roland Westrelin, *Shenandoah: An Open-Source Concurrent Compacting Garbage Collector for OpenJDK*, PPPJ 2016.
- JEP 363, *Remove the Concurrent Mark Sweep (CMS) Garbage Collector*.
- The Swift and Clang documentation on automatic reference counting; the MMTk project.
- The PyO3 user guide: `Py<T>`, the `Python<'py>` token, and dropping references without the GIL.
- Wasmtime's API for garbage-collected references: roots kept in the store, used through `&mut Store`.
