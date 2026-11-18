# 0019. WebAssembly target

- **Status:** Proposed
- **Date:** 2026-11-18
- **Related:** 0002 (32-bit targets), 0003 (strategies, stack maps), 0004 #11 (deferred here), 0005 (bytecode as the code generator's input), 0006 (ABI version), 0008 (isolates as futures), 0009 (capabilities), 0013 (standalone executables), 0014 (line tables), 0016 (native libraries), 0017 (the IR)

## Context

ADR 0004 #11 deferred WebAssembly: the rest of Vali's control design rests on per-task stacks of native frames, and WebAssembly can't provide them.

- **No stack switching.** The stack-switching proposal (typed continuations) is in phase 3 as of early 2026, and Wasmtime does not support it yet. When it lands it gives one-shot continuations. Multi-shot `call/cc` would still need copying.
- **Its own stack is opaque.** Code can neither walk nor scan WebAssembly's call stack. A collector cannot find roots in it, and a backtrace cannot be read from it.
- **No code generation inside a component.** There is no JIT tier, and Cranelift does not emit WebAssembly.
- **WebAssembly 3.0 does provide** tail calls (`return_call`, `return_call_indirect`), exception handling, GC types and memory64.
- **Rust's `wasm32-wasip3` target is tier 2 as of Rust 1.100.0,** so the runtime and native libraries build for it on stable Rust, as the architecture requires. **But its standard library is built with `panic=abort`,** so on stable a panic traps the component instance, and the component model never lets a trapped instance be entered again. Unwinding works on nightly: `-Zbuild-std` with `-C panic=unwind -C target-feature=+exception-handling`. Nightly as of 2026-11-17 emits the standard exception instructions (`try_table`, `throw_ref`), and Wasmtime 50 runs them by default. In a probe, `catch_unwind` caught a panic, `Drop` ran, and the instance carried on. The probe grew from 105 KB to 158 KB, mostly from the unwinder and landing pads (measured 2026-11-18).
- **WASI 0.3 was released on 11 June 2026.** Its component model has native async: `async` functions, `stream<T>` and `future<T>`. An export can be lifted with the *callback* option, so it never blocks on a stack. It returns a code that says "wait on this waitable set", and the host calls its callback when an event arrives. This was designed for guests that have no stack switching.

A WebAssembly build also has to reach native code. There are two kinds:

- **Rust native libraries** (ADR 0016), which compile to WebAssembly along with the runtime.
- **Other components,** written in any language, reached through WIT interfaces. On this target, these play the role that platform libraries play natively.

## Options

### Control

- **A. The interpreter only.** Compile `vali-runtime` to WebAssembly and run bytecode on it. Tier 0's frames already live in segments the runtime owns (ADR 0004 #6), so continuations and suspension work unchanged. Cheapest to build, but slowest: an interpreter inside the engine's own compiled code.
- **B. Heap CPS.** Every return point becomes a closure allocated on the heap, and every call is a tail call that passes it along. Continuations and suspension are trivial. But every non-tail call allocates, and this would be the only target that represents control differently from ADR 0004.
- **C. An explicit stack, split at return points.** Guile's Hoot does this with its "tailify" pass. Each function is cut at its non-tail calls into an entry function plus one *continuation function* per return point. A call pushes a frame whose return word names the continuation function, then `return_call`s the callee. A return reads the word and `return_call_indirect`s through it. Every call is a tail call, so the engine's stack never grows past the run loop and the piece currently running. Frames are data in runtime-owned segments, laid out exactly as tier 0's are. A call allocates nothing, but every return is an indirect call.
- **D. Asyncify** (Binaryen). Instrument every function so it can unwind its locals into memory and later rewind them. Code size and per-call cost go up everywhere. It captures eagerly, by copying the whole stack. Multi-shot continuations need repeated rewinds, and nothing it does can be shared with ADR 0004.
- **E. Wait for stack switching.** Unknown timing, one-shot only, and it doesn't solve root scanning or backtraces.

### Memory

- **F. Linear memory, with Vali's own strategies** (ADR 0003). The heap is the one the Rust runtime already manages, and generated code inlines the same barriers and allocation fast paths.
- **G. WebAssembly GC.** The engine collects. But Rust code cannot hold GC references, so every object the runtime or a bridge touches would need a second representation. No strategy from ADR 0003 survives, and the memory limit (ADR 0009 #21) would depend on the engine.

### Foreign code

- **H. WIT interfaces as native libraries.** A WIT interface is served to the compiler like a native library (ADR 0016 #8). Its functions become procedures, converted through the canonical ABI.
- **I. Raw core-module imports.** Scheme declares core WebAssembly imports with integer signatures and handles memory itself. Lower-level, unsafe, and not composable with components.

## Recommendation

**C, F and H, delivered in two steps: the interpreter (A) first, then compiled code (C) on the same frames.**

### The target

1. **The output is a WebAssembly component targeting WASI 0.3:** `vali build --target wasm32-wasip3`. It is built ahead of time only, as ADR 0013 describes for executables. There is no JIT. Code evaluated at run time runs on tier 0 (ADR 0013 #13).
2. **Values stay 64 bits wide.** This settles ADR 0002's left-open item for wasm32. WebAssembly has native `i64` operations, so the tag table, object layouts and fixnum range stay the same, and only addresses are 32 bits, in the word's low half. Generated code, the runtime and the debugger records see the same values on every target. Pointer-heavy data costs memory as on a 64-bit target. memory64 can come later without changing values.
3. **The heap is in linear memory, under Vali's own strategies (F).** The runtime, bridges and generated code share one representation, as on native targets.

### Control

4. **Step one is tier 0 on WebAssembly.** `vali-runtime` compiles with the `native-frames` feature off. There is no assembly trampoline and no stack switch: the VM is a Rust loop on the engine's stack, and Scheme frames are already in segments. `call/cc`, SRFI 226 and task suspension work as they do in tier 0 now. This gives a working target before a second code generator exists.
5. **Step two is compiled code with an explicit stack (C).** A second code generator, built on `wasm-encoder`, lowers run-format bytecode, which is the same input Cranelift gets (ADR 0005).
   - **Each bytecode function becomes an entry function and one continuation function per return point.** Join points, loops and contified procedures stay inside one WebAssembly function as structured control flow: `block`, `loop`, `br`. The IR's join points (ADR 0017) are already in that shape.
   - **Frames use tier 0's layout,** in the same segments. Interpreted and compiled frames therefore form one chain with no stack switch between them, which native targets can't offer. A frame's return word holds a function-table index instead of a code address. An interpreted caller's word names a shared function that re-enters the VM.
   - **Calls, returns and tail calls:** a call writes the return word and `return_call`s the callee; a return `return_call_indirect`s through the word; a tail call reuses the frame and `return_call`s. A closure's code word (ADR 0002) holds the entry's table index.
   - **Values live in WebAssembly locals only within one piece.** Whatever is live across a call is already kept in frame slots (ADR 0004 #5).
6. **The collector never scans the engine's stack.** At every call that can collect, live values are in frame slots, so a stack map is a statement of which slots are live. That is the format ADR 0004 #5 already uses. Locals that hold pointers are reloaded after such a call.
7. **Continuations and suspension follow ADR 0004 #8 and #9 unchanged.** Capturing seals segments, and the underflow flag copies frames back lazily. The engine's stack under the running piece holds only the run loop, so a task suspends by returning to the loop, and the isolate's future returns `Pending` (ADR 0008 #4). Nested runs for Rust callbacks (ADR 0004 #10) work as they do natively.
8. **Raising and escaping drop segments, which are data.** Scheme errors don't need WebAssembly exceptions. Fuel and the memory limit (ADR 0009 #21) apply unchanged.
9. **Backtraces walk the explicit stack.** A return word identifies a continuation function, and each function has a debug record (ADR 0004 #6): its name, line table (ADR 0014) and inline chain. One backtrace builder serves tier 0 and compiled code. The name section names each piece `f` and `f@k` for continuation `k`, so engine traces, which show only the innermost piece, still make sense. Generating DWARF for browser and Wasmtime debuggers is left open. Proper tail calls drop frames here as everywhere (ADR 0004, Consequences).
10. **The ABI version (ADR 0006 #4) covers the WebAssembly generator too:** the return word's meaning, the function-table layout and the runtime functions' imports.

### Native code

11. **Rust native libraries are compiled into the component.** `vali build` links them with the runtime, as ADR 0013 #6 links the runtime archive. A reachable native library whose crate doesn't build for `wasm32-wasip3` fails the build, naming the library (as ADR 0013 #8 does for code).
12. **A WIT interface is a library (H).** A loader can serve a WIT package. The compiler turns each interface into a native library, `(wit <namespace> <package> <interface>)`, such as `(import (wit wasi clocks monotonic-clock))`. The package version is part of the identity (ADR 0009 #2). The component then imports that interface. Proposed conversions, through the canonical ABI:

    | WIT | Scheme |
    |---|---|
    | `bool`, `char`, `f32`, `f64` | boolean, character, flonum |
    | `s8` … `u64` | exact integer, range-checked as ADR 0007 #5 does |
    | `string` | string. Both sides are UTF-8 (ADR 0002 #11), so conversion is a copy |
    | `list<u8>` | bytevector |
    | `list<T>` | list |
    | `tuple<…>` | vector |
    | `record` | a record type generated for it |
    | `enum`, `flags` | a symbol; a list of symbols |
    | `variant` | a record of the case's symbol and its payload |
    | `option<T>` | the value or `#f`. When `T` can itself be `#f`, a `some` record wraps it |
    | `result<T, E>` | `T` is returned; `E` is raised as a condition that carries it |
    | `own<R>`, `borrow<R>` | a resource object, freed like a Rust object (ADR 0016 #2) by `resource.drop`, plus an explicit `close` |
    | `future<T>` | a value whose `await` suspends the task |
    | `stream<u8>`, `stream<char>` | a binary or textual port. Other streams get a generator-style reader |

13. **Calling an async import suspends only the calling task.** It is lowered with the async option. If the call blocks, the task suspends on the subtask's waitable (ADR 0004 #9) and the isolate runs another task.
14. **Exports are lifted with the callback option.** The isolate's run loop *is* the callback. When no task is ready, it returns "wait" on the isolate's waitable set, and each event that arrives wakes the task waiting on it. This is ADR 0008 #4's `Pending` translated to the component model, and it needs no stack switching. A program exports `wasi:cli/run`. A library declares the WIT world it implements, and `vali build` checks its exports against the world's types and generates their lifting. An export WIT marks as synchronous must not suspend; a task that tries raises a condition naming the export.
15. **WASI imports back capabilities (ADR 0009 #5).** `(scheme file)` uses `wasi:filesystem` preopens, `(scheme time)` uses `wasi:clocks`, and so on. The host program receives capability objects built from those imports. A component imports only the WASI interfaces its reachable libraries use, so its import list states its authority.
16. **Vali doesn't link components.** It writes a component with imports. Composition is done by standard tools (`wac`, `wasm-tools compose`) or by the host runtime. ADR 0009 #22 holds: Vali never loads code at run time.
17. **One component instance runs isolates concurrently, not in parallel.** Parallel isolates (ADR 0008 #5) wait for threads in WASI 0.3.x.
18. **A panic ends the instance by default, and unwinding is an opt-in nightly build.** This changes ADR 0016 #9 for this target only.
    - **On stable** (the default), the runtime installs a panic hook that writes the procedure's name, the panic message and the Scheme backtrace to stderr before aborting. That is possible because Vali's stack is data the hook can walk (item 9). The host sees a trap, and recovers by instantiating the component again. Bridge authors should treat a panic as fatal on this target.
    - **On nightly,** `vali build --target wasm32-wasip3 --unwind` rebuilds the standard library with unwinding over WebAssembly exception handling. Panics then become Scheme conditions, as ADR 0016 #9 says. This follows the architecture's rule that nightly features are optional and never required.
    - **When Rust ships this target's standard library with unwinding,** the default becomes unwinding and the flag goes away. That depends on LLVM making exception handling a default target feature. rust-lang/rust#118168 tracks it.

## Consequences

- WebAssembly gets full `call/cc`, SRFI 226, tasks and async without stack switching, using the same frames and continuation code as tier 0.
- The collector and backtraces work without seeing the engine's stack, because Vali's stack is the only one that holds Scheme state.
- Every Scheme return is an indirect tail call, and splitting at return points multiplies the number of functions. Code is larger, and return-heavy code runs slower than it would with native returns.
- There is a second code generator to maintain, beside Cranelift. Lowering from the same run-format bytecode keeps the tiers' meaning in one place, but every bytecode instruction is implemented twice.
- WIT gives Scheme typed access to any component, and exposes Scheme libraries as components to any language that has WIT bindings. Browsers can run the result through `jco`, and JSPI is not needed.
- If stack switching ships, it could speed up one-shot continuations, but the explicit stack stays for multi-shot ones and for root scanning.

## Questions to settle before accepting

- Panics (item 18): is ending the instance acceptable as the stable default, or should the WebAssembly target wait for unwinding on stable before it ships? And what does unwinding cost on the real runtime, rather than on a probe?
- How fast is `return_call_indirect` in Wasmtime, V8 and SpiderMonkey? That decides whether returns to a statically known continuation should be direct `return_call`s.
- The conversions in item 12: lists or vectors for `list<T>`, how `option<T>` behaves, and whether `result` errors are raised or returned.
- How a Scheme library names the WIT world it implements, and where its WIT comes from.
- Whether the first step (item 4) ships on its own, and with which strategies.
- Fuel: is decrementing it at function entry enough when loops live inside one WebAssembly function?

## References

- Andy Wingo, *Guile Hoot*: the tailify pass, and compiling Scheme to WebAssembly with an explicit return stack.
- The WebAssembly component model: *Async.md* and the canonical ABI's callback option.
- WASI 0.3 release notes (Bytecode Alliance, June 2026).
- The WebAssembly stack-switching proposal.
- Binaryen's Asyncify.
- Robert Hieb, R. Kent Dybvig and Carl Bruggeman, *Representing Control in the Presence of First-Class Continuations*, PLDI 1990.
