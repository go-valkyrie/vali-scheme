# 0003. Memory management

- **Status:** Proposed
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, reworked around pluggable strategies; records the options under evaluation
- **Amended:** 2026-11-16, the strategy is a type parameter of the runtime and a target of the code generator
- **Amended:** 2026-11-16, objects shared between tasks: the share operation, atomic stores, handle kinds, concurrent cycle collection (semantics in ADR 0008)
- **Amended:** 2026-11-16, the frame-return hook becomes a return barrier, now that compiled frames are native (ADR 0004)
- **Related:** 0002 (values), 0004 (frames), 0006 (ABI version), 0008 (threads), 0009 (resource limits), 0012 (distribution format)

## Context

Scheme allocates constantly (pairs, closures, boxed flonums, strings) and makes cycles routinely: mutually recursive closures share an environment, records point back to their parents, and `set-cdr!` can close a loop. Whatever reclaims memory sits on the hot path of every tier.

Reference counting is attractive for an embedded language. Rust can hold Scheme values much as it holds `Rc`s, with no rooting rules, and memory is reclaimed incrementally, with no pause to trace the whole heap. The evaluation measured its cost when done naively. Profiling scheme-rs put count updates at 19–32% of main-thread time: each copy or drop of a heap value was an out-of-line call, a match on the value's tag and a locked atomic instruction, and function entries and exits adjusted the counts of arguments and locals. Those are implementation choices. Only atomicity is inherent, and only for objects that more than one task can reach. Modern collectors that use reference counting avoid this:

- **Deferred counting** (Deutsch and Bobrow) counts no references from frames or registers, only heap-to-heap ones. Calls then cost nothing; objects whose count reaches zero are freed at the next scan of the stack.
- **Ownership inference** (Lean 4, Koka's Perceus) counts precisely but lets the compiler place the operations: parameters are owned or borrowed, decrements go at last use, and uniquely held objects are reused in place. Apple's ARC, with borrowed parameters and redundant pairs removed, is the same idea.
- **LXR** (PLDI 2022) combines deferred and coalesced counting with occasional concurrent tracing, and matches tracing collectors on throughput.

No measurement compares these for Scheme. Which strategy is fastest depends on the program: precise counting wins where data is rebuilt (reuse saves allocation), and deferred counting wins where code calls unknown functions or captures continuations. Other hosts may want something else again: no collection at all for short runs, or a tracing collector for throughput or short pauses.

Under ADR 0008, an isolate's tasks share its heap and can run in parallel, but every object starts local to the task that allocated it and becomes *shared* only when another task can reach it. ADR 0008 says where that happens; this ADR says what it costs under each strategy. Lean 4 and Koka use the same split: non-atomic counts for local objects, atomic counts for shared ones.

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
| **Raw `Value`s in Rust are valid only until the next allocation;** anything kept longer is a handle, `Clone` and `Drop` like an `Rc` | Moving collectors | None at run time. The API never promises more, even where a strategy would allow it |
| **Two kinds of handle:** a *local* handle (`!Send`), which never shares its object, and a *shared* handle (`Send`), which shares its object when created | Counting strategies, which must know before another thread can touch a count | None: Rust code that never crosses tasks uses local handles and pays nothing |
| **A pinning API** for Rust code that needs a stable address, such as a bytevector passed to I/O | Moving collectors | Small |
| **Bridged Rust objects hold handles, not Scheme values the collector must trace** | Concurrent collectors, whose tracing would otherwise run Rust code concurrently with its owner | None |
| **A header word owned by the strategy:** a count, mark bits or a forwarding pointer, plus the *shared* mark (Lean 4 keeps it in the sign of the count) | All | None |
| **A share operation,** which the runtime calls when an object becomes reachable from a second task (ADR 0008). It marks the object and everything reachable from it | Counting strategies: shared objects take atomic count updates from then on | None: tracing strategies leave it empty |
| **The runtime describes objects to the strategy:** size, where its pointers are, how to copy it | Tracing and cycle collection | Moderate: headerless pairs (ADR 0002) need a forwarding scheme, such as Chez's marker in the `car` |
| **A store hook on every write of a pointer into the heap, given the old and new values** | Counting (increment and decrement), generational and concurrent tracing (write barriers), and sharing: a value stored into a shared object is shared | None when empty |
| **A pointer store into a shared object is an atomic exchange** | Counting strategies, so that two racing writers cannot both decrement the old value | Only on shared objects; stores into local objects stay plain |
| **A load hook on every read of a pointer from the heap** | Concurrent compaction (a Shenandoah-style load barrier) | None when empty |
| **A return barrier:** the runtime can redirect a frame's return address to a stub the strategy supplies | Concurrent collectors that fix stack roots incrementally, as HotSpot's stack watermarks do. Frames are native and describe themselves (ADR 0004), so their return addresses can be redirected | None until used |
| **Exact stack maps and safepoints,** reachable on every thread running one of the isolate's tasks | Every strategy except no collection. Tracing strategies stop all of an isolate's running tasks; a task suspended at an `.await` is already stopped | Required anyway |
| **Heap words accessed through accessors, as relaxed atomics, in Rust code** | Parallel tasks sharing objects (ADR 0008), and concurrent collectors, whose threads read a heap while its tasks run | None at run time: relaxed loads and stores are plain moves on x86-64 and AArch64. It constrains how the runtime is written |
| **Identity hashes stored in the object, not derived from its address** | Moving collectors (`eq?` hash tables) | A lazily added hash field |
| **Forwarding installed with an atomic compare-and-swap** | Concurrent compaction, where the mutator and collector race to move an object | None |
| **The allocator belongs to the strategy,** with an allocation buffer per running task so allocation never takes a lock: bump pointers, free lists, or allocation marked live during concurrent marking | All | None |
| **Weak references, ephemerons, guardians and finalisation, with timing unspecified** | All. Under no collection, finalisers never run | Programs must close ports and Rust resources explicitly |

Shared by several strategies, not part of the contract: **cycle collection** for the counting strategies, by trial deletion or a backup tracer. Cycles among local objects are collected by their task; cycles among shared objects need a concurrent cycle collector, which is the case Bacon and Rajan designed theirs for. Also shared: **escape analysis**, which keeps objects that never leave their function out of the heap under every strategy.

## Options under evaluation

| Strategy | How it works | Strengths | Weaknesses |
|---|---|---|---|
| **No collection** (like the JVM's Epsilon) | Bump allocation, nothing freed; the memory limit (ADR 0009) ends the run | Short CLI runs; the baseline that measures every other strategy's cost; about a day's work | Useless for long-running hosts |
| **Precise counting with ownership inference** (Perceus, Lean 4, ARC) | Owned parameters by default, borrowing inferred for known callees, decrements at last use, drop-reuse, uncounted immediates and immortal objects | Memory freed at last use; uniquely held data reused in place; prompt release of Rust resources | Counts remain for heap stores, projections from live objects, calls to unknown functions, and continuation capture; a branch on the shared mark per count update; the compiler needs the inference passes |
| **Deferred counting** (Deutsch–Bobrow, coalesced as in LXR) | Only heap-to-heap references counted; objects at zero freed at the next stack scan (epoch), triggered by allocation and on every return to the host | Calls, returns and register moves cost nothing; continuations cost nothing; no inference needed | Freeing waits for an epoch, and an epoch must scan every task that can reach a shared object; uniqueness can never be proven, so no in-place reuse |
| **Moving tracing** (an MMTk binding: Immix, generational Immix, LXR) | Mark-region or copying collection, generational | High throughput; mature implementations | Pauses, unless concurrent; an MMTk binding is substantial work |
| **Concurrent mark-sweep** | Short pauses to start and finish marking; marking and sweeping run on a collector thread | Short pauses without moving objects | Never compacts, so the heap fragments (HotSpot's CMS was removed in JDK 14 for this reason); memory limits need headroom for garbage made during marking |
| **Concurrent compaction** (Shenandoah-style) | A load barrier follows forwarding pointers while objects move concurrently | Short pauses and no fragmentation | The costliest barriers; the most complex strategy |

Rejected outright: ZGC-style coloured pointers, which put metadata in a pointer's high bits and clash with hardware pointer tagging (ADR 0002), and collectors based on page protection, which need process-wide signal handlers an embedded library cannot own.

## Plan for deciding

1. **Milestone M1:** no collection, plus both counting strategies, measured against that baseline on the benchmark set. The faster counting strategy becomes the default.
2. **Later, when a host needs it:** an MMTk binding for tracing, and either concurrent strategy.

## Questions to settle before accepting

- Is the contract worth committing to as a whole now, including the load hook and the relaxed-atomics rule, which matter only to concurrent strategies?
- Which strategies are kept and maintained long-term, since each one kept must pass the whole test suite.
- How often a deferred-counting epoch runs, and what triggers it.
- Whether sharing marks a whole structure at once, or marks lazily: only the object handed over, with each child marked when first loaded through a shared object. Lazy marking avoids walking structures whose parts are never touched by the other task, at the cost of a check on loads from shared objects.
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
