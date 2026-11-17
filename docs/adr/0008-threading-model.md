# 0008. Threading model

- **Status:** Proposed
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, reworked: an isolate's tasks share its heap and run in parallel; objects are local to a task until shared
- **Amended:** 2026-11-17, a Rust handle no longer shares its object by existing; using it from another task does (ADR 0003)
- **Related:** 0002 (values), 0003 (what sharing costs), 0004 (continuations, async), 0006 (shared code), 0009 (sandboxing, limits)

## Context

Whether Scheme values can cross threads affects every runtime type. It decides whether counts and fields need atomic access, whether pairs, vectors and variables need locks, and whether the collector must stop several threads at once.

Hosts are async Rust programs, typically on Tokio's multi-threaded runtime. `tokio::spawn` requires a `Send` future, and the runtime moves tasks between worker threads at any `.await` (work stealing). A task is polled by only one thread at a time. Hosts want two kinds of parallelism:

- **Many independent evaluations,** each its own task: a server handling requests, a build tool evaluating packages.
- **One program spreading work across cores** while sharing data, with Scheme tasks running as host tasks.

Profiling scheme-rs, which lets every value cross threads, showed locks on pairs and global variables taking 17–41% of main-thread time, out-of-line atomic count updates 19–32%, and a global allocation lock 27–33% of allocation-heavy code. All but one of these are implementation choices: word-sized fields need no locks, never-assigned bindings are constants (ADR 0005), and allocation buffers per thread need no lock. The one inherent cost is atomic counting, and only for objects that more than one task can actually reach.

Two known ways to limit atomic counting:

- **Biased reference counting** (Choi, Shull and Torrellas; Python's free-threaded build) lets an object's owner thread count without atomics. It assumes objects stay with their thread, which work stealing breaks: after one migration, everything a task allocated counts as foreign.
- **Marking shared objects** (Lean 4, Koka) tracks only *whether* an object can be reached from more than one task. Local objects count without atomics; shared ones atomically. Task migration doesn't matter, because "local" means one task, not one thread.

## Options

- **A. Isolates only.** Values never leave their isolate, and an isolate runs one task at a time. No atomics anywhere, but one program cannot use two cores on shared data.
- **B. One heap, every value shared** (scheme-rs). Every count update is atomic.
- **C. One heap, biased counting.** Cheap until tasks migrate, which under Tokio is constantly.
- **D. One heap per isolate, shared by its tasks; objects local to a task until shared** (Lean 4, Koka).

## Recommendation

**D.**

1. **An isolate is the sandbox:** a heap, its library instances (one per phase, ADR 0001), its own table of symbols interned at run time, and its capabilities, import policy and resource limits (ADR 0009). Values never cross isolates; isolates exchange copies. Evaluations that must not affect each other run in separate isolates.
2. **An isolate runs any number of tasks, in parallel.** Each Scheme task is a host async task, a `Send` future that the host's runtime may place on any worker. Tasks of one isolate share its heap.
3. **Every object is local to the task that allocated it, until it is shared.** An object becomes shared when another task can reach it:
   - it is passed to a spawned task, or captured by the closure a task runs;
   - it is sent through a cross-task primitive, such as a channel;
   - it is stored into an object that is already shared, including library variables, which every task of the isolate can see;
   - Rust code hands a handle to it to another task, and the owner marks it shared so that task can use it (ADR 0003). A Rust handle alone shares nothing: handles are `Send` and survive `.await`, but only their owning task can use them until then.

   Sharing is transitive: everything reachable from a shared object is shared. What sharing costs depends on the memory-management strategy (ADR 0003). Under counting strategies, shared objects take atomic count updates; under tracing strategies, sharing costs nothing.
4. **Races never break memory safety.** Word-sized fields of pairs, vectors, records and variables are read and written with plain (relaxed atomic) loads and stores, never under a lock. Unsynchronised racing writes from Scheme leave one of the written values, unspecified which, as in Java. Programs that need ordering use synchronisation primitives.
5. **Shared between isolates, read-only and thread-safe:** loaded artifacts and bytecode, JIT-compiled code (ADR 0006), and the global table of symbols that appear in compiled code. Symbols interned at run time go into the isolate's own table, which its collector can free and its memory limit counts; `string->symbol` looks in the global table first. Equal symbols are `eq?` within an isolate.
6. **Async bridges return `Send` futures,** since tasks are `Send`. A variant for `!Send` futures, run on a single thread, may come later.
7. **Tier-up runs on a background compiler thread,** which reads shared bytecode and publishes code by atomically swapping a function's code pointer. Call counters may be approximate.

## Consequences

- Code that never shares data pays nothing for threads: under counting strategies, one predictable branch per count update; under tracing, nothing.
- A Scheme program can use every core, on shared data, with its tasks scheduled by the host's own async runtime.
- Sharing a large structure costs one traversal of it, unless sharing is marked lazily (an open question in ADR 0003).
- Tracing strategies must stop every running task of an isolate at a safepoint; a task suspended at an `.await` is already stopped.
- Each isolate's run-time symbol table must allow concurrent access.
- Evaluations that must be isolated from each other still are: separate isolates share nothing mutable.

## Questions to settle before accepting

- The Scheme API for tasks and synchronisation: SRFI 18, futures and channels, or both.
- How fuel and memory limits are accounted across an isolate's parallel tasks: one shared budget, or a budget per task.
- How values are copied between isolates: a built-in deep copy, or only by serialisation.
- Whether a host can run an isolate's tasks on a single thread when its bridges return `!Send` futures.

## References

- Sebastian Ullrich and Leonardo de Moura, *Counting Immutable Beans: Reference Counting Optimized for Purely Functional Programming*, IFL 2019.
- Alex Reinking, Ningning Xie, Leonardo de Moura and Daan Leijen, *Perceus: Garbage Free Reference Counting with Reuse*, PLDI 2021.
- Jiho Choi, Thomas Shull and Josep Torrellas, *Biased Reference Counting: Minimizing Atomic Operations in Garbage Collection*, PACT 2018; PEP 703, *Making the Global Interpreter Lock Optional in CPython*.
- The Tokio documentation: `tokio::spawn` and the multi-threaded scheduler.
- Racket places; V8 isolates.
