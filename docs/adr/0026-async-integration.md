# 0026. Async integration

- **Status:** Accepted, not built
- **Date:** 2026-11-20
- **Related:** 0001 (library instantiation), 0003 (handles, roots, epochs), 0004 (#9 suspension, #10 nested runs), 0005 (fuel), 0008 (isolates as futures, tasks), 0009 (loaders, fuel and limits), 0016 (bridges, panics), 0019 (async imports in WebAssembly), 0023 (limits)

## Context

Many hosts are not async: a command-line tool or a game loop has no executor and must be able to use Vali fully without one. Async hosts need three things that ADRs 0004, 0008, 0009 and 0016 promise without designing them: Scheme waiting for a host future (an HTTP request) while the thread returns to the executor; the host awaiting Scheme, racing it against a timeout, cancelling it by dropping it and `tokio::spawn`ing it; and Scheme not holding a worker thread, since Tokio asks for 10 to 100 microseconds between awaits.

What the code does today, and this record changes:

- **A host call is one synchronous run.** `Cx::execute` enters the VM and returns when the call ends; a second entry while one runs is refused (`Cx::running`).
- **Fuel is spent only at function entry** and compared with one cumulative limit (`Cx::set_fuel_limit`). A contified loop is a backward `jump` that enters no function, so it spends none: issue #69.
- **`eval` is Scheme over Rust primitives.** `%eval-prepare` fetches through a synchronous loader, expands, compiles and loads, and `eval` then runs the bodies it returns. It registers each library before its body runs, so a failed body leaves a registered library with unset variables: issue #70. `compile_program_async` already retries when expansion needs a loader answer (`Attempt::Need`, #65), but no async loader reaches a running isolate.
- **A bridge is a synchronous function,** and a panic in one becomes a Scheme condition (ADR 0016 #9).

Precedent: Wasmtime's `call_async` cancels on drop and yields every so many units of fuel; Lua forbids a yield across a C function; Chez's engines return what is left of a computation that ran out of fuel; Tokio wakes a task and returns `Pending` when its budget runs out; Node interleaves calls only at awaits.

## Decision

### Shape

1. **Two steps.** Step one gives each host call a task of its own on a shared isolate, with pausing at async bridges and at fuel, async bridges, callbacks from Rust into Scheme as futures, and `(vali async)` futures. Step two adds eager `spawn`, preemption between tasks and SRFI 18 (item 33). **Not built yet:** either step.

### The host API

2. **Two types, borrowed from prepared statements.** A `Callable` is a procedure prepared once: `isolate.callable(&procedure)` or `callable_named("handle-request")` resolves it, checks its arity and attaches default `CallOptions` (item 9). It is reusable, cheap to clone and `Send + Sync + 'static`. `callable.bind(&args) -> Result<Call, Error>` makes one run, and nothing runs until the `Call` is driven. Driving consumes it, and a pause for fuel hands it back (item 10); **dropping it cancels and discards the run, whatever its state** (item 23). Running again is `bind` again. The `Call` is the host's handle; the internal `Task` is the run's state in `vali-runtime`. **`call` is always synchronous and `call_async` always the future,** on the owned `Isolate` and on `SharedIsolate`. They prepare, bind and drive with the defaults and return `Result<Handle, Error>` through item 10's conversion, so code using them never sees a `Call`, and running out of fuel ends the run; `call_with` and `call_async_with` take a `CallOptions`.
3. **A synchronous host keeps the owned `Isolate`.** `call`, `callable` and `with` take `&mut self`, which makes entering exclusive with no lock, no future and no executor. Its `Call` is driven by `call.run_on(&mut isolate)`, which consumes it and returns `Result<Handle, Stopped>`: the isolate is passed explicitly, and a `Call` of another isolate is `Error::WrongIsolate`.
4. **An async host shares the isolate.** `isolate.into_shared()` gives a `SharedIsolate`, `Clone + Send + Sync`: an `Arc` around the isolate and a plain `std::sync::Mutex`. Its `Callable`s and `Call`s carry the `Arc`, so a `Call` is `Send + 'static`, and **a `Call` is itself `IntoFuture`, by value,** with output `Result<Handle, Stopped>`. `shared.call_async(..)` returns that future, so a host can `tokio::spawn` it. A poll takes the lock, swaps the run's task into `Cx`, runs it for at most the poll budget (item 12), swaps it out and unlocks. The lock is never held across an await, so a wait for it lasts one poll. `Call` has `fuel_used()`, `add_fuel(n)` and `set_options(CallOptions)`, before it is driven and when handed back. `SharedIsolate` also has synchronous `with`, `call` and `Call::run()`, which take the same lock, so a host can look inside while calls are parked; called from a bridge's synchronous part, which runs under the lock, they would deadlock, so a thread-local flag makes that a catchable condition.
5. **Calls interleave only at pauses,** at an await or the end of a poll, so Scheme state that calls share can change across an await, as on Node's event loop.
6. **A synchronous call never waits.** When Scheme reaches an async bridge in a synchronous `call`, it raises a catchable condition naming the bridge and saying the call must be driven as a future, before the bridge converts its arguments. `block_on` would hold a thread and deadlock a single-threaded executor. **Vali depends on no executor,** only `std::task` and `std::sync`; a synchronous host that wants async bridges drives a `Call` with any minimal `block_on` (`pollster`, say).
7. **Dropping a `Call` or the future that owns it cancels it** (item 23). Timeouts are the host's: `timeout(shared.call_async(..))` works because dropping cancels.

### Fuel

8. **Metering is an isolate setting, off by default:** `HeapConfig::metered` (ADR 0023's one configuration type). **A metered isolate** counts fuel exactly and supports fuel limits: `CallOptions.fuel`, the isolate-wide cap, `Stopped::OutOfFuel` and `fuel_used()`. Fuel is one unit at each function entry and backward jump of the per-library bytecode (a call into Rust spends none). The per-library optimiser runs before fuel is defined and may change counts, as `VALI_NO_OPT` does; only what happens after the artifact is loaded must preserve them. **An unmetered isolate** tracks no fuel: `CallOptions { fuel: Some(_) }` and setting the cap return `Err(Error::NotMetered)`, never silently ignored, and `fuel_used()` returns `None`. The switch is per isolate, not per call, because JIT code is shared between isolates and entry points are swapped on tier-up (ADR 0008 #9); Wasmtime's `consume_fuel` splits the same way. **Not built yet:** backward jumps spend nothing today (#69, to be fixed first).
    - **Fuel in later tiers is exact in total, with a bounded pause window.** Every tier charges exactly what the bytecode would. The JIT and the cross-library linker (ADR 0012 #11) charge for work they remove or restructure: an inlined call charges its entry, an unrolled loop charges per original iteration, a folded loop folds its fuel. So `fuel_used()` at the end of a run, and whether a limit is reached, are the same in every tier and deterministic. A later tier may deduct a block's static cost at once, so a pause can land up to one block's fuel past where tier 0 would pause; nothing Scheme can observe depends on where a run pauses, and quotas see totals. The overshoot bound is documented.
9. **Fuel belongs to a `Call`.** The call and every `(vali async)` future it drives (item 24) share its budget, which closes the escape of splitting work into futures. A `SchemeFn` callback (item 15) is a new `Call` with its own. Options are one plain struct for every kind of call, sync and async, with public fields, not `#[non_exhaustive]`, and `Default`; a `Callable` holds the defaults, `bind` copies them, and `set_options` changes one run's:

   ```rust
   CallOptions { fuel: Some(n), ..Default::default() }
   ```

   `fuel: Option<u64>` is this call's limit, default `None`, unlimited; a host that sandboxes sets it, on a metered isolate. `poll_budget: Option<u64>` is the yield interval of item 12, default `None`. `call(&proc, &args)` uses the defaults and `call_with(&proc, &args, options)` takes them.
10. **Running out of fuel pauses the call and hands it back.** Driving consumes the `Call`, which makes a finished one unrepresentable: `.await` consumes a future, and driving twice is impossible by type. The output is `Result<Handle, Stopped>`, with `enum Stopped { OutOfFuel(Call), Failed(Error) }` (a working name), not `Clone`. `OutOfFuel` returns the same run, as `std::sync::mpsc::SendError<T>` returns its value: the host calls `add_fuel` and drives it again, or drops it, which discards the run. `Failed` carries an ordinary `Error`, which keeps `Clone, PartialEq, Eq`. `impl From<Stopped> for Error` lets `?` work when the host does not resume; it drops the `Call` inside, which discards the run, the hard stop, and `Error::OutOfFuel { used }` is what it produces. A loop is the natural resume pattern:

    ```rust
    let mut call = handler.bind(&[req.into()])?;
    let v = loop {
        match call.await {
            Ok(v) => break v,
            Err(Stopped::OutOfFuel(c)) if quota.allows(c.fuel_used()) => { call = c; call.add_fuel(100_000); }
            Err(e) => return Err(e.into()),
        }
    };
    ```

    The parked task counts against the memory and stack limits while the host holds the `Call`. Scheme cannot see or catch the pause, so it cannot refuel itself, and it needs no futures.
11. **An isolate-wide cap stays, on a metered isolate:** an optional total across all calls (`set_fuel_limit`, ADR 0009 #21). Reaching it pauses every running `Call`, each handing itself back in `OutOfFuel`; the host raises the cap and drives them again, since a call driven under a cap still reached pauses again. `fuel_used()` reports a call's fuel (the error carries it as `used`) and the isolate's total.
12. **Yielding is opt-in,** so trusted code pays nothing for it. With a `poll_budget`, a run yields once it is spent, a precaution for code not trusted to yield: the VM saves its state in the task, calls `wake_by_ref` on the poll's waker, and the poll returns `Pending`. A synchronous call never yields. **4,096 is the recommended value,** about 50 to 200 microseconds of tier-0 Scheme (`runtime-bench`: 12 to 47 ns per call), against Tokio's 10 to 100 microseconds between awaits; to be re-measured. In a metered isolate the budget shares the fuel counter, and the VM tests one number, the soonest of the call's limit, the cap and the end of the poll. **`interpret` is generic over `const COUNTS: bool`:** one copy decrements and compares at entries and backward jumps and one does not, chosen once per poll. A metered isolate always counts; an unmetered one counts only when the run has a `poll_budget`; otherwise nothing is checked.

### Bridges

13. **An async bridge returns a future, and its synchronous part does the borrowing.** Either `fn query(&self, sql: &str) -> impl Future<Output = Result<Rows, E>> + Send + 'static`, whose body takes what the future needs under the borrow (a clone, an `Arc`), or an `async fn` with no borrowed parameters. Arguments convert through ADR 0016 #4 when Scheme calls. The macro rejects an `async fn` with a borrowed receiver or parameter such as `&str`, pointing to the split form. The future is `Send + 'static` (ADR 0008 #8), holds no Scheme value and no borrow, and **runs outside the lock**. Object borrows (ADR 0016 #7) end when the synchronous part returns, and other calls run meanwhile, so state the future changes after an await is the author's to guard, with a `std` `Mutex` in an `Arc`. A synchronous bridge that blocks still blocks the executor thread; Vali has no executor to move it to.
14. **A bridge may answer at once or defer, per call.** Its result is an `IntoScheme` value or a future of one. A future's output converts back on the isolate's side after it completes, so the future never touches the heap. A failed conversion, and an `Err` the future resolves to, raise a condition at the call naming the bridge, so a `guard` around the call sees it.
15. **`SchemeFn<Args, Ret>` is a parameter type: a Scheme procedure, continuations included, seen from Rust as a future.** It converts from any procedure, capturing the `Handle` and the shared isolate from the `Context` in the synchronous part, and is `Send + Clone + 'static`. `f.call_async(args)` returns a `Call`, so Rust has one handle type for every Scheme run, a `'static + Send` future of a new task whose output converts through `FromScheme`. Scheme errors come out as `Err`, and dropping it cancels that task. It cannot deadlock, because the bridge's future is polled outside the lock. Under an owned isolate it raises item 6's condition. It is the async host's answer to nested runs (item 30).
16. **`Resume<T>` declares an async bridge for callback-style APIs:** `fn read(&self, n: usize, done: Resume<Bytes>)`. It is a one-shot completion token over a `std` channel, with the same pause as a future. `done.resume(Result<T, E>)` may be called from any thread; dropping it unresumed raises an error naming the bridge; resuming after the call was cancelled does nothing.
17. **Async bridges are run-time only.** Marking one expansion-safe is an error (ADR 0009 #15). Transformers run in a synchronous isolate, so an async bridge reached while expanding raises item 6's condition, as ADR 0009 #17 does for any run-time-only bridge.

### Panics

18. **A panic is a programmer error, not a condition,** in sync and async bridges and in Vali's own code alike. It poisons the whole isolate; nothing in Scheme sees it, and no handler or `guard` runs. Vali catches it only long enough to mark the isolate poisoned, then `resume_unwind`s it from the call that hit it, so Tokio reports a `JoinError` and a synchronous caller unwinds. Once poisoned, every later `call`, `with` or driven `Call` returns `Err(Error::Poisoned)` naming the bridge and the message, paused calls resolve to it on their next poll, and the only recovery is a new isolate. A discard on a poisoned isolate releases no counts. `SharedIsolate`'s `Mutex` poisons itself when a poll unwinds holding it, and that `PoisonError` maps to `Error::Poisoned`. An author who wants resilience catches the panic inside the bridge and returns `Err`. `panic = "abort"` builds abort.

### Pausing in tier 0

19. **A pause is a return from the VM with the task intact.** The pause points are a call to an async bridge, and fuel running out at an entry or a backward jump. `interpret` returns an outcome (finished, yielded, awaiting); the frames stay in the task's segments; the registers it keeps in locals (`fp`, `pc`, the call's base slot and resume word, and under precise counting the innermost frame's slot count) go into a `Pending` record in the `Task`, and a bridge's future into `Task::awaiting`. Resuming re-enters through the trampoline and reloads from the frame's closure and `pc`. Each entry rewrites the entry frame's host stack word and the `DebugState` thread id (ADR 0004 #6).
20. **Delivery.** A poll first polls the bridge's future, outside the lock, with its own waker. When ready, the lock is taken, the output converts, and the result is delivered as the call's return value, or, for a call in tail position, as the return of the frame it replaced, so an await in tail position grows nothing. An error raises at that call. The task's `%catch`es, the continuation below it and its dynamic state stay across the pause, so a `guard` spanning an await still catches, and a pause runs no `dynamic-wind` thunk.
21. **Parked tasks are roots.** `Cx` holds the running task by value (ADR 0008 #2) and a table of parked ones, one per call that waits. The collector scans both, so a paused stack, its catches, `below` and dynamic registers stay live and, under precise counting, counted. Epochs run only at calls, backward jumps and host returns (ADR 0003), none during a pause. A paused task's segments count against the stack limit, and its spare segment is freed when it awaits.
22. **JIT frames need real stack switching,** and this leaves room for it. Native frames on segments (ADR 0004 #2) cannot be left by returning, so a pause there switches to the thread's stack, with Vali's assembly or a crate such as `corosensei` (ADR 0004 Left open). Cancellation is safe because Rust never runs on segments (ADR 0004 #7). **The JIT ADR (M5) owns the mechanism;** this record states the guarantees of item 8. Unmetered isolates track no fuel: a run with a `poll_budget` uses the counting variant only to yield, with approximate yields, and a run without one uses a variant with no checks, in every tier. Per-block costs, a register counter, caching the variants for shared code, and the pause are that ADR's.

### Cancellation

23. **A run is discarded when its `Call` is dropped or cancelled,** in any state, as Rust's drop-cancellation says. Its stack, pending call and bridge future are dropped, and `dynamic-wind` after thunks do not run, as for `Exit`. What the task did to the world stays. The isolate stays usable, which takes:
    - **The task:** the end of a failed run, as `finish_run` does: frames released from `Pending`'s `fp` outward (`release_frames`), waiting `%catch`es' handlers released, `below` and the dynamic registers cleared, the stack cut to one whole segment, the pending tail call dropped.
    - **The heap:** a pause falls between instructions, so nothing is half-built and nothing is rolled back.
    - **The re-entry guard:** `running` is false at every pause. A leaked `Call` (`mem::forget`) leaves a parked task holding its stack until the isolate drops.
    - **Handles and objects:** no handle is invalidated, and no object borrow is held (item 13).
    - **Retired modules:** the graveyard is emptied only when no frame of any task is live.
    - **Libraries `eval` has loaded and not finished running** (item 29).

### `(vali async)`: lazy futures

24. **`(vali async)` exports `future`, `await`, `await-all`, `await-any`, `future-cancel`, `future?`, `future-done?` and `yield`.** `(future thunk)` starts nothing: like a Rust future, it runs only while awaited. `(await f)` runs its thunk in a new task with its own stack, starting with the creating task's parameterization and handlers, driven inside the awaiting task's poll. `await-all` and `await-any` drive several futures inside that poll, taking turns at their pauses. Step one has no ready queue or scheduler. The structure is a tree: cancelling a parent cancels the children it drives, and their fuel is the call's (item 9).
25. **Awaiting.** A finished future returns its cached result or re-raises its error, as `force` does. Of two tasks awaiting one future, one drives it and the other waits through a short list of wakers on it. A future's driver holds its run, so cancelling the driver cancels the future, and other awaiters get a "future was cancelled" condition; handing it to another waiter is left to step two. A `(vali async)` future is the Scheme-side handle to the same kind of run as a `Call`. `await` walks the chain of futures being driven and raises "awaiting this future would wait on itself" on a cycle. A continuation captured in one future's task and invoked in another is refused for now, naming both tasks.
26. **Rust futures need no variant:** `(future (lambda () (http-get url)))` wraps any async bridge call. **`(yield)`** in an async run wakes the run's waker and returns `Pending`, as `tokio::task::yield_now` does; in a synchronous `call` it does nothing, and it costs nothing unless called. Inside futures driven by `await-all` or `await-any` it gives the others in that poll a turn. Trusted long loops under `call_async` should `(yield)` now and then, or the host sets a `poll_budget`. **In a synchronous `call`,** `await` runs a thunk to completion on the spot, so `(vali async)` works in synchronous hosts, and only reaching a Rust async bridge raises item 6's condition.
27. **Naming.** `future` and `await` stay, though in Multilisp, Racket and Guile `future` and `touch` mean parallel evaluation. In step two `(spawn thunk)` is eager and returns the same kind of future, and SRFI 18's `thread-join!` is an `await` on it.

### `eval` and loading at run time

28. **`eval` fetches, then expands, and the fetch may await.** "Loading happens before expansion, never during it" (ADR 0009 #1) holds. `%eval-prepare` splits into planning the missing imports, a fetch, and expanding, compiling and loading with the graph in memory; `eval`, in Scheme, calls them in turn, and the fetch is a bridge that answers at once or defers (item 14).
    - **Under an `AsyncLoader`** it awaits the loader, so `eval` of code with imports not yet loaded needs an async call; in a synchronous one it raises item 6's condition naming `eval`. Code whose imports are all loaded needs no fetch and works in either. The isolate holds the loader behind an object-safe wrapper, since `AsyncLoader` returns `impl Future`.
    - **Under a synchronous `Loader`** the fetch finishes at once and `eval` works in both kinds of call.
    - **What only a macro names** is not an error. With no resolver, a `cond-expand` library test a macro builds stops expansion with the needed name (`library_answer`, `Attempt::Need`, as #65 does for `compile_program_async`); the fetch awaits the loader for it, and expansion runs again. Only an `include` in a body, known only during expansion, stays an error under an async-only loader (ADR 0009 #1).
29. **A library `eval` has loaded and not finished running is unregistered when its task ends without finishing it.** `eval` registers each library when it loads it, so later libraries of the evaluation link to it, and runs its body afterwards. A failed body or a discard leaves it registered with variables never set, a bug today with no async (#70, to be fixed first). The rule: the isolate marks a library instantiating from registration until its body returns, and unregisters those still marked when the task ends; the evaluator's expander forgets them too, or the next `eval` would skip loading them. An async `load` is left open.

### Nested runs, ports and limits

30. **Nested runs are synchronous only, for synchronous hosts.** When Rust must call Scheme and wait (ADR 0004 #10), the nested run cannot await: an async bridge reached inside one raises a catchable condition at the continuation barrier, and the run takes no poll yield. A Rust frame under it cannot be parked, which is Lua's wall. Async hosts use `SchemeFn` (item 15). **Not built yet:** nested runs; `Cx::execute` refuses re-entry today.
31. **Async ports are bridges, later,** in item 13's split form; console and string ports stay synchronous (ADR 0009 #5 for the capability).
32. **Limits apply across awaits:** the memory limit and ADR 0023's caps count what parked tasks hold, and the stack limit their segments. Awaiting spends no fuel; the host's timeout limits wall-clock time.

### Step two

33. **Step two adds eager tasks and preemption.** `(spawn thunk)` makes a task at once, queues it and returns a future (item 27). A scheduler loop in the poll runs ready tasks, polls awaiting ones through wakers of their own, and returns `Pending` when none can run. Preemption uses item 12's counter: the VM stops at the soonest of the call's limit, the end of the poll and the end of the running task's slice. SRFI 18's blocking operations are awaits inside the isolate: sleeps and timeouts await a host timer capability, and a held mutex, a condition variable and `thread-join!` block the task in the object's queue of task ids. `Task` gains an id, a state, a result, joiners and SRFI 18's name and specific.

## Consequences

- A sync host uses Vali with no executor, lock or future, and an async host gets `'static + Send` futures that `tokio::spawn` accepts.
- Trusted code pays nothing for fuel or yielding. Code that must be bounded runs on a metered isolate or with a `poll_budget`; the cost is a second copy of the interpreter loop in the binary, and a counting variant in the JIT.
- Calls on a shared isolate interleave at awaits. A panic poisons the isolate and unwinds through the host.
- A `Call` paused for fuel keeps its stack until driven or dropped, counting against the memory and stack limits.
- Async I/O is unavailable in a synchronous `call`, a nested run and at expansion time, each by an error naming the bridge.

## Alternatives considered

- **Borrowing the owned isolate for a call:** no lock, but one call at a time, no looking inside while a call waits, and no `tokio::spawn`. It stays the sync host's `call`.
- **Handing Rust the pause as a multi-shot Scheme continuation:** it would run the rest of the call twice, cross host-call dynamic-state boundaries, cost a capture at every await, and need a second cancellation mechanism.
- **An `async` VM** or **stack switching in tier 0:** a state machine or a switch, where a `Pending` record and a return from `interpret` serve. Only the JIT needs a switch (item 22).
- **Yield by clock or epoch** (Wasmtime's): one load per check, but it needs a timer and is not repeatable. It can come later as another budget.
- **`&mut Call` awaitable again with a cached result,** and **`Error::CallFinished`:** both make a finished `Call` a runtime state, where consuming it rules it out by type.
- **An owning `Paused` in the error,** and **a `CallId` with `resume`, `discard` and `paused_calls()`:** the first broke `Error`'s `Clone` and `Eq`, and the second needs the host to sweep tasks no value owns.
- **Exact pause points as well:** deopt or per-point checks near the limit, the most cost for the least benefit.
- **Approximate fuel once code tiers up:** counts would depend on background tier-up timing.
- **Keeping metered isolates in tier 0:** unnecessary, since the JIT needs a counting variant for the poll budget anyway.
- **Metered by default,** or **a poll budget on by default:** trusted code would pay for tracking it does not need.
- **Fuel at function entry only,** or **the isolate's, not the call's:** a contified loop would spend none (#69), and work split into futures would escape a per-call limit unless futures share the call's.

## Left open

- **An opt-in `set_drive_async(true)`:** a synchronous `call` would poll a runtime-agnostic bridge future to completion on the calling thread, instead of raising item 6's condition.
- **Details of step one:** the spelling of `load_with_async`, the name `Stopped`, of the `#[bridge]` attribute for async functions, and of `SchemeFn`'s argument and result types; an async load of a program's libraries; a variant for `!Send` futures (ADR 0008 #8); whether `HeapConfig` (ADR 0023) holds `CallOptions`' fields; re-measuring the recommended 4,096, and the JIT's mechanism (item 22).
- **Step two:** what happens to spawned tasks when the call's task ends; what to do when no task is ready and none awaits the host (an error in the call's task, in every blocked task, or leaving it to the host's timeout), and abandoned mutexes; the time slice's default and its relation to the poll budget; whether a spawned task inherits the parameterization; SRFI 18 priorities; loosening item 25's refusal of continuations across tasks; how an uncaught error in a spawned task reaches `thread-join!`; importers waiting on a library another task is instantiating; and the timer capability's shape (ADR 0009 #5).
- **Awaiting in nested runs,** with real stack switching (item 22).
- **WebAssembly:** ADR 0019 #13 lowers an async import to a suspended task, item 19's pause with a subtask for the Rust future.

## References

- Tokio documentation: cooperative scheduling, and "Async: What is blocking?".
- The Wasmtime documentation: `Func::call_async`, `Store::fuel_async_yield_interval`, `Config::consume_fuel`.
- R. Kent Dybvig and Robert Hieb, *Engines from Continuations*, Computer Languages 1989.
- SRFI 18, *Multithreading support*.
- Issues #65, #69 and #70.
