# 0004. Frames and continuations

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, the Scheme-level control API is SRFI 226 (ADR 0007)
- **Amended:** 2026-11-16, reworked: each task runs on its own stack of runtime-owned segments, with native frames for compiled code, so native debuggers and profilers can unwind it
- **Related:** 0003 (stack maps, return barrier), 0005 (the VM's calling convention), 0006 (frame layout in the ABI), 0007 (SRFI 226), 0008 (tasks), 0009 (memory limit)

## Context

R7RS-small requires full `call/cc`, `dynamic-wind` and proper tail calls, and Vali also provides delimited continuations (SRFI 226). Vali's tasks must suspend at any call to await a Rust future (ADR 0008). Three tiers must call each other freely, the collector needs exact stack roots, and compiled code should be debuggable and profilable with native tools: GDB, LLDB, perf, Instruments.

The compiler converts code to CPS, but CPS is only its intermediate representation. It does not mean continuations are heap-allocated closures at run time: return points become frames.

How frames are represented decides all of this:

- **On the thread's own native stack:** `call/cc` must copy the stack, a fixed-size stack overflows on deep non-tail recursion, and a task cannot suspend without its frames being lost to the next task on that thread. Tail calls are solved by reusing frames, but the other problems are not.
- **As data in runtime-managed records, run by a trampoline:** continuations and suspension are easy. But native debuggers and profilers see none of the Scheme frames, and every call goes through memory.
- **On per-task stacks the runtime owns, with native frames** (stackful coroutines): this is what Go's goroutines do. A goroutine stack starts at 2 KB and grows on demand, by copying since Go 1.4, after chained segments suffered "hot splits". Prologue checks catch overflow without guard pages, and cgo calls switch to the thread's own stack. Frames are native, so GDB, perf and pprof unwind them. Wasmtime runs Cranelift-compiled code inside Tokio the same way: its fibers suspend when guest code calls an async host function.
- **Multi-shot continuations** are what Go never needs: `call/cc` may resume one continuation many times. Chez Scheme's segmented stacks (Hieb, Dybvig and Bruggeman) handle it. Capturing seals the current segment instead of copying it, and frames are copied back lazily as they are returned through, a chunk at a time, which also avoids hot splits. One-shot continuations (Bruggeman, Waddell and Dybvig) need no copying at all.

## Decision

1. **Each task runs on its own stack,** made of segments the runtime allocates: small at first, chained as the task needs more. Deep non-tail recursion chains more segments and is bounded by the memory limit (ADR 0009), not by a fixed stack size.
2. **Compiled code uses native frames on those segments.** In JIT and AOT code, the stack pointer points into the task's current segment, and Scheme calls are real calls and returns. Tail calls reuse the caller's frame, through Cranelift's tail calling convention and `return_call`.
3. **Native tools can unwind compiled code.** Generated code keeps frame pointers and emits standard call-frame information.
4. **Segment boundaries are handled in generated code.** Each function's prologue checks the stack limit, as Go's and Cranelift's checks do; when it is exceeded, an overflow handler chains a new segment. Returning past the bottom of a segment goes through an underflow handler, which continues in the previous segment.
5. **Every frame describes itself:** its procedure, its return address, the link to its caller, and its value slots. Values live across a call are kept in frame slots at that call, and each safepoint has an exact stack map (Cranelift's user stack maps; ADR 0003). The frame layout and the stack-map format are part of the ABI (ADR 0006).
6. **Tier 0's frames are data.** The VM is Rust and runs on the thread's own stack. The Scheme frames it interprets live in the task's segments, in the same slot layout as native frames, so the collector and debugger extensions read both the same way. Calls between the VM and compiled code switch stacks, like calls into Rust.
7. **Rust never runs on a task's segments.** Bridges and runtime calls switch to the thread's own stack first, as Go does for its runtime and cgo calls, so segments can stay small and Rust code cannot overflow them. A switch costs a few nanoseconds. If measurements show that matters for some bridges, those can be marked to run in place after a check that enough stack remains.
8. **Continuations: full `call/cc`, SRFI 226's delimited continuations and continuation marks, and `dynamic-wind`,** all on segments:
   - capturing a continuation seals the current segment and continues in a fresh one, copying nothing;
   - reinstating one copies frames back lazily, a chunk at a time, as they are returned through;
   - one-shot uses (async suspension, generators, exceptions) only switch stacks.
9. **A task can suspend at any call.** When Scheme calls an async bridge, the task switches back to the thread's stack and its future returns `Pending`. Its segments stay as they are. When the future completes, the host polls the task again, possibly on another thread, and it switches back in. This is how Wasmtime's fibers work.
10. **Rust code is never in the middle of a Scheme stack.** A bridge returns a value, asks the runtime to tail-call a Scheme procedure, or returns a future. When Rust really must call Scheme and wait for the answer (a comparison callback inside a Rust sort, say), it starts a nested run behind a continuation barrier. Continuations captured inside the nested run can escape out of it, but cannot re-enter it once it has returned.
11. **WebAssembly needs a target of its own,** designed in a later ADR. It has no standard stack switching yet, and running Vali's VM inside a WebAssembly engine's own VM is at best a fallback. A real target probably compiles Scheme to WebAssembly directly.

## Consequences

- GDB, LLDB, perf and similar tools unwind JIT and AOT frames natively. JIT code must be registered with them, and interpreted frames need a debugger extension. Both are left to a later ADR on debugging and profiling.
- `call/cc`, generators, exceptions and async share one mechanism. Capturing a continuation is cheap; reinstating a multi-shot continuation pays for copying.
- A task's first segment can be small, so many suspended tasks cost little memory.
- Every call from compiled code into Rust, and every switch between the VM and compiled code, costs a stack switch.
- Switching stacks needs a small amount of assembly for each CPU architecture. An existing crate such as `corosensei` could provide it.
- The frame layout is part of the ABI (ADR 0006), so changing it means rebuilding native code.
- Proper tail calls make replaced frames disappear from backtraces, as in any Scheme. If that hurts debugging, a debug mode could keep a short record of recent tail calls, as MIT Scheme's history and Racket's errortrace do.

## Alternatives considered

- **The thread's own native stack:** simplest for native tools, but continuations must copy the whole stack, deep recursion overflows, and tasks cannot suspend.
- **Frames as data everywhere, driven by a trampoline** (this ADR's first version, close to scheme-rs): simple and uniform. But compiled frames are invisible to native debuggers and profilers, and every call goes through memory.
- **Contiguous stacks grown by copying** (Go since 1.4): avoids hot splits. But multi-shot continuations still need copying, and moving a stack requires that nothing outside it points into it. Chez-style segments handle growth and continuations with one mechanism.
- **Cheney on the MTA** (CHICKEN): uses the C stack as the allocation nursery, which ties the design to compiling via C.

## Left open

- Segment sizes and growth policy.
- How exceptions and parameters are built on continuation marks.
- The stack-switching implementation: Vali's own assembly, or a crate.
- The WebAssembly target (item 11), for a later ADR.
- Debugging and profiling, for a later ADR, including:
  - registering JIT code with debuggers (the GDB JIT interface) and with perf (`jitdump` files);
  - publishing each thread's current task stack under a known symbol, for debugger extensions;
  - GDB and LLDB extensions that show interpreted frames and print values;
  - a sampling profiler that walks task stacks.

## References

- Robert Hieb, R. Kent Dybvig and Carl Bruggeman, *Representing Control in the Presence of First-Class Continuations*, PLDI 1990.
- Carl Bruggeman, Oscar Waddell and R. Kent Dybvig, *Representing Control in the Presence of One-Shot Continuations*, PLDI 1996.
- Andrew Kennedy, *Compiling with Continuations, Continued*, ICFP 2007.
- SRFI 226, *Control Features*.
- The Go runtime: goroutine stacks, the contiguous-stacks design for Go 1.3, and `runtime-gdb.py`.
- The Wasmtime documentation: async support and fibers.
- The Cranelift documentation: the `tail` calling convention, `return_call`, stack-limit checks and user stack maps.
