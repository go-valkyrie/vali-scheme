# 0006. Relocatable code and ABI versioning

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, the code generator also writes platform object files for standalone executables (ADR 0013); the memory-management strategy is part of the ABI version
- **Amended:** 2026-11-16, native code reaches disk only through `vali build`; JIT-compiled code is never cached between runs, and artifacts have no native sections
- **Amended:** 2026-11-18, the ABI version covers the page size, the page header and its `frozen` field, and the sticky count (item 4; ADR 0020)
- **Related:** 0002 (tags), 0003 (barriers and safepoints), 0004 (frames), 0005 (bytecode), 0008 (isolates), 0009 (trust), 0013 (standalone executables)

## Context

Native code comes from two places. The baseline JIT compiles hot functions while a program runs, and `vali build` compiles whole programs ahead of time into standalone executables (ADR 0013). Both should come from one code generator, or the tiers will drift apart. JIT-compiled code may be shared by several isolates (ADR 0008), each with its own data.

Generated code also bakes in assumptions about the runtime: value tags, object layouts, the frame layout, the signatures of runtime functions and the memory-management strategy. If any of those change, old native code is wrong in ways no run-time check would catch.

Runtimes differ on whether JIT-compiled code is kept between runs. HotSpot never did, and recompiles in each process. .NET Framework kept a machine-wide cache of native images (NGen), but .NET Core dropped it in favour of code compiled ahead of time into the application (ReadyToRun) plus tiered JIT compilation. OpenJ9 caches relocatable compiled code, and HotSpot's Project Leyden is moving towards caching more of what a run computes. A cache of native code has a price: the runtime must load native code from files, decide whether to trust them, and detect and skip stale ones.

## Decision

1. **Generated code contains no absolute addresses.** Runtime functions are called through a table, and constants are loaded from the module's constant pool. Both are reached through a context pointer that every generated function receives, as Wasmtime passes its `vmctx`. Code refers to other code only by PC-relative offsets within the same unit, or through tables.
2. **The JIT and `vali build` share one code generator,** built on Cranelift. They differ only in where the code goes: into executable memory, or into platform object files that the linker turns into an executable or library (ADR 0013). The generator takes the target CPU and the memory-management strategy (ADR 0003) as parameters, so one `vali` tool can generate code for any of them.
3. **Native code reaches disk only through `vali build`.** JIT-compiled code lives for one process and is never cached between runs. Artifacts hold no native code (ADR 0001), and the runtime never loads native code from a file.
4. **An ABI version covers everything generated code assumes:**
   - the value tags (ADR 0002);
   - the layouts of objects that generated code accesses directly;
   - the frame layout, stack-map format and segment-boundary handling (ADR 0004);
   - the order and signatures of the runtime function table;
   - the layout of the context pointer's target;
   - the safepoint and write-barrier protocols (ADR 0003);
   - the calling convention;
   - the memory-management strategy (ADR 0003), whose barriers, count operations and allocation fast paths are inlined into generated code;
   - the page size and alignment, the header word at the start of every page that can hold a pair, with its `frozen` field, which generated code reads before it writes a pair, and the sticky count, which counting code inlined into generated code tests (ADR 0020).

   Any change to these bumps the version. Run-format bytecode in the cache records it (ADR 0005), and `vali build` refuses to link object files with a runtime archive whose ABI version, target or strategy differs (ADR 0013).
5. **Executable memory is never writable at the same time (W^X).** Code is written first, then switched to read and execute.

## Consequences

- Because per-instance data comes through the context pointer and constant pools, one copy of JIT-compiled code can serve several isolates (ADR 0008).
- Every runtime call and constant load costs one more indirection than code patched in place. Tier 1 is a baseline compiler that already makes many runtime calls, so this cost should be small.
- Each process warms up its JIT from scratch. Cold start is served by the bytecode cache and tier 0, and short runs rarely leave tier 0.
- The runtime never trusts native code from a file. Sandboxing untrusted code concerns only bytecode, which is safe by construction (ADR 0005).
- Platforms that forbid generating code at run time run tier 0, or programs are built for them as standalone executables (ADR 0013).

## Alternatives considered

- **Patching absolute addresses at load time (relocations):** slightly faster code, but no sharing of JIT code between isolates.
- **Separate code generators for the JIT and for `vali build`:** each could be tuned to its job, at the cost of two sets of bugs.
- **Caching JIT-compiled code between runs** (.NET Framework's NGen images, OpenJ9's shared cache): faster warm-up for long-running processes, but the runtime would load native code from files, which brings trust rules, signing and stale-code detection. The bytecode cache already covers cold start.

## Left open

- The form of the ABI version: a single integer, or a hash of the definitions it covers.
- The baseline CPU features per target, for example x86-64-v2 or v3.
- Caching JIT-compiled code or profiles between runs, revisited only if measurements show warm-up dominates for real hosts. HotSpot's Project Leyden (JEPs 483 and 515) is the model to follow if so.

## References

- The Cranelift and Wasmtime documentation: tail calls and the `vmctx` parameter.
- .NET ReadyToRun images and tiered compilation.
- JEP 483, *Ahead-of-Time Class Loading & Linking*; JEP 515, *Ahead-of-Time Method Profiling*.
