# 0005. Bytecode format

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, this bytecode is the run format only; distribution uses ADR 0012
- **Amended:** 2026-11-16, bindings that are never assigned are constants or direct slots; cells only for assigned locals captured by closures
- **Amended:** 2026-11-16, debug information is self-contained: usable without the source files
- **Amended:** 2026-11-17, variables are defined or imported, and belong to library instances (item 4)
- **Amended:** 2026-11-17, every assigned local is boxed, captured or not, so re-entering a continuation shares it (item 4)
- **Amended:** 2026-11-18, instructions the IR needs: direct calls, dispatch sites with link slots, and ownership operations (item 5); constancy across libraries is used by the linker (item 4; ADR 0017)
- **Amended:** 2026-11-18, after review: untrusted code arrives as source or distribution artifacts, and run format is validated as defence in depth (context, consequences); conversion inserts ownership operations (item 5); an as-built note on primitives and globals (item 4); what the validator and VM check, and a known gap (item 7)
- **Amended:** 2026-11-18, as built: the wide variants are a `wide` prefix (item 2)
- **Related:** 0001 (artifacts), 0002 (values), 0004 (frames), 0006 (versioning), 0009 (untrusted code), 0012 (distribution format), 0017 (the IR)

## Context

Bytecode is the contract between the compiler, the artifact cache and every tier: tier 0 interprets it, and tier 1 and AOT compile from it. The design overview settles two properties, register-based and versioned, and leaves the instruction set and encoding open.

Tier 0 is not a stopgap. A short CLI run spends all of its time there. Steel's bytecode VM matched scheme-rs's JIT on small benchmarks, and the target is to come within about 1.5x of Vali's baseline JIT. Cold start matters too: loading an artifact should not involve translating its bytecode.

Untrusted code arrives as source or as distribution artifacts, whose validator is the security boundary (ADR 0017 #13). Run format is produced only by Vali's own converter, but it is still treated as if it could be malformed: no bytecode, however malformed, should be able to break memory safety. That is defence in depth.

## Options

### Encoding

- **A. Variable-length bytes** (the JVM, CPython): compact, but decoding is slower and operands are unaligned.
- **B. 32-bit words, one or more per instruction** (Lua, Guile 3): each instruction starts with a word holding the opcode and small operands, and larger operands take extra words. Aligned and quick to decode, at some cost in size.
- **C. A portable format translated at load time** into a pre-decoded in-memory form: the fastest dispatch, but every cold start pays for the translation.

### Safety

- **D. Every instruction checks what it relies on** (types and bounds), and the loader validates structure, so any artifact that loads is safe to run.
- **E. Unchecked instructions for whatever the compiler proved,** trusting the compiler. Faster, but then bytecode must be trusted as much as native code.

## Decision

Options **B and D**, as follows.

1. **Words.** Instructions are little-endian 32-bit words. Each instruction's first word holds an 8-bit opcode and up to 24 bits of operands in a few fixed layouts: three 8-bit fields, an 8-bit and a 16-bit field, or one 24-bit field. Wide variants and extra words carry larger operands.
2. **Registers are frame slots** (ADR 0004), addressed by 8-bit operands in the common forms, so three fit in one word. Functions with more than 256 slots use wide variants. A function's slot count is fixed at compile time.

   **As built:** the wide variants are one prefix, `wide`, holding the high bytes of the next instruction's three fields, so any instruction can address 16-bit slots, argument counts and free-variable indices. The encoder writes it only when a field needs it and the decoder refuses it anywhere else, so each instruction has one encoding. The interpreter expands its dispatch twice from one definition, once for instructions as they come and once, behind the prefix's arm, with fields widened, so code without the prefix runs as before: the benchmarks are within noise of the interpreter without it.
3. **Calls** put the callee and its arguments in consecutive slots at the top of the caller's frame. Those slots become the bottom of the callee's frame, as in Lua, so arguments are never copied. Multiple values come back in consecutive slots with a count.
4. **Names are resolved ahead of time.** Library-level variables are indexes into the module's variable table, and constants are indexes into its constant pool. Both tables are filled in when the artifact loads (ADR 0006). No instruction looks anything up by name.
   - **Each entry of the variable table is defined by the module or imported:** imported entries name the exporting library by identity and the variable by its exported name.
   - **Variables belong to library instances.** Each instance of a library, one per phase where it is used (ADR 0001 #2), has its own cell for each variable it defines. Loading a module links each imported entry to the cell of the instance it imports from. There is no table of globals shared by name, so a program defining `memv` cannot change what another library's `memv` means.

   **Bindings are cells only when they must be.** The compiler knows which bindings are ever `set!`:
   - **A binding that is never assigned** is a constant, read straight from the constant pool or inlined. Calls to a known procedure that is never assigned are direct calls. The interface records which exports are constant (ADR 0001), and the linker uses that across libraries (ADR 0017 #11).
   - **An assigned library variable** is a plain slot in its module's variable table, read and written with ordinary loads and stores.
   - **An assigned local variable** lives in a heap-allocated box (assignment conversion). At first only captured ones did, but re-entering a continuation copies the frames it sealed (ADR 0004 #8), and a variable in a slot would be copied with them, so a `set!` after the capture would be undone by re-entering it. Loops rebind rather than assign, so their variables stay in slots. Flow analysis in the optimising compiler can unbox again what no continuation can copy.

   No variable access takes a lock. scheme-rs kept every global behind a lock and cloned it on each read, which cost a quarter of `fib`'s run time in our profiling.

   **As built:** the procedures implemented in Rust are linked as primitives, a table only `(vali primitive)` imports reach, and a native library's functions are linked straight to its module as it loads. Isolate globals remain only for REPL variables and for bytecode a host builds itself.
5. **Fast paths are in the instruction set** for what dominates Scheme loops: fixnum arithmetic with an overflow fallback, fixnum compare-and-branch, `car`, `cdr`, `null?`, `pair?`, `eq?`-and-branch, and vector access with a fixnum index. Generic operations call into the runtime function table, which the JIT shares.

   **The IR adds** (ADR 0017): direct calls to a known function, with no procedure check; dispatch sites, each with a slot in a per-module table that the linker fills; and `dup`, `drop`, `drop-reuse` and constructors that build in a reused cell, which conversion inserts itself, from its own liveness, and only under strategies that count references (ADR 0017 #6).
6. **Function entry** checks arity, including optional and rest arguments and `case-lambda` clauses, decrements the fuel budget, and counts calls for tier-up. Loops are tail calls, so this is the only place either count is needed.
7. **Safe by construction (D).** Every instruction checks its operands' types and bounds. The loader rejects artifacts whose slot indexes, constant indexes or jump targets are out of range, so the dispatch loop needs no checks of its own. Proven contract checks (ADR 0010) can be removed from bytecode. Memory-safety checks are removed only in native code.

   **As built:** the validator also rejects any write to slot 0, which holds the running closure the VM reads its code from, and any call whose header would cover it. `%catch`, `%call/cc` and `%continue` check their argument counts, a returned callee's header words are cleared from the caller's slots, and `set-free` changes only closures of the running module. **A known gap, closed with the IR (ADR 0017):** deferred counting treats a caller's slots above a pending call's window as dead, and nothing yet stops bytecode reading one it wrote before the call. Vali's compiler never does; the IR's converter computes liveness, and the validator will then check it.
8. **Each function has side tables:** debug information (ADR 0014), meaning its name, a line table mapping instructions to resolved source positions, and the names and live ranges of its local variables by slot; and slot liveness at each safepoint (ADR 0003). Debug information is self-contained, so stack traces and debuggers work without the source files.
9. **The format is versioned and private.** Bytecode carries a format version. It is a cache format, not an interchange format: any version mismatch means rebuilding, and there is no compatibility promise between Vali releases. It is rebuilt from source or from the library's distribution artifact (ADR 0012). Libraries are never shipped in this format.
10. **Bytecode is directly executable.** Loading an artifact fills in its tables but does not rewrite its code.

The opcode list lives in a specification, `docs/bytecode.md`, that changes along with the format version. It is not an ADR.

## Consequences

- Tier 0 can run an artifact as soon as it has been read and its tables filled in.
- The dispatch loop starts as a `match` inside a loop, the portable choice in stable Rust. If Rust's explicit tail calls become stable, threaded dispatch is an option. The format depends on neither.
- The VM pays for type checks that the compiler could have removed. The JIT recovers that cost in hot code.
- Malformed run-format bytecode cannot break memory safety, which backs up the distribution format's validator.
- With no compatibility promise, the format can change freely while the VM is being tuned.

## Alternatives considered

- **16-bit register operands everywhere:** simpler decoding with no wide variants, but fewer operands per word, so more instructions take two words.

## Left open

- Whether jump offsets count words or bytes.
- Which compare-and-branch and fast-path instructions to add first. Measuring the benchmark set decides; the opcode list is in `docs/bytecode.md`, not here.

## References

- Roberto Ierusalimschy, Luiz Henrique de Figueiredo and Waldemar Celes, *The Implementation of Lua 5.0*, Journal of Universal Computer Science, 2005.
- Yunhe Shi, David Gregg, Andrew Beatty and M. Anton Ertl, *Virtual Machine Showdown: Stack versus Registers*, VEE 2005.
- The Guile 3 manual: the VM and its instruction encoding.
