# 0008. Threading model

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, reworked: an isolate's tasks share its heap and run in parallel; objects are local to a task until shared
- **Amended:** 2026-11-17, a Rust handle no longer shares its object by existing; using it from another task does (ADR 0003)
- **Amended:** 2026-11-17, reworked again and accepted: each heap is used by one thread at a time; tasks within an isolate are concurrent but not parallel; parallelism comes from isolates that exchange copied messages and use explicitly shared memory
- **Related:** 0002 (values), 0003 (memory management), 0004 (continuations, async), 0006 (shared code), 0009 (sandboxing, limits)

## Context

Whether Scheme values can cross threads affects every runtime type. It decides whether counts and fields need atomic access, whether the collector must stop several threads at once, and what Rust code must prove before touching a value.

Hosts are async Rust programs, typically on Tokio's multi-threaded runtime. `tokio::spawn` requires a `Send` future, and the runtime moves tasks between worker threads at any `.await`. A future is polled by only one thread at a time.

What hosts and programs want from "threads" is mostly **concurrency**: many evaluations or requests in flight, each waiting on I/O. Async gives that without parallel access to memory. **Parallel computation** is the rarer need, and the people who need it are best served by message passing plus explicitly shared memory, as MPI programs are, not by unrestricted shared mutable state.

The previous version of this record chose one heap per isolate shared by its parallel tasks, with every object local to a task until shared, as Lean 4 and Koka do. Working it through showed what that costs:

- **In the memory contract:** a share operation that marks whole structures, a shared mark in every header, atomic exchange for stores into shared objects, concurrent cycle collection for shared cycles, and stop-the-world coordination across threads for tracing strategies (ADR 0003).
- **In the Rust API:** handles owned by a task, an error and a sharing protocol whenever another task of the same program used one, and a context per task.
- **At run time:** measured on a 2.1 GHz Xeon, an uncontended atomic increment and decrement costs about 18 ns against about 1.2 ns plain, and a contended one about 90 ns. Walking a list in cache, atomic counting costs 2 to 3 times as much as the walk. Avoiding atomic counts cut Swift programs' run time by 22.5% and 7.3% (Choi, Shull and Torrellas, PACT 2018), and scheme-rs spent 19–32% of main-thread time on them.

Precedent for the alternative is broad. Racket runs threads concurrently within a place and places in parallel, exchanging messages over place channels. Termite Scheme brings Erlang's processes and mailboxes to Gambit. Erlang collects each process's heap on its own, so nothing stops the world. Dart's isolates have their own heaps and share code and immutable objects. JavaScript pairs an event loop with workers, and Python 3.12 gives each subinterpreter its own lock (PEP 684, PEP 734).

## Decision

1. **An isolate is the unit of memory and of sandboxing:** a heap, its library instances (one per phase, ADR 0001), its table of symbols interned at run time, its capabilities, import policy and resource limits (ADR 0009). **One thread at a time uses an isolate's heap.**
2. **An isolate runs any number of tasks, concurrently but not in parallel.** A task is a green thread: a continuation the isolate schedules (ADR 0004). When a task awaits a host future, another task runs. Tasks share the isolate's memory freely, with no atomics and no locks.
3. **SRFI 18 threads are tasks.** Mutexes, condition variables and thread-local state work as SRFI 18 specifies; they give concurrency, not parallel speed-up.
4. **An isolate is driven as a `Send` future** and may move between host threads between polls. Entering it is exclusive, as a lock per isolate would make it. Rust code holds values through handles owned by the isolate and uses them through its context (ADR 0003).
5. **Parallelism comes from running several isolates.** They run on the host's worker threads at the same time and exchange values in two ways:
   - **Messages, copied.** Sending a value over a channel copies the graph of objects it reaches into the receiving isolate, keeping sharing and cycles. What can be sent, and how procedures and ports are treated, is settled with the Scheme API.
   - **Explicitly shared memory.** Objects created as shared live outside every isolate heap and can be reached from several isolates at once. Every access to them is atomic or synchronised, and their cost is paid only by programs that ask for them.
6. **Shared between isolates, read-only:** loaded artifacts and bytecode, JIT-compiled code (ADR 0006), the global table of symbols that appear in compiled code, and immutable literals. Symbols interned at run time go into the isolate's own table, which its collector can free and its memory limit counts; `string->symbol` looks in the global table first.
7. **Memory strategies are single-threaded per heap.** Count updates are plain, there is no shared mark, and collection is per isolate, never stopping another isolate. A collector may still run on a thread of its own beside its isolate, if a strategy is built that way (ADR 0003).
8. **Async bridges return `Send` futures,** since isolates are `Send`. A variant for `!Send` futures, run on a single thread, may come later.
9. **Tier-up runs on a background compiler thread,** which reads shared bytecode and publishes code by atomically swapping a function's code pointer. Call counters may be approximate.

## Consequences

- Scheme code never pays for atomics, except on objects a program explicitly created as shared.
- Memory safety within an isolate needs no argument about races: only one thread touches its heap at a time.
- Collection pauses are per isolate. A long-running parallel program made of many isolates never stops all of them at once.
- Sending a large structure costs a copy proportional to its size. Programs that need parallel access to one large structure use shared memory, such as shared bytevectors.
- An isolate is heavier than a task: it instantiates its own libraries. Snapshotting a parent's initialised libraries could make spawning one cheap later.
- SRFI 18 programs keep their meaning but gain no parallel speed-up. Vali needs a second Scheme API, for isolates, channels and shared memory.
- Evaluations that must be isolated from each other run in separate isolates, as before.

## Alternatives considered

- **One heap per isolate shared by parallel tasks, objects local until shared** (Lean 4, Koka; the previous version of this record): no copying between tasks, but the costs listed under Context, in the memory contract, the Rust API and the run time.
- **One heap, every value shared** (scheme-rs): every count update atomic.
- **One heap, biased counting** (Choi, Shull and Torrellas; Python's free-threaded build): cheap until tasks migrate between threads, which under Tokio they do constantly.
- **A heap per task, as Erlang has:** the cheapest messages and the finest-grained collection, but Scheme libraries routinely hold mutable top-level state, so every task would need its own library instances, and spawning a task would cost as much as spawning an isolate.

## Left open

- **The Scheme API for parallelism:** spawning an isolate, channels between isolates (blocking, or Concurrent ML-style events as Racket's are), collective operations in the style of MPI (broadcast, scatter, gather, reduce), and what can be sent: data, records whose type both isolates know, and possibly procedures whose code is shared and which capture nothing mutable.
- **The shared-memory API:** shared bytevectors with atomic operations, as JavaScript's `SharedArrayBuffer` and `Atomics` provide, and possibly concurrent tables later.
- **How tasks within an isolate are scheduled:** only at awaits and blocking operations, or also preempted by fuel at safepoints (ADR 0009).
- **How resource limits are set when one program runs as several isolates:** a budget per isolate, or one shared by a group.

## References

- Kevin Tew, James Swaine, Matthew Flatt, Robert Bruce Findler and Peter Dinda, *Places: Adding Message-Passing Parallelism to Racket*, DLS 2011.
- Guillaume Germain, Marc Feeley and Stefan Monnier, *Concurrency Oriented Programming in Termite Scheme*, Scheme Workshop 2006.
- Joe Armstrong, *Making Reliable Distributed Systems in the Presence of Software Errors*, PhD thesis, 2003: Erlang's per-process heaps.
- The Dart documentation on isolates and isolate groups.
- PEP 684, *A Per-Interpreter GIL*, and PEP 734, *Multiple Interpreters in the Stdlib*.
- Jiho Choi, Thomas Shull and Josep Torrellas, *Biased Reference Counting: Minimizing Atomic Operations in Garbage Collection*, PACT 2018.
- SRFI 18, *Multithreading support*.
