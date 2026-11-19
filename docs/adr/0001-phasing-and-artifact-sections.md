# 0001. Phasing and artifact sections

- **Status:** Accepted
- **Date:** 2026-11-16
- **Related:** 0006 (native code), 0009 (loaders and expansion-time access), 0010 (contracts in interfaces), 0017 (link-time optimisation), 0018 (library ABI versions), 0021 (portable artifacts and platform features)

## Context

Vali must start fast for short CLI runs, so compiled libraries are cached and reused. A cached result is safe to reuse only if expanding and compiling a library is deterministic: the output must depend on nothing but the library's source and its dependencies. In a Scheme with procedural macros, two things usually break that:

1. **State shared across phases.** If a library's expansion-time instance is also its run-time instance, as in Chez Scheme, a macro can leave state behind. What one library expands to then depends on what was expanded before it.
2. **Unrecorded inputs.** A macro that reads a file, the clock or the environment produces output that a cache key built from source alone does not capture.

Macros must also be exportable from libraries, procedural (`syntax-case`) ones included. Steel exports only `syntax-rules` macros, so any form defined by a procedural macro would need a separate expansion step.

R6RS lets an import say at which phase it is needed (`(import (for (lib) expand))`). R7RS has no such syntax, and most programmers never want to write it.

## Decision

### Phasing

1. **Phasing is implicit by default.** An import makes a library available at every phase where its bindings are referenced. The expander instantiates the library at a phase the first time it needs it there. R6RS `for` import levels are accepted as an explicit override: such an import is available only at the levels it lists.
2. **Each phase gets its own instance of a library.** Run-time state and expansion-time state are never shared. Expanding a library starts from fresh expansion-time instances of its dependencies, so it cannot observe effects left behind by expanding another library. This is Racket's separate compilation guarantee.

   Each unit (library, program, REPL input or `eval`) expands with its own isolate for each phase, made on first use, with fresh instances of what it needs, and dropped when the next unit starts. A transformer written in Scheme is kept as compiled code, which each unit's isolate runs afresh. Expansion-time isolates count references, deferred, so they free as they go.
3. **Static information on bindings** (Chez's `define-property`, Racket's `syntax-local-value`) is serialisable data: syntax objects, symbols, numbers, strings, and lists, vectors and records of those. It cannot hold procedures or other run-time values.
4. **Expansion is deterministic.** Fresh names, scope identities and any iteration order that reaches the output come from the library being expanded, never from global counters, addresses or the time.

   Fresh names and the indices of compiled patterns, templates and quoted syntax are numbered within each unit, and code names its unit beside each index; a `cond-expand` in a body asks the loader, before expansion, whether a library exists. Each unit also expands in a scope context of its own (ADR 0015 #5).

   Expansion-time code can only reach the outside world through capabilities and bridges (ADR 0009), and its own state starts fresh for each library (item 2). What remains is non-determinism inside the language runtime itself, which ordinary Scheme code can observe with no I/O at all. The runtime closes each such leak:
   - **Hash tables iterate in insertion order.** Neither a per-process hash seed nor an `eq?` table's address-based hashing can change the order. Seeds may still be randomised against hash flooding, since the order no longer depends on them.
   - **The printer never shows memory addresses or scope identities.** Opaque objects print without an address. Syntax objects print without scopes, which are renumbered when a visit section is loaded from the cache (item 7), so showing them would make cold and warm builds differ.
   - **Source identities are machine-independent.** A macro can embed a source location in its output, as an `assert` message does. Loaders therefore give identities relative to a root, never absolute machine paths (ADR 0009).
   - **Garbage collection at expansion time is triggered only by the volume allocated,** so weak tables and guardians behave the same on every build of the same library.
   - **Inexact functions such as `sin`, `exp` and `log` use one portable implementation,** not each platform's maths library, whose results can differ in the last bit.
   - **Output written during expansion is stored in the artifact and replayed** when the artifact is reused, so a macro's warnings don't vanish on a cache hit. Cargo does the same with cached compiler warnings. Not built yet (ADR 0022).

### Artifacts

5. A compiled library is an artifact with three sections:

   | Section | Contains | Read when |
   |---|---|---|
   | Interface | Exports and binding kinds (variable or macro); whether each variable is constant; static information; declared contracts and signatures; the library's ABI version (ADR 0018) | An importer is expanded or compiled |
   | Visit | Compiled transformers and other expansion-time code | An importer's expansion uses one of this library's macros |
   | Invoke | Run-time definitions and the library body | The library runs |

6. **Visit sections load lazily.** The expander reads one only when it meets one of that library's macros. A warm run reads only invoke sections. Artifacts never hold native code: JIT-compiled code is not cached between runs, and native code reaches disk only through `vali build` (ADRs 0006 and 0013).
7. **Syntax objects keep stable scope identities when stored.** Syntax objects in visit sections and static information are serialised with stable scope identities, which are remapped to fresh scopes on load.

   A library's artifact has six required sections: `build` (the stamp of the build that wrote it), `source` (a hash of what was read to compile it, or none), `library` (its identity, imports and interface), `syntax` (its syntax export), `invoke` (its module as distribution code) and `depends` (see below). Visit code is in `syntax`, with each transformer written in Scheme as distribution code; it is read whole, not lazily. The syntax export is stored as an importer would see it: remapped into a fresh context, so only the scopes it reaches are written, numbered in the order they are met, with the text of every source its spans point into. A library that is not one of Vali's own may not link the internal `%` primitives, whatever its artifact says.

   The checks made at load time are these, and a library that fails one is refused with an error naming it, never used:
   - **The build stamp** must equal this build's. It holds the compiler's version, the distribution format version, the runtime ABI version (`vali-abi`), and a hash of the source files of the compiler and the crates beneath it (reader, optimiser, format, bytecode, runtime, numbers) and of Vali's own Scheme libraries. The compiler's build script computes it from sorted, workspace-relative paths, so identical sources give the same stamp on any machine, and any edit to them gives another. A compiler built outside this workspace, where those sources are not beside it, is stamped by its own sources and version alone.
   - **The source hash**, when the caller has read the source: a SHA-256 of the library's text, each file its declarations include (in order, with the path), what its declaration-level `cond-expand`s found about libraries, and each file its body includes (sorted by path). A library whose source was not read, which is a standard library loaded from its prebuilt artifact, is vouched for by the build stamp instead, since that hashes the sources the artifact was built from. An artifact that records no source is refused when a source was read.
   - **The `depends` section** has two lists, each entry an identity and a SHA-256. The first is each import and a hash of that import's `library` and `syntax` sections. The second is each library whose code ran while this one was expanded (instantiated at any phase), and a hash of that library's whole artifact, which covers what it relied on in turn. Every entry must name a library that is loaded, with the same hash. The first list must also name exactly the libraries the `library` section says are imported, in the same order.

   Not built yet: recording the library availability that `cond-expand` in a body tested, per-binding facts (ADR 0012 #6), and expansion-time reads through capabilities (ADR 0009). The platform identifiers a `cond-expand` or `(features)` call observed while expanding go in a section of their own, which the loader checks against the platform it runs on (ADR 0021 #3 to #5); that section is not built yet either.

   The distribution format encodes each primitive by its name, not by its place in `Primitive::ALL`, so the order of that list is not part of the format, and removing a primitive makes old artifacts fail to decode with its name in the error. Hashes are SHA-256, written out in `vali-dist` to keep the format's dependencies at none; item 9's content hashes are these.

   Writing an artifact again from one loaded gives the same bytes. Setting `VALI_ROUND_TRIP` writes, drops and reloads every library as it is compiled, so the test suite can run on what artifacts hold.

   A library's `syntax` section holds only its own macros and compiled syntax, and the bindings they reach; its imports' macros come from their own exports, which importing it imports too. Without this, every library that imports `(scheme base)` carried a copy of base's macros and of `base.sld`, about 82 KB each, and loading one cost more than compiling it.

   The `vali-stdlib` crate's build script compiles the standard libraries and embeds their artifacts; the CLI and the test harness install them. Each is loaded from its artifact unless a host serves its own version, or the artifact does not fit what it was compiled against, when it is compiled from source. The import graph is read from the artifacts too: a standard library whose artifact this build can load is never read from its source; what it imports comes from its `depends` section, read once per process. Vali's own libraries include no files and test no libraries in `cond-expand`, so their sources hold nothing more the graph needs. If such an artifact then does not fit, the library's source is read when it is compiled. This also spares every `eval` and REPL input, which fetch their import graph afresh, from reading `base.sld` again. In a release build, compiling a program that imports `(scheme base)` takes about 2.3 ms in a warm process; of that, converting base's distribution code to bytecode is about 1 ms. The `vali` REPL, as a whole process, starts in about 16.5 ms; about 6 ms of that is starting the executable at all. `cargo run --release -p vali-stdlib --example startup` measures these.

   **Feature identifiers.** `(features)` and `cond-expand` report the same list: the language (`r7rs`, `ratios`, `vali` and the rest), the version (`vali-0.0.0`), the operating system, architecture and byte order of the build (R7RS appendix B: `posix`, `unix`, `darwin`, `linux`, `windows`, `x86-64`, `aarch64`, `little-endian`, and so on), and `srfi-N` for every number `N` that a `(srfi N ...)` library among Vali's own has. The `srfi-N` list is derived from the library table, so it cannot drift from what ships. Only the operating system, architecture and byte order are platform identifiers; the rest are fixed by the build stamp (ADR 0021 #2). Vali's own libraries must stay portable (ADR 0021 #10): a `cond-expand` on `srfi-N` is allowed, and the record of platform identifiers checks the rest. Until that record is built, none of them may test a feature, which a unit test in the loader checks, so a prebuilt artifact holds nothing that depends on the platform it was built on.

### Cache validity

8. **An artifact records a fingerprint of everything its expansion and compilation read.** It is reused only while every fingerprint still matches:
   - the library's own source, and everything read while expanding it: files brought in by `include`, files read by macros, existence checks and directory listings (ADR 0009 defines what counts as a read);
   - for each import, a hash of its interface and visit sections, which is everything an importer's expansion or compilation can observe;
   - for each library instantiated at expansion time, a hash of its whole artifact, because its run-time code ran during expansion (that hash covers its own fingerprints, so this is transitive);
   - (not built yet) every platform feature identifier its expansion tested through `cond-expand`, and whether it was present, plus the whole platform set if any expansion-time code called `(features)`, however deep. The platform identifiers are the operating system, architecture and byte order, which differ between machines that could share an artifact; the other identifiers are fixed by the build stamp. An artifact with an empty record is portable, and the loader refuses one whose record differs from the platform it runs on (ADR 0021);
   - the compiler version and the ABI version.

   These sections are the run-time cache. Libraries distributed without source use the stable format of ADR 0012, which records its dependencies per binding instead.

9. **Fingerprints are content hashes.** A timestamp never decides validity on its own. It can only let a loader or capability skip re-hashing a resource it can show is unchanged: for example, a file whose size, modification time and inode match those recorded beside its hash, or an OCI blob named by its digest.

10. **The library cache keeps artifacts between runs.** Expanding a library from source costs several times what loading its artifact does, so `vali_compiler::compile_program_cached` (and the `vali` command, which uses it) saves the artifact of each library compiled from source and passes it back next time. Hosts opt in by making a `Cache` and calling it; `compile_program` never touches a directory, so a host that does not ask has no cache.

    - **What it holds.** Distribution-format artifacts only, never run-format bytecode: that is made again at load, a function at a time (ADR 0017 #10), so it costs little and the cache has no second format to keep valid. Only libraries a host's loader served from source it read are kept. Vali's own libraries load from the artifacts built with Vali and are never stored (the prebuilt artifact wins, and its build stamp vouches for it), nor are native libraries, nor libraries a program defines in its own file (no source hash is recorded for them).
    - **Never trusted.** Every load makes the checks of item 5, so an entry that is stale (its source changed), from another build, against a dependency that changed, damaged, or tampered with is refused. The compiler keeps refusing a bad artifact; the cache's answer to a refusal is to compile the library from source and replace the entry. The refusal is reported (`Program::cache`) and never raised, and a cache that cannot be read or written costs the run nothing but the time. An entry also carries a SHA-256 of its artifact, which catches damage before decoding; it is not a defence against someone who can write the directory, who could write any file the user runs, and the directory is made readable by its user alone where the platform can.
    - **Key.** `ROOT/<stamp>/<key>.vac`, where `<stamp>` is a hash of the build stamp (item 7's first check, so the artifacts of another build sit in a directory of their own) and `<key>` is a hash of the library's identity, its source hash (item 7's second check: text, includes, what its `cond-expand`s found) and the operating system, architecture and byte order. The platform is in the key because ADR 0021's record of the platform identifiers an expansion observed is not built yet; once an artifact carries it and the loader checks it, a mismatch is one more refusal and the platform can leave the key. What a library imports is not in the key, because it is not known before the imports are loaded; the artifact's `depends` section records it and the load checks it, so a changed dependency refuses the entry of each importer that observed the change (an importer that did not, because only the dependency's implementation changed, still loads) and the entry is replaced under the same key.
    - **Location.** Per user, since entries are content-addressed and shared by every program that has the same library: `$VALI_CACHE_DIR`, else the platform's cache directory under `vali` (`~/Library/Caches/vali` on macOS, `$XDG_CACHE_HOME/vali` or `~/.cache/vali` on Linux and other Unix, `%LOCALAPPDATA%\vali\cache` on Windows), and `--cache-dir DIR` over both. There is no per-project cache; a project that wants one points the flag or the variable at a directory of its own. With no home directory in the environment the command runs without a cache.
    - **Eviction.** Least recently used, past a size cap: `Cache::prune` deletes the entries whose modification time is oldest until the rest fit, 256 MiB by default (`VALI_CACHE_SIZE`, in MiB, changes it), after any run that wrote an entry. A read touches the entry, at most once an hour, so the time is that of last use. Entries of old builds are the oldest, so they leave first; stale partial writes older than an hour go too. `vali cache clean` deletes every entry, `vali cache stats` counts them and `vali cache dir` names the directory. Only files the cache itself names are ever deleted, so a directory that holds other things is safe to point it at.
    - **Concurrent runs.** An entry is written to a temporary file in its directory and renamed over its name, so a reader sees the old entry or the new one and never half of one, and two runs that write the same entry leave one whole entry: they compile the same bytes. Nothing locks. Eviction racing a reader costs that reader a miss.
    - **`vali build FILE`** compiles the libraries a program imports and stores their artifacts, the same ones a run would, in the cache, or with `-o DIR` in `DIR`, laid out as a cache: `vali --cache-dir DIR FILE` then loads them. Loading still reads each library's source and checks the artifact against it, so what is shipped is a warm cache beside the sources, not yet a distribution without them; that is ADR 0012's format.
    - **Cost.** A cold run pays to encode and write each artifact, a few milliseconds for seven libraries, and the warm runs after it save more than that each time. Only the expansion is saved: the import graph is still fetched, so each library's source is still read and hashed.

Items 8 and 9 refine the design overview's cache key (source, each dependency's interface hash, compiler and ABI versions). That key misses two cases: a file read at expansion time changing, and a change to a library that another library's macros call at expansion time.

## Consequences

- Expansion results can be cached by content and shared between machines. The same inputs produce byte-identical artifacts. CI checks this by building the same libraries several times and comparing bytes, varying what must not matter: build order, hash seeds, allocation patterns, address layout, and the platform. This test also catches leaks not listed under item 4.
- Changing a library's implementation without changing its interface or macros does not invalidate its importers, unless they ran its code at expansion time.
- Expansion-time state, such as a registry that macros build up, is reset for every library expanded. This is why domain knowledge belongs at run time, with macros that only transform syntax.
- A library used at several phases runs its body once per phase. A costly body used by macros adds to every cold build.
- Macros cannot pass closures to other libraries through static information. Anything a macro needs to know about another definition must be expressible as data.
- Expansion-time access to files and other resources has to go through the host and be recorded (ADR 0009).

## Alternatives considered

- **Explicit phasing only (Racket's `for-syntax`):** precise, but R7RS code cannot express it, and it is an unfamiliar burden for people writing scripts.
- **Implicit phasing with one shared instance (Chez Scheme):** each library body runs once, which is cheaper. But expansion then depends on what else was expanded in the same session, which breaks caching by content.
- **No artifact cache, expanding on every run:** simplest, but cold start was the main problem found in the evaluation.
- **Timestamps instead of content hashes (Chez Scheme):** cheaper to check, but timestamps go wrong in both directions. They miss changes that keep or restore an old modification time: archive extraction, `cp -p`, or two edits within the clock's resolution. They also force rebuilds after checkouts and copies that changed nothing, and they mean nothing on another machine. Racket has checked SHA-1 hashes alongside its timestamps since version 6.6.0.3.

## Left open

- Whether the artifact file format needs more than its version and named sections. The bytecode inside is ADR 0005.
- Whether visit and invoke sections can be stored and fetched separately, which would help hosts that fetch artifacts over a network.
- How static information is declared in source.

## References

- Matthew Flatt, *Composable and Compilable Macros: You Want It When?*, ICFP 2002.
- Abdulaziz Ghuloum and R. Kent Dybvig, *Implicit Phasing for R6RS Libraries*, ICFP 2007.
- Matthew Flatt, *Binding as Sets of Scopes*, POPL 2016.
- The Racket reference: modules, phases and linklets.
- The Racket documentation: the compilation manager and its `.dep` files.
- The Chez Scheme User's Guide, version 9.5: `compile-imported-libraries` and `maybe-compile-library`.
