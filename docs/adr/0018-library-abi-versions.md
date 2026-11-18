# 0018. Library ABI versions

- **Status:** Accepted, not built
- **Date:** 2026-11-18
- **Amended:** 2026-11-18, after review: a procedural macro's reach through `datum->syntax` cannot be listed, so templates define the macro part of the ABI and per-binding facts catch the rest (items 1 and 3)
- **Related:** 0001 (interface sections), 0006 (Vali's own ABI version), 0010 (contracts), 0012 (distribution, per-binding facts), 0017 (link-time optimisation)

## Context

A distributed library is linked against whatever version of each dependency is present when it loads (ADR 0017). Per-binding facts (ADR 0012 #6) catch a dependency that no longer has a binding the importer names, but not one that changed what the binding means: a procedure that now takes its arguments in another order, a contract tightened, a constant made assignable. Ecosystems that leave this to convention break silently. A library's author bumps a version, or forgets to, and nothing checks either. Elm's package manager and .NET's API compatibility tools show that the toolchain can check it instead: diff the new interface against the last published one, and refuse a breaking change that isn't declared as one.

"ABI version" here means a library's own. Vali's runtime ABI version (ADR 0006) is a different thing: it says which Vali build produced native code and run-format bytecode.

## Decision

1. **A library's ABI is its interface as importers rely on it:**
   - its exports, and each one's kind: variable, procedure, macro, record type or generic function;
   - each procedure's arity, and its contract or signature (ADR 0010);
   - whether each variable is constant;
   - the private bindings named in its exported macros' templates, which other libraries' frozen expansions name even though they are not exported. A procedural macro can reach other private bindings through `datum->syntax`; those cannot be listed in advance, and importers' per-binding facts catch their removal at link time (item 6);
   - the record types and generic functions it exports, and the methods it adds to other libraries' generic functions.
2. **A library declares its ABI version in `define-library`,** as a major and a minor number: `(define-library (util) (abi-version 2 1) (export …) …)`. Other Schemes can be kept from seeing it with `(cond-expand (vali (abi-version 2 1)))`, which is a valid library declaration in R7RS.
3. **The toolchain classifies every change to the ABI** as compatible or breaking:
   - **compatible:** adding an export; widening an arity; loosening an argument's contract or tightening a result's; making an assignable variable constant; adding a method on a type the library owns;
   - **breaking:** removing or renaming an export, or a private binding named in an exported macro's template; changing a binding's kind; narrowing an arity; tightening an argument's contract or loosening a result's; making a constant assignable; changing a record type's fields.
4. **Compiling a library for release checks its declared version against the last published interface.** A breaking change without a new major version, or a compatible change without at least a new minor version, is a compile error naming the binding and the change.
5. **Each importer records, for each dependency, the ABI version it was compiled against,** next to its per-binding facts (ADR 0012 #6).
6. **Linking requires both:**
   - the dependency's major version equals the recorded one, and its minor version is at least the recorded one;
   - every per-binding fact the importer recorded still holds.

   Either failing stops the link with an error naming the importer, the dependency, both versions, and the first binding or rule that failed. The version check gives the clear message; the facts catch a dependency whose declared version is wrong, for example one published without the check in item 4.
7. **A library without an `abi-version` is unversioned.** Libraries compiled from source together, such as a program's own and the REPL's, need none: the artifact cache already rebuilds importers when an interface changes (ADR 0001 #8). Unversioned libraries link on per-binding facts alone. A distribution artifact meant for others must declare a version.

## Consequences

- Breaking a distributed library's ABI fails at link time with a message that says what broke, instead of misbehaving at run time.
- Library authors must keep versions, and their release builds need the last published interface to compare against.
- Importers do not have to recompile for a compatible release.
- The interface section records the ABI version (ADR 0001 #5, as amended), and so does each importer's dependency record.

## Alternatives considered

- **Versions computed from a hash of the interface:** they cannot be wrong, but they aren't readable, and every change, compatible or not, makes a new one unless compatible changes are classified anyway.
- **Versions declared and trusted:** simplest, and the mistake every ecosystem without a checker has made.
- **Exact version matches at link time:** strict, but every compatible release forces every importer to recompile.
- **Per-binding facts alone** (ADR 0012 #6 before this record): catches a missing binding, but not a changed meaning, and its errors cannot say which release broke what.
- **Versions in the library name, as R6RS does** (`(util (2 1))`): there is precedent, but it is ambiguous with R7RS names that end in integers, such as `(srfi 1)`.
- **Versions in package metadata**, as Cargo and NuGet keep them: a bare library file would then have none.

## Left open

- Where a release build finds the last published interface: a file passed on the command line, a package registry, or both.
- Whether major version 0 means "unstable", where any minor release may break, as semantic versioning has it.
- Which contract changes count as loosening or tightening beyond identical contracts, which ADR 0012 also leaves open.
- Importing a library by version range.

## References

- The Elm package manager: `elm diff` and enforced semantic versioning.
- The .NET documentation: API compatibility validation of packages.
- Semantic Versioning 2.0.0.
- R6RS, section 7.1: library versions.
