# 0012. Distributing compiled libraries

- **Status:** Accepted
- **Date:** 2026-11-16
- **Related:** 0001 (artifacts, fingerprints), 0005 (run-time bytecode), 0006 (code generation), 0009 (loaders, trust), 0010 (contracts), 0017 (the IR, link-time optimisation), 0018 (library ABI versions)

## Context

Vali is meant to be a general-purpose Scheme. Some people will want to distribute libraries without their source. Those libraries must keep working across Vali releases, because their users cannot rebuild them.

The bytecode in ADR 0005 can't serve that purpose. It is a working format, built for fast start-up: slots are resolved ahead of time, registers are numbered, opcodes have fast paths, and proven contract checks are already removed. ADR 0005 makes it private, with no compatibility promise, so that the VM can keep changing.

The JVM and the CLR solve the same problem with two levels. They ship a stable bytecode that refers to other code by name, never by layout or slot, and that is checked when loaded. Their engines don't run it as shipped: the CLR always compiles it to native code first, and HotSpot rewrites it into faster internal forms. The one place Java freezes a dependency into compiled code shows what goes wrong otherwise: `javac` inlines `static final` constants, so changing one in a library has no effect on callers until they are recompiled.

## Decision

1. **Two representations of compiled code:**
   - The **distribution format** is stable, versioned and publicly specified. It is what compiled libraries ship as.
   - The **run format** is the bytecode of ADR 0005: private, an implementation detail, made as a library loads and never stored.
2. **Converting from distribution to run format is fast:** a linear-time pass that resolves references, assigns slots, selects instructions and validates structure. Every optimisation that uses only the library's own code happens before the distribution format is written. Converting never needs whole-library analysis, so a function can convert when it is first called (ADR 0017 #10). Optimisation across libraries happens when the program is linked (item 11).
3. **Every compile goes through the distribution format.** Compiling from source produces distribution code and then converts it, so there is a single path from compiler to VM, and it is tested by every build.
4. **References between libraries are symbolic:** a library identity and a binding name, resolved when the library is linked. The distribution format never contains slot numbers, object layouts, runtime-table indexes or anything else private to one Vali version.
5. **A distribution artifact contains:**
   - its interface;
   - distribution code for run time (invoke) and for expansion time (the transformers of exported macros);
   - syntax objects with stable scope identities (ADR 0001);
   - the `cond-expand` features its expansion tested;
   - optionally, self-contained debug information (ADR 0014): function names, line tables, local variable names and macro origins, resolved so that no source file is needed, and optionally the source text itself. Distribution artifacts hold no native code (ADR 0006).

   Debug information and the names of non-exported bindings can be stripped, or debug information shipped as a separate file matched by the artifact's hash. What remains is about as hidden as Java bytecode: decompilable, but not source.
6. **Dependencies are recorded per binding used.** The one thing compiling a library freezes about its dependencies is the expansions of their macros. For each binding its code names, the artifact records that the binding exists and its kind. This includes non-exported bindings that a dependency's macros referred to on its behalf. For each dependency, it also records the ABI version it was compiled against (ADR 0018).

   When the artifact is loaded, each recorded fact and version is checked against the dependency actually present, as ADR 0018 #6 says. A dependency can change anything its users did not rely on, including adding exports, without breaking them. A mismatch refuses the load with an error naming both libraries and the binding.

   An isolate reuses a library it has already loaded only if it is the same compiled module the program was compiled against; another under the same identity is an error, not a silent link to the old instance.
7. **Distribution code never inlines across libraries.** Inlining another library's procedures, folding its constants and resolving its generic functions happen at link time, against the versions actually present (ADR 0017 #11). No implementation is frozen into anyone else's artifact, so no library needs to consent to being inlined.
8. **Versioning.** Each artifact carries a format version and lists the format features it uses. Optional sections, such as debug information, can be ignored by a loader that doesn't know them. A Vali release reads every earlier format version. A loader refuses an artifact that needs a feature it does not know, naming the feature. An artifact built by a newer Vali that uses no new features still loads on an older one.
9. **Interfaces are claims, and untrusted ones are enforced.** Distribution code is validated on load and is memory-safe however it was produced, as in ADR 0005. Its interface may still claim more than its code delivers, for example "returns an integer". Unless the host trusts the artifact's source (ADR 0009), Vali enforces those claims on the library's side: checked entry points check declared results as well as arguments, and constancy claims are checked when the library is linked.
    - **Every definition is exported, published or private, and the compiler alone tells them apart.** A library's interface records which of its definitions it exports and which of the rest it *publishes*: the private ones that code the library generates into other libraries may reference. In Scheme that is whatever a macro's template, compiled syntax or transformer's output templates can name. The compiler infers it when it builds the library, and a `publish` declaration in `define-library` adds names a walk cannot see, such as those dynamic code generation reaches. Portable code can write `(cond-expand (vali (publish helper)) (else))`. The word is language-neutral on purpose: another front end publishes what its inlining exposes, as Kotlin's `@PublishedApi` and Swift's `@usableFromInline` do.
    - **Private definitions are unreachable from other libraries' code; exported and published ones are linkable.** A link to a variable of another library, from a library's code or a transformer's, is allowed if that library exports or publishes the name, and an artifact with any other link is refused when it is loaded, naming the library and the variable. The linker checks only that: the IR carries no marks of where a link came from, and at run time a published definition is as public as an exported one. It applies to Vali's own libraries as it does to the rest.
    - **The compiler enforces the API.** Source code reaches only the names a library exports, which scoping already guarantees. A macro reaches what its library published, so one whose expansion would name a definition its library did not publish, as one from an artifact that predates or bypasses inference can, is a compile error naming the definition and suggesting `publish` or `export`.
    - **A library's export is held to what the library owns.** Its interface binds no variable of another library that library does not export. Its syntax export binds such a variable only as importing that library brought it in: under the name that library's own syntax export has, and under scopes of the same kinds in the same number, and never under this library's own scopes. Neither binds an isolate global (a REPL's variable) or, unless the library is Vali's own, an internal primitive, and the library's code and transformers link no isolate global. An artifact that does is refused when it is loaded. Nor does a library's code link an internal primitive, unless the library is Vali's own, except the few that the macros of Vali's own libraries expand into (record types) and the three that compiled syntax lowers to, `%quote-syntax`, `%syntax-match` and `%syntax-build`, which `#'` and `syntax-case` emit in procedures as in transformers. Those three look up the unit they are given in tables that hold every loaded library's, so the code and the transformers of a library that is not Vali's own may use them only as the expander emits them: as the callee of a direct call whose unit argument is a string constant naming the library's own unit. A library that passes one on as a value, or calls it with any other unit, is refused when it is loaded. A crafted artifact can bind a private variable of another library under a scope that passes the syntax export's check, but it cannot make the binding reach it: the variable is not published, so the loader refuses any link to it and the expander refuses any reference.
10. **Run format is never stored.** Loading a distribution artifact decodes and validates its code and converts a function when it is first called (ADR 0017 #10), so a typical program, which calls a small share of the functions it loads, pays for converting only those. Nothing caches the result: conversion is cheap enough once most of it is skipped, and a cached copy would put code that had already been validated on disk, inside the security boundary (ADR 0017 #13). The standard libraries' run format is not embedded in the binary either.
11. **Linking optimises across libraries** (ADR 0017 #11): it inlines, folds, resolves dispatch sites and chooses unchecked entry points with every library actually loaded, on a background thread, while tier 0 runs each library's own code. Every such decision is proven, and a library loaded later that invalidates one sends the affected code back to its per-library version. The linker's output is IR, validated and converted on first call like the rest, and anything kept between runs is kept as that IR, never as run format (item 10).

## Consequences

- Closed-source libraries work across Vali releases, and the VM's bytecode stays free to change.
- Loading a distributed library decodes and validates its code and converts its body, and every other function converts on its first call, so the cost of conversion follows what a program runs and not what it loads.
- The compiler splits into three parts: one from source to distribution code, which optimises each library on its own; one from distribution code to run format, which is linear; and the linker, which optimises across libraries in the background. Optimisations that would need run-format details must be expressible before conversion, or done by the JIT.
- The distribution format needs a written specification and compatibility tests, from milestone M1 on. Every Vali release must keep reading old artifacts, so CI keeps a corpus of artifacts from earlier versions.
- A library's macros used to compile a closed library are frozen into it, like a C header. Fixing a bug in the macro means rebuilding the library that used it.
- Library authors keep ABI versions (ADR 0018), but never need to decide what may be inlined.

## Alternatives considered

- **No closed distribution (source always required):** simplest, but it rules out a use the language should support.
- **Run-format bytecode locked to one Vali version** (Racket's binary packages, OCaml, GHC): cheap, but closed libraries break on every release.
- **Making the run-format bytecode stable:** one format and no conversion step. But it freezes slot assignment, register conventions and the opcode set, so every VM optimisation becomes a format change. Consumers would also have to trust contract proofs already compiled into the code.
- **Shipping source-level core forms:** the most stable option. But turning them into run format needs the whole optimising half of the compiler at load time, which breaks rule 2.

## Left open

- The encoding: a tree of optimised core forms or a stack-based bytecode. Either works if it meets rules 2 and 4.
- When a contract counts as compatible, beyond being identical to the one relied on.
- Signing distribution artifacts.

## References

- The Java Virtual Machine Specification: the class file format and linking.
- ECMA-335, the Common Language Infrastructure; .NET ReadyToRun images.
- The Swift documentation: library evolution, module interfaces and `@inlinable`.
- The Racket documentation: machine-independent compiled code.
