# 0005. Bytecode format

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, this bytecode is the run format only; distribution uses ADR 0012
- **Amended:** 2026-11-16, bindings that are never assigned are constants or direct slots; cells only for assigned locals captured by closures
- **Amended:** 2026-11-16, debug information is self-contained: usable without the source files
- **Amended:** 2026-11-17, variables are defined or imported, and belong to library instances (item 4)
- **Related:** 0001 (artifacts), 0002 (values), 0004 (frames), 0006 (versioning), 0009 (untrusted code), 0012 (distribution format)

## Context

Bytecode is the contract between the compiler, the artifact cache and every tier: tier 0 interprets it, and tier 1 and AOT compile from it. The design overview settles two properties, register-based and versioned, and leaves the instruction set and encoding open.

Tier 0 is not a stopgap. A short CLI run spends all of its time there. Steel's bytecode VM matched scheme-rs's JIT on small benchmarks, and the target is to come within about 1.5x of Vali's baseline JIT. Cold start matters too: loading an artifact should not involve translating its bytecode.

Bytecode may come from a source the host doesn't fully trust. A host running untrusted code loads only bytecode (ADR 0009), and that is safe only if no bytecode, however malformed, can break memory safety.

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
3. **Calls** put the callee and its arguments in consecutive slots at the top of the caller's frame. Those slots become the bottom of the callee's frame, as in Lua, so arguments are never copied. Multiple values come back in consecutive slots with a count.
4. **Names are resolved ahead of time.** Library-level variables are indexes into the module's variable table, and constants are indexes into its constant pool. Both tables are filled in when the artifact loads (ADR 0006). No instruction looks anything up by name.
   - **Each entry of the variable table is defined by the module or imported:** imported entries name the exporting library by identity and the variable by its exported name.
   - **Variables belong to library instances.** Each instance of a library, one per phase where it is used (ADR 0001 #2), has its own cell for each variable it defines. Loading a module links each imported entry to the cell of the instance it imports from. There is no table of globals shared by name, so a program defining `memv` cannot change what another library's `memv` means.

   **Bindings are cells only when they must be.** The compiler knows which bindings are ever `set!`:
   - **A binding that is never assigned** is a constant, read straight from the constant pool or inlined. Calls to a known procedure that is never assigned are direct calls. The interface records which exports are constant (ADR 0001), so this holds across libraries.
   - **An assigned library variable** is a plain slot in its module's variable table, read and written with ordinary loads and stores.
   - **Only an assigned local variable captured by a closure** lives in a heap-allocated box (assignment conversion).

   No variable access takes a lock. scheme-rs kept every global behind a lock and cloned it on each read, which cost a quarter of `fib`'s run time in our profiling.
5. **Fast paths are in the instruction set** for what dominates Scheme loops: fixnum arithmetic with an overflow fallback, fixnum compare-and-branch, `car`, `cdr`, `null?`, `pair?`, `eq?`-and-branch, and vector access with a fixnum index. Generic operations call into the runtime function table, which the JIT shares.
6. **Function entry** checks arity, including optional and rest arguments and `case-lambda` clauses, decrements the fuel budget, and counts calls for tier-up. Loops are tail calls, so this is the only place either count is needed.
7. **Safe by construction (D).** Every instruction checks its operands' types and bounds. The loader rejects artifacts whose slot indexes, constant indexes or jump targets are out of range, so the dispatch loop needs no checks of its own. Proven contract checks (ADR 0010) can be removed from bytecode. Memory-safety checks are removed only in native code.
8. **Each function has side tables:** debug information (ADR 0014), meaning its name, a line table mapping instructions to resolved source positions, and the names and live ranges of its local variables by slot; and slot liveness at each safepoint (ADR 0003). Debug information is self-contained, so stack traces and debuggers work without the source files.
9. **The format is versioned and private.** Bytecode carries a format version. It is a cache format, not an interchange format: any version mismatch means rebuilding, and there is no compatibility promise between Vali releases. It is rebuilt from source or from the library's distribution artifact (ADR 0012). Libraries are never shipped in this format.
10. **Bytecode is directly executable.** Loading an artifact fills in its tables but does not rewrite its code.

The opcode list lives in a specification, `docs/bytecode.md`, that changes along with the format version. It is not an ADR.

## Consequences

- Tier 0 can run an artifact as soon as it has been read and its tables filled in.
- The dispatch loop starts as a `match` inside a loop, the portable choice in stable Rust. If Rust's explicit tail calls become stable, threaded dispatch is an option. The format depends on neither.
- The VM pays for type checks that the compiler could have removed. The JIT recovers that cost in hot code.
- Untrusted bytecode is as safe to load and run as source, under the same limits.
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
