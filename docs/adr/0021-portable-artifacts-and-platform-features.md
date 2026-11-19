# 0021. Portable artifacts and platform features

- **Status:** Accepted, not built
- **Date:** 2026-11-19
- **Related:** 0001 (amended by this record: cache validity, feature identifiers), 0009 (capabilities), 0012 (distribution artifacts), 0015 (phases, expansion), 0022 (the lint framework, which carries this record's warning)

## Context

A compiled library should run wherever the build that made it runs. Anything that decides on the platform while a library is being *expanded* breaks that: the decision is baked into the artifact, and the artifact is wrong on a machine with other values.

Three things decide on the platform at expansion time:

- `cond-expand` on an operating-system, architecture or byte-order identifier, in a library's body or in the code a macro expands into. It is always resolved while expanding.
- A call to `(features)` made by expansion-time code: a transformer, or any procedure a transformer calls, however deep.
- A macro from another library that does either, used while expanding this one.

Calling `(features)` at run time is harmless. The artifact stays neutral and decides when it runs.

`(features)` and `cond-expand` report one list (ADR 0001, feature identifiers). It mixes three kinds of identifier:

- **Language and version:** `r7rs`, `ratios`, `exact-complex`, `full-unicode`, `ieee-float`, `vali`, `vali-0.0.0`. They are fixed by the build, which the artifact's build stamp already pins (ADR 0001 #7).
- **`srfi-N`:** derived from the libraries that ship, so also fixed by the build stamp.
- **Platform:** the operating system (`posix`, `unix`, `darwin`, `linux`, `windows`, ...), the architecture (`x86-64`, `aarch64`, `wasm32`, ...) and the byte order (`little-endian`, `big-endian`). These differ between machines that could share an artifact.

ADR 0001 left "features tested by `cond-expand` in a library body" open, and item 8 meant to fingerprint them, but nothing records them yet. The feature-identifiers change closed the gap for Vali's own libraries by banning `cond-expand` and `(features)` in their sources, which a loader test checks. That is a blunt rule: it also forbids a `cond-expand` on `srfi-N`, which is portable.

## Decision

1. **Artifacts are portable by default.** A platform-specific artifact is the flagged exception, and the flag is the record in item 3.
2. **Platform identifiers are the operating system, the architecture and the byte order, and nothing else.** Every other identifier is a build identifier, pinned by the build stamp, or `r7rs` and `srfi-N`, which are portable. Testing those never makes an artifact non-portable. `vali_runtime::features()` and the compiler's list are split along this line, so the expander can tell the kinds apart.
3. **The expander records what it learned about the platform.** While it expands a unit, it notes each platform identifier that was tested and whether the platform has it. Two things count as a test:
   - a `cond-expand` that names a platform identifier (anywhere in a requirement, `and`, `or` and `not` included), whether in a library declaration or a body;
   - every call to `(features)` made while expanding, however deep in phase-1 code. The hook is the source of feature values, not the call site, so a procedure that a transformer calls through three libraries is caught. A call records the platform's whole set of identifiers.

   The record belongs to the library being expanded, not to the library whose code asked. An empty record means the artifact is portable.
4. **The artifact stores the record.** It is a section of the artifact (ADR 0012 #5 already lists the features an expansion tested), holding each tested identifier and whether it held, plus the whole platform set if `(features)` was called. Adding it changes the distribution format, so its version goes up (ADR 0012 #8). It is part of what the artifact's hash covers, so an importer that depends on the artifact (ADR 0001 #7, the `depends` section) sees a different artifact for a different record.
5. **The loader refuses a non-portable artifact on a platform whose values differ.** It compares each recorded identifier, and the whole set if one was recorded, with the platform it runs on. A mismatch is an error naming the library and the identifier, like the other validity checks. Where the source is available, the host compiles from it instead, as it does for any artifact that does not fit. A portable artifact has nothing to compare and loads anywhere its build stamp matches.
6. **Imported macros record on the importer.** If library B's macro tests the platform while A is expanded, the test ran during A's expansion, so A's record holds it, and B's artifact does not change. The warning (item 9) names both A's use site and B's macro. The same holds for B's phase-1 code that runs when A's expansion instantiates B (ADR 0001 #2).
7. **Run-time-only imports do not propagate.** If A imports a platform-specific library C and uses it only at run time, A stays portable: C's own artifact carries its record, and loading C is checked by item 5. Nothing about A's expansion depended on the platform.
8. **`(vali platform)` is the run-time alternative.** It exports `platform-os`, `platform-arch` and `big-endian?`, plus whatever else the run-time decision needs (the list is settled when it is built). They are ordinary procedures, evaluated when the program runs, so the artifact stays neutral. A macro that wants a platform decision at run time expands into a call to one of them, or to `(features)`, which is not tracked at run time.
9. **The lint.** The compiler warns, with the lint `platform-features` (ADR 0022), for each place that adds to the record: the `cond-expand` or `(features)` call, and for an imported macro both the use site in the importing library and the macro in the library that tested. The message suggests testing at run time, with `(vali platform)` or `(features)`. A library that is platform-specific on purpose declares `(allow platform-features)`, which silences the warning. It does not touch the record or the refusal in item 5.
10. **Vali's own libraries must stay portable.** This replaces the blanket ban on `cond-expand` and `(features)` in their sources. `vali-stdlib`'s build script fails if any artifact it builds has a non-empty record, and a test over the embedded artifacts checks the same. A `cond-expand` on `srfi-N` or `r7rs` is allowed, and so is `(features)` in run-time code.
11. **Units without artifacts** (a program, a REPL input, `eval`) get the warning but store no record, since nothing is reused later.

## Consequences

- A cache, a distributed artifact or a prebuilt standard library that was made on one platform cannot silently run on another with different choices baked in.
- The common case costs nothing: a library that never tests the platform has an empty record and loads anywhere.
- Hosts that build on one machine and run on another can read the record to learn which artifacts need rebuilding.
- The expander gains one hook, on the source of feature values, and one set of observations per unit. Both are cheap.
- A platform-specific library is still possible and visible: it carries a non-empty record, and its source says `(allow platform-features)`.
- The record leaves the build-identifier features pinned by the stamp. If a build identifier ever varies between builds that share a stamp, it needs to move into the platform set.
- Vali's own libraries may now use `cond-expand` on `srfi-N`, and the loader test that forbade every `cond-expand` goes away.

## Alternatives considered

- **Warn only:** the artifact would still carry the platform's choices, and on another machine the failure would show up as wrong behaviour, not an error.
- **Forbid platform tests in libraries:** safe, but a library that wraps a platform's file paths or calling convention has no way to exist, and programs would need another mechanism for the same thing. Platform-specific artifacts are legitimate; they just must not be mistaken for portable ones.
- **Opt-in portability** (a declaration promises portability, and the expander checks it): most libraries would never declare it, so most artifacts would carry no guarantee, and a missing declaration would look like a platform-specific library.
- **Propagating imports** (A is platform-specific if anything it imports is): most programs import something with a platform split, such as an I/O library, so every library would end up non-portable. The run-time use does not depend on the expansion, and the imported library's own record already refuses it where it does not fit.
- **Tracking only `cond-expand` and direct `(features)` calls:** misses a transformer that reaches `(features)` through a helper library, which is the case most likely to go unnoticed.
- **Treating `full-unicode`, `ratios`, `exact-complex` and `vali-0.0.0` as platform identifiers:** they describe the build, and the stamp pins the build already. Recording them too would refuse artifacts for no reason.

## Left open

- The record's encoding in the artifact, and the new format version.
- The full list of `(vali platform)` exports.
- A packaging step (the planned `vali build`) that refuses to publish a non-portable artifact unless asked.
- Whether a library may declare a platform requirement instead of hitting the refusal, so a host can choose the right artifact up front.
- Whether a cache with one slot per library can hold the artifacts of several platforms at once.

## References

- ADR 0001, cache validity and feature identifiers.
- R7RS appendix B, feature identifiers.
