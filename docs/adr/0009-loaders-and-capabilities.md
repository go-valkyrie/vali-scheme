# 0009. Loaders and capabilities

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, expansion-time reads: what counts as a read, read-only access, path resolution, Rust bridges
- **Amended:** 2026-11-16, marking Rust bridges as expansion-safe: meaning, enforcement, fingerprinting
- **Amended:** 2026-11-16, loaders may serve distribution artifacts instead of source (ADR 0012)
- **Amended:** 2026-11-16, no native code in artifacts; native code reaches disk only through `vali build` (ADR 0006)
- **Amended:** 2026-11-17, the synchronous and asynchronous loader traits, and fetching the import graph before expansion (item 1)
- **Amended:** 2026-11-17, until the capability API exists, a host grants what a library that reaches outside needs by serving that library itself; `include` reads through the loader; `exit` ends the run (items 6 and 8)
- **Related:** 0001 (cache validity), 0003 (memory limit), 0005 (untrusted bytecode), 0006 (native code), 0008 (isolates)

## Context

Vali is embedded in hosts that decide what scripts may do. A host may serve libraries from somewhere other than a directory tree, such as an OCI registry or a database, and may evaluate code it didn't write. Evaluating Steel and scheme-rs found both ways to get this wrong. scheme-rs searches a fixed path (the current directory, one environment variable, built-ins) and lets expansion-time code read any file. Steel blocks expansion-time file access entirely, so `include` cannot work.

Macros that read files are the hard case for caching: their output depends on those files, so the cache must know about every read. Other Schemes leave gaps here:

- **Racket** tracks `include` automatically. Any other file a macro reads is tracked only if the macro author calls `register-external-file`.
- **Rust's procedural macros** have the same gap.
- **Chez Scheme** tracks `include` by timestamp, and nothing else.

In each of these, a read nobody registered leaves a stale cache, with no error.

## Decision

### Loaders

1. **Libraries are found only through a loader that the host supplies.** Given a library name and the library importing it, a loader returns one of four things: the library's source with a canonical identity, a distribution artifact (ADR 0012), a cached artifact, or nothing. There is no built-in search path. Loaders come in a synchronous and an asynchronous form.
   - **The two forms have the same shape:** `Loader::load` returns a result, and `AsyncLoader::load` returns a future of the same result, `Send` so it can run on a multi-threaded executor. Every `Loader` is also an `AsyncLoader`.
   - **Loading happens before expansion, never during it.** A library's imports and its `include` declarations are known before its body is expanded, so compiling with an asynchronous loader first fetches the whole import graph, awaiting as it goes, then expands and compiles with everything in memory. The expander never awaits, which it couldn't do from inside a transformer anyway, since the VM is synchronous.
   - **An `include` inside a body,** not in a library's declarations, is only known during expansion. It needs a synchronous loader; under a purely asynchronous one it is an error that says so.
2. **Vali caches and reports by identity, not by name.** Two names that resolve to the same identity are the same library. Source spans in errors name the identity.
3. **Vali's own libraries,** `(scheme base)` and the rest, come from a built-in loader that hosts combine with their own.
4. **A loader that serves a cached artifact must also be able to serve the source or a distribution artifact,** for when the cached one is stale or was built by another Vali version (ADR 0005).
   - A distribution artifact from an untrusted source has its interface claims enforced (ADR 0012).

### Capabilities

5. **File, network, environment, clock and randomness access are objects the host grants** to an evaluation. Code that wasn't given one can't reach it. Nothing in Vali has ambient authority.
6. **Standard libraries that reach outside the program are instantiated with the evaluation's capabilities.** These are `(scheme file)`, `(scheme process-context)`, `(scheme time)` and `(scheme load)`. Importing them always succeeds. An operation without the capability it needs raises a condition that names the missing capability.

   **Until the capability API exists (M2), hosts serve these libraries.** For `(scheme file)`, `(scheme process-context)` and `(scheme time)`, compiling asks the host's loader first; Vali's own versions serve otherwise, and every operation in them raises a condition naming the capability it lacks. A host grants a capability by serving the library over a native library of its own (ADR 0016), as the test harness does for process-context. No other standard library can be replaced this way. `(jiffies-per-second)` is a constant, so it reads no clock and needs nothing.

   **Ending a run needs no capability:** it reaches nothing outside the program. `exit` runs every outstanding after thunk and then ends the current host call with `Error::Exit(code)`; `emergency-exit` does not run them. Neither ends the process, which is the host's to decide, and no handler or `guard` can stop either. They come from `(vali exit)`, which Vali's `(scheme process-context)` re-exports and a host's can too.
7. **Capabilities can be narrowed:** file access to some directories or to reading only, environment access to some variable names.

### Expansion time

8. **Expansion-time access uses the same mechanism,** with its own set of capabilities, separate from run time. A loader can attach a capability to each library it serves, such as read access to that library's own directory or package, so `include` works without granting more.

   **As built, `include` reads through the loader:** `Loader::include(path, from)` returns the text of a file the library or program `from` includes, or `None` if `from` may not read it, which is the default. The host decides what each identity may read. `include`, `include-ci` and `include-library-declarations` declarations are resolved while the import graph is fetched, and the files bodies include literally, nested includes too, are fetched then as well, so the expander never awaits and reads each file once (item 13). An `include` only a macro writes is not fetched, and is an error.
9. **Expansion-time access is read-only.** A file written during expansion would not be written again when the cached artifact is reused. Generating files is a build tool's job.
10. **Every expansion-time read is recorded** in the fingerprint of the library being expanded (ADR 0001), whichever library's macro made the read. Three kinds of access count as reads:
    - the contents of a file;
    - a check for whether a file exists, including a "no", so that creating the file later invalidates the artifact;
    - a directory listing, recorded as its sorted names. Listings are always returned sorted, so expansion stays deterministic.
11. **Reads are recorded by identity, not by machine path.** A resource is named relative to the identity of the library it belongs to (item 2), so an artifact built on one machine can be validated on another.
12. **A relative path resolves against the library that wrote it.**
    - **A path that appears in syntax** resolves against that syntax's source, as Racket's `include` does. In `(include "x")`, if `"x"` was written in library L, the file is next to L, even when a macro from another library produced the `include` form.
    - **A path that macro code builds as a plain string** resolves against the library being expanded.
    - **Paths never resolve against the process's current directory.**

    The read uses the capability attached to the library the path resolved against.
13. **One expansion sees one version of each resource.** The first read of a file, listing or existence check during a library's expansion is kept. Later reads in the same expansion get the same answer, so the recorded fingerprint matches what was used.
14. **Sources that aren't content** (the clock, randomness, the network, the environment) are unavailable at expansion time unless the host grants them. A library that uses one is not cached.
### Rust bridges at expansion time

15. **Bridges are run-time only by default.** A bridge's author can mark it *expansion-safe* where it is defined. Vali's own Rust-implemented primitives carry the same marking.
16. **Expansion-safe means deterministic in what Vali can see.** The bridge's result depends only on its arguments and on what it reads through the capabilities Vali passes it, and those reads are recorded (item 10). It does no other I/O, reads no clock or randomness, and keeps no state between calls that could change a result. Vali cannot check any of this; the marking is the author's promise.
17. **Run-time-only bridges cannot be called at expansion time.** Each phase has its own instance of a library (ADR 0001). In a bridged library's expansion-time instance, only expansion-safe bridges are bound to their Rust functions. The others are bound to a stub that raises a condition naming the bridge and the macro use that reached it.

    The stub fails only when called, because implicit phasing instantiates a whole library at expansion time, including functions only ever called at run time. Code written specifically for expansion time is another matter: transformers and other expansion-time expressions run only during expansion. So in those places, a direct reference to a run-time-only bridge is a compile-time error. The interface section records each bridge's marking for this check.
18. **A bridge that runs during expansion becomes part of the fingerprint.** Each expansion-safe bridge has a version. That version is recorded in the fingerprint of every library whose expansion called the bridge (ADR 0001). By default the version is derived from the defining crate's name and version, plus a hash of the bridge function's own source tokens. Changes inside the bridge's dependencies don't show up in that hash, so the author bumps an explicit version when those matter.
19. **Rust values made at expansion time stay there.** A macro can use them to compute its output, but they cannot appear in that output or in static information, which must be serialisable (ADR 0001). Trying is an error when the artifact is written.

### Policies and limits

20. **Import policies** are allowlists of the library names an evaluation may import. They are checked whenever an import is resolved, including by `eval` and `environment`.
21. **Each evaluation has resource limits:** a fuel budget decremented at function entry (ADR 0005), and a memory limit enforced by the allocator (ADR 0003). Both apply during expansion too, because macros can loop. Running out stops the evaluation and returns control to the host. Scheme code cannot catch it, and no `dynamic-wind` after-thunks run.
22. **The runtime never loads native code from a file.** Native code in a process is either JIT-compiled there or part of the executable itself (ADRs 0006 and 0013). Untrusted code arrives only as source or bytecode, and bytecode is safe by construction (ADR 0005), so no rule about trusting native code is needed.

## Consequences

- A host can serve libraries from an OCI registry or any other store, and sandbox code it didn't write, without changes to Vali.
- Every standard procedure that reaches outside the program needs a capability check, and standard libraries are instantiated per evaluation rather than once per process.
- Cached artifacts stay valid exactly as long as everything they read is unchanged.
- A macro cannot leave a stale cache by forgetting to declare a dependency. Scheme code has no way to read a file without it being recorded; only a bridge wrongly marked expansion-safe can.
- Hosts can expose any Rust function without thinking about expansion: an unmarked bridge is simply unavailable to macros.
- A macro author learns that a bridge is run-time only when compiling the macro, if the transformer refers to it directly. Otherwise the author learns at the first expansion that reaches the bridge.
- A wrong expansion-safe marking leaves stale or non-deterministic artifacts with no error. The determinism test from ADR 0001 (build twice, compare bytes) catches some of these cases.
- Each recorded read is checked on every warm run. ADR 0001 lets unchanged files skip re-hashing, but a library that reads hundreds of files at expansion time still slows every start.
- Data a program needs at run time is better read at run time, through run-time capabilities, than embedded by a macro. Editing the data then re-expands nothing, and the artifact stays small.
- Hosts must set up capabilities explicitly. Trusted uses such as a REPL or local scripts will need a convenience constructor that grants everything.

## Alternatives considered

- **A configurable search path:** easy to start with, but it cannot express an OCI registry or a database, and every host would add its own workaround.
- **Ambient access checked against a policy** (the process can do anything, and a policy says no): one missed check anywhere leaks access, whereas a missing capability cannot be used at all.
- **Opt-in dependency registration** (Racket's `register-external-file`, Rust's unstable `proc_macro::tracked_path`): any read the author forgets to register leaves a stale cache, silently.
- **Blocking expansion-time file access** (Steel): safe, but neither `include` nor schema-driven macros can work.
- **Every bridge available at expansion time:** convenient, but it reopens the opt-in registration problem for any bridge that does I/O.
- **Hosts mark bridges when registering them, instead of authors where they are defined:** the host knows its policy, but only the author knows whether the function is deterministic. Hosts keep control through import policies (item 20), which apply to expansion-time imports too.

## Left open

- The capability API, as part of milestone M2, replacing hosts serving their own versions of the standard libraries.
- Recording expansion-time reads in fingerprints (item 10), once artifacts are cached.
- Whether, and how, Scheme code can hold and pass capabilities explicitly in object-capability style, beyond those the standard libraries use.
- Signing and verifying artifacts that come from remote sources.
- How the bridge API spells the expansion-safe marking and an explicit version (for example `#[bridge(expansion_safe, version = "2")]`). This belongs with the rest of the bridge API in milestone M2.
- A form that declares the files a library reads at run time, like Racket's `define-runtime-path`, so loaders and packaging tools know what to ship with it.

## References

- Mark S. Miller, *Robust Composition: Towards a Unified Approach to Access Control and Concurrency Control*, PhD thesis, Johns Hopkins University, 2006.
- The Racket documentation: `compiler/cm-accomplice` (`register-external-file`), `include`, and `racket/runtime-path`.
- The Chez Scheme User's Guide, version 9.5: `compile-imported-libraries`.
