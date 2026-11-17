# 0013. Standalone executables

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, no native code in artifacts; native code reaches disk only through `vali build` (ADR 0006)
- **Amended:** 2026-11-16, debug information is self-contained: usable without the source files
- **Amended:** 2026-11-16, which libraries `eval` keeps, what it brings in, and how its cost is contained (items 11 to 14)
- **Amended:** 2026-11-17, `eval` as built in tier 0: the compiling expander kept only for programs that import `(scheme eval)`, and evaluated code run through thunks (item 14)
- **Related:** 0003 (memory-management strategies), 0006 (code generator), 0009 (capabilities), 0010 (static dispatch), 0012 (distribution format), 0015 (run-time syntax objects)

## Context

The main purpose of AOT compilation is to turn a Scheme program into a standalone executable: one file that runs without Vali installed, starts as fast as native code, and can be shipped like any other binary. It is also the only way native code is written to disk: JIT-compiled code is never cached between runs (ADR 0006).

A standalone build has needs that the embedded case doesn't:

- **The strategy is a property of the output.** A program built with `vali build -mcms` needs the concurrent mark-sweep strategy, even if the `vali` tool itself was built with reference counting for its REPL and scripts.
- **The binary has to be complete.** It cannot fall back to a bytecode cache it doesn't have, unless it carries the VM.
- **Everything is known at build time.** That allows optimisations a library compiled on its own cannot make.

## Decision

1. **`vali build` produces a standalone executable** from a program and every library it imports.
2. **Its inputs are distribution artifacts** (ADR 0012), either compiled from source on the spot or shipped closed.
3. **A build target is a target triple plus a memory-management strategy,** for example `x86_64-linux` with `-mcms`. Without `-m`, the build uses the default strategy. The `vali` tool's own strategy is independent of the strategy it builds for, just as a cross-compiler's host CPU is independent of its target.
4. **Macros run in the tool's own runtime.** Strategies never change what a program means (ADR 0003), and distribution artifacts don't depend on the strategy, so the expanded program is the same whichever runtime ran the macros.
5. **The shared code generator (ADR 0006) writes platform object files** for the target, with the target strategy's barriers, count operations and allocation fast paths inlined.
6. **The toolchain ships the runtime as a static archive for each strategy and target,** such as `libvali_rt-rc.a` or `libvali_rt-cms.a`. `vali build` links the program's object files with the archive for its target. Each archive is the runtime instantiated for one strategy (ADR 0003), so every hook is inlined. The archives for the host target ship with the toolchain; others are fetched when first needed.
7. **The executable carries only what the program can use.** The expander, compiler and VM are linked in only if the program can compile code at run time: through `eval`, `environment`, `load`, or loading libraries dynamically. Otherwise the binary holds native code and the runtime alone. Items 11 to 14 say what such a binary carries.
8. **Native code must cover the whole program.** If any reachable code cannot be compiled to native code for the target, the build fails with an error naming it. The exception is a binary that carries the VM; there, the code may run as bytecode, with a warning at build time.
9. **Whole-program optimisation is allowed.** Every library in the binary is fixed at build time, so the build may inline across library boundaries without the consent rule of ADR 0012 (which protects libraries distributed separately). It may also seal generic functions and dispatch statically wherever contracts allow (ADR 0010).
10. **Start-up runs each library's body as native code,** in dependency order. There is no artifact cache and nothing to validate.

### `eval` in a standalone executable

11. **The build keeps the libraries `eval` can reach, and only those.** Evaluated code is expanded and compiled at run time, so the binary must hold every library that code can import. The build decides which ones:
    - **From the program, when it can.** If every environment passed to `eval` or `load` is built from constant library names, as in `(eval form (environment '(scheme base) '(my lib)))`, those libraries and their dependencies are kept.
    - **From the build configuration otherwise.** Environments the build cannot work out, such as `(interaction-environment)` or an environment computed at run time, must be declared in the build configuration, with the libraries they may contain.
    - **Otherwise the build fails** with an error naming the call it could not resolve and saying how to declare its environment.
12. **What `eval` brings in.** A binary that can evaluate code also carries:
    - the expander, the compiler as far as the distribution format, conversion to run-format bytecode and the tier-0 VM. The VM also runs macro transformers during expansion;
    - the interface and visit sections of every library kept by item 11, since expanding code that uses them needs their exports and their compiled macros;
    - the scopes and binding data that run-time syntax objects need (ADR 0015 #10).
13. **Evaluated code runs on tier 0.** The program's own code is already native, and evaluated code is usually small and short-lived, so Cranelift is not linked unless the build asks for JIT compilation of evaluated code. A build option also lets evaluated code skip the optimisation passes, going straight from expanded code to the distribution format, which makes both the binary and each `eval` cheaper.
14. **The size is visible.** Build output says why the front end was included and lists the calls that caused it. CI builds the same program with and without `eval` and tracks both sizes, so the cost is measured rather than guessed.

    **As built in tier 0,** before executables exist: compiling a program that imports `(scheme eval)` keeps the expander that compiled it, with everything it knows of the program's libraries, and loading the program keeps it in the isolate. Programs that don't import `(scheme eval)` keep nothing. Each `eval` expands only the new code, against the libraries already compiled, and fetches and compiles any library it imports that isn't loaded yet: Vali's own, or the host's through the loader the host passed to `load_with` (ADR 0009 #1). A Rust procedure can't run Scheme code, so `%eval-prepare` compiles and loads the code and returns thunks, each new library's body then the code's own, which `eval`, in Scheme, calls in order, the last in tail position. Environments are immutable sets of imports, checked when `environment` makes them; definitions in evaluated code stay local to it. `interaction-environment`, which a REPL needs, waits for the REPL. Each evaluation's module stays loaded, so a program that evaluates code in a loop grows; dropping modules nothing refers to is left for later.

## Consequences

- A Scheme program can ship as one binary, built from a toolchain that needs no Rust or C compiler, only a linker.
- The toolchain grows with each strategy and target it supports. That is why only the host target's archives ship by default.
- Every strategy kept long-term must have its archives built and tested for every supported target.
- Programs that use `eval` produce much larger binaries, because they carry the front end and the expansion-time sections of every library `eval` can reach. Programs that don't use it pay nothing. Go and Rust have no `eval` at all, and GraalVM's native-image charges similarly for reflection.
- A program whose `eval` environments the build cannot work out needs them declared before it builds. That is deliberate: guessing would either bloat every such binary or fail at run time.
- A bug that appears only under one strategy shows up only in executables built for it. The test suite must run the same programs as executables under each strategy, not only inside the `vali` tool.

## Alternatives considered

- **Building the runtime for the target strategy from Rust source at `vali build` time:** no archives to ship, but every user then needs a Rust toolchain, and builds are slow.
- **One runtime archive that picks its strategy when the program starts:** fewer archives, but every store and allocation in the runtime pays an indirect call, which is what ADR 0003 avoids.
- **Executables that bundle the VM with the program's bytecode:** simple, and how many scripting languages make "executables". But nothing is compiled ahead of time and the binary always carries the VM, which defeats the purpose. It remains possible as a fallback for programs that use `eval`.

## Left open

- **The linker:** bundle one (lld, as Rust and Zig do) or use the system's.
- How archives for other targets are fetched and verified.
- Whether `vali build` also produces shared and static libraries, for embedding compiled Scheme in programs written in other languages (related to the C ABI in the design overview).
- What a standalone executable's default host grants: probably what the `vali` tool grants scripts, with build flags to restrict it (ADR 0009).
- Static or dynamic linking of the system C library, and code signing on macOS and Windows.
- Native debug information (DWARF, PDB) generated from Vali's debug information (ADR 0014), so system debuggers and profilers can map machine code back to Scheme source. Vali's own debug information is always included unless stripped.

## References

- The GHC user's guide: runtime system variants selected at link time (`-threaded`, `-debug`, `-prof`).
- The Go documentation: `go build -race`, which links a different runtime.
- .NET Native AOT: publishing an application ahead of time from its intermediate language.
