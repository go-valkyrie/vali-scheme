# 0012. Distributing compiled libraries

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, no native code in artifacts; native code reaches disk only through `vali build` (ADR 0006)
- **Amended:** 2026-11-16, debug information is self-contained: usable without the source files
- **Related:** 0001 (artifacts, fingerprints), 0005 (run-time bytecode), 0006 (code generation), 0009 (loaders, trust), 0010 (contracts)

## Context

Vali is meant to be a general-purpose Scheme. Some people will want to distribute libraries without their source. Those libraries must keep working across Vali releases, because their users cannot rebuild them.

The bytecode in ADR 0005 can't serve that purpose. It is a working format, built for fast start-up: slots are resolved ahead of time, registers are numbered, opcodes have fast paths, and proven contract checks are already removed. ADR 0005 makes it private, with no compatibility promise, so that the VM can keep changing.

The JVM and the CLR solve the same problem with two levels. They ship a stable bytecode that refers to other code by name, never by layout or slot, and that is checked when loaded. Their engines don't run it as shipped: the CLR always compiles it to native code first, and HotSpot rewrites it into faster internal forms. The one place Java freezes a dependency into compiled code shows what goes wrong otherwise: `javac` inlines `static final` constants, so changing one in a library has no effect on callers until they are recompiled.

## Decision

1. **Two representations of compiled code:**
   - The **distribution format** is stable, versioned and publicly specified. It is what compiled libraries ship as.
   - The **run format** is the bytecode of ADR 0005: private, an implementation detail, held only in the cache.
2. **Converting from distribution to run format is fast:** one linear pass that resolves references, assigns slots, selects instructions and validates structure. Every optimisation needing analysis across a function happens before the distribution format is written. Converting never needs whole-library analysis.
3. **Every compile goes through the distribution format.** Compiling from source produces distribution code and then converts it, so there is a single path from compiler to VM, and it is tested by every build.
4. **References between libraries are symbolic:** a library identity and a binding name, resolved when the library is linked. The distribution format never contains slot numbers, object layouts, runtime-table indexes or anything else private to one Vali version.
5. **A distribution artifact contains:**
   - its interface;
   - distribution code for run time (invoke) and for expansion time (the transformers of exported macros);
   - syntax objects with stable scope identities (ADR 0001);
   - the `cond-expand` features its expansion tested;
   - optionally, self-contained debug information (ADR 0014): function names, line tables, local variable names and macro origins, resolved so that no source file is needed, and optionally the source text itself. Distribution artifacts hold no native code (ADR 0006).

   Debug information and the names of non-exported bindings can be stripped, or debug information shipped as a separate file matched by the artifact's hash. What remains is about as hidden as Java bytecode: decompilable, but not source.
6. **Dependencies are recorded per binding used.** Compiling a library freezes some things about its dependencies into it: the expansions of their macros, any bodies it inlined, and the contracts and constancy it relied on. For each binding it used, the artifact records what it relied on. This includes non-exported bindings that a dependency's macros referred to on its behalf.

   When the artifact is loaded, each recorded fact is checked against the dependency actually present. A dependency can change anything its users did not rely on, including adding exports, without breaking them. A mismatch refuses the load with an error naming both libraries and the binding.
7. **Inlining across libraries needs consent.** Code is inlined from another library only if that library marks it inlinable, as Swift's `@inlinable` does. Otherwise its implementation is not frozen into anyone else's artifact.
8. **Versioning.** Each artifact carries a format version and lists the format features it uses. Optional sections, such as debug information, can be ignored by a loader that doesn't know them. A Vali release reads every earlier format version. A loader refuses an artifact that needs a feature it does not know, naming the feature. An artifact built by a newer Vali that uses no new features still loads on an older one.
9. **Interfaces are claims, and untrusted ones are enforced.** Distribution code is validated on load and is memory-safe however it was produced, as in ADR 0005. Its interface may still claim more than its code delivers, for example "returns an integer". Unless the host trusts the artifact's source (ADR 0009), Vali enforces those claims on the library's side: checked entry points check declared results as well as arguments, and constancy claims are checked when the library is linked.
10. **The cache sits behind both kinds of input.** Loading a distribution artifact converts it and caches the result, keyed by the artifact's hash and the Vali version, so warm runs read run-format bytecode either way.

## Consequences

- Closed-source libraries work across Vali releases, and the VM's bytecode stays free to change.
- The first load of a distributed library on a new Vali version pays for one conversion pass. Later loads hit the cache.
- The compiler splits into two halves: one from source to distribution code, which does the optimising, and one from distribution code to run format, which is linear. Optimisations that would need run-format details must be expressible before that point, or done by the JIT.
- The distribution format needs a written specification and compatibility tests, from milestone M1 on. Every Vali release must keep reading old artifacts, so CI keeps a corpus of artifacts from earlier versions.
- A library's macros used to compile a closed library are frozen into it, like a C header. Fixing a bug in the macro means rebuilding the library that used it.
- Library authors must think about which bindings to mark inlinable, as Swift authors do.

## Alternatives considered

- **No closed distribution (source always required):** simplest, but it rules out a use the language should support.
- **Run-format bytecode locked to one Vali version** (Racket's binary packages, OCaml, GHC): cheap, but closed libraries break on every release.
- **Making the run-format bytecode stable:** one format and no conversion step. But it freezes slot assignment, register conventions and the opcode set, so every VM optimisation becomes a format change. Consumers would also have to trust contract proofs already compiled into the code.
- **Shipping source-level core forms:** the most stable option. But turning them into run format needs the whole optimising half of the compiler at load time, which breaks rule 2.

## Left open

- The encoding: a tree of optimised core forms or a stack-based bytecode. Either works if it meets rules 2 and 4.
- When a contract counts as compatible, beyond being identical to the one relied on.
- Signing distribution artifacts.
- How libraries mark bindings inlinable, and how versions of a library are named for error messages.

## References

- The Java Virtual Machine Specification: the class file format and linking.
- ECMA-335, the Common Language Infrastructure; .NET ReadyToRun images.
- The Swift documentation: library evolution, module interfaces and `@inlinable`.
- The Racket documentation: machine-independent compiled code.
