# 0004. Frames and continuations

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, the Scheme-level control API is SRFI 226 (ADR 0007)
- **Amended:** 2026-11-16, reworked: each task runs on its own stack of runtime-owned segments, with native frames for compiled code, so native debuggers and profilers can unwind it
- **Amended:** 2026-11-17, a suspended task lets its isolate run another task; the isolate, not the task, is the host's future (ADR 0008)
- **Amended:** 2026-11-17, tier 0's frames are native frame records with named return addresses, and the interpreter links itself into the chain, so native debuggers and profilers unwind and name interpreted frames too
- **Amended:** 2026-11-17, as built: a three-word frame header, three kinds of marker stub, signal frames where stacks switch, and the measured cost of keeping the chain current
- **Amended:** 2026-11-17, continuations as built in tier 0: segments shared between the stack and continuations, an underflow flag that copies one frame at a time, and catches that point into sealed frames (item 8)
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
6. **Tier 0's frames are native frame records too.** The VM is Rust and runs on the thread's own stack. The Scheme frames it interprets live in the task's segments, which grow downward like native stacks, and each starts with the platform's frame record: the saved frame pointer and the return address, where the ABI puts them on x86-64 and AArch64. Interpreted and compiled frames therefore form one chain, which the collector, debuggers and profilers walk the same way. Calls between the VM and compiled code switch stacks, like calls into Rust.
   - **Interpreted frames have named return addresses.** Each bytecode function gets a small native *marker stub*, a few bytes that never run, registered under the function's Scheme name through the GDB JIT interface, which GDB and LLDB both read. An interpreted frame's return address points into its function's stub, so a backtrace shows the Scheme name. The bytecode position is kept in a separate word. V8's `--interpreted-frames-native-stack` and CPython 3.12's perf trampolines do the same.
   - **The interpreter links itself into the chain.** The VM is entered through a small trampoline written in assembly for each architecture. On every Scheme call and return, the VM stores the current Scheme frame's address into the trampoline's saved frame pointer, so an unwinder stopped anywhere in the interpreter continues through the Scheme frames and back to the native code that entered the VM.
   - **Three kinds of frame need their own call-frame information,** so besides each function's stub there are three shared ones:
     - **ordinary frames,** whose frame pointer holds their record;
     - **the bottom frame,** whose caller is the host. A frame on a segment cannot say where the host's stack was, so the host's stack pointer is kept in the word above the bottom frame's header, and its stub's call-frame information reads it from there;
     - **the first frame of a chained segment.** Segments, and the thread's stack, sit anywhere in memory relative to each other, and GDB stops at a frame whose caller sits at a lower address, as a corrupt stack. The trampoline and this stub are therefore marked as signal frames, which GDB and LLDB accept at a stack switch. GDB shows the trampoline as `<signal handler called>`.
   - **The header has a third word,** above the frame record: the caller's bytecode position, and its base slot, so the result lands in the right slot even when a frame was moved to a new segment. Unwinders ignore it.
   - **Per-function stubs are copies of a standard prologue,** 16 bytes each, in a page of executable memory per module, registered as an in-memory ELF object holding only a symbol table. Debuggers find no call-frame information for them, analyse the prologue, and unwind through the frame pointer. The bottom frame and the first frame of each chained segment keep their shared stubs, whose call-frame information is special, so they show the shared names.
   - **What stays configurable:** frame records and the trampoline link are always on. On fib and tak, keeping them current measured between noise and about 10% of tier 0's call-heavy code on a shared VM; a quieter machine will pin it down. Updating the link only where control leaves the dispatch loop (Rust procedures, allocation, errors) is a cheaper option, at the cost of stale frames in samples taken inside the loop itself. Named stubs and their registration are on by default, with a switch for hosts that count every microsecond of load time. perf's map file is written only when asked for. Where generating code is forbidden, such as iOS, which relies on AOT compilation, every interpreted function shares one `<interpreted>` stub: unwinding still works, without names.
   - **What still needs a debugger extension:** the current line within an interpreted function, since stubs carry no bytecode positions, and printing Scheme values.
7. **Rust never runs on a task's segments.** Bridges and runtime calls switch to the thread's own stack first, as Go does for its runtime and cgo calls, so segments can stay small and Rust code cannot overflow them. A switch costs a few nanoseconds. If measurements show that matters for some bridges, those can be marked to run in place after a check that enough stack remains.
8. **Continuations: full `call/cc`, SRFI 226's delimited continuations and continuation marks, and `dynamic-wind`,** all on segments:
   - capturing a continuation seals the current segment and continues in a fresh one, copying nothing;
   - reinstating one copies frames back lazily, a chunk at a time, as they are returned through;
   - one-shot uses (async suspension, generators, exceptions) only switch stacks.

   **As built in tier 0:**
   - **Capturing splits the segment.** `%call/cc` seals every frame from its caller out; the stack goes on below them, in the same segment. A segment's memory is shared between the stack and the continuations that sealed part of it, and the stack holds views of segments: a segment and the top of the part it may use. A segment part of which was sealed is never reused, since what the sealed frames left there would read as counted values.
   - **A continuation owns the frames it sealed:** from its first frame out to one whose caller is the host or is sealed already. It refers to the continuation that holds that caller, its parent, and keeps the segments its frames are in alive. It is an ordinary heap object whose record sits in the isolate's object table (ADR 0016). Its children are the values its frames hold in their own slots (those below the callee's header), its parent and its catches' handlers, so every strategy frees continuations, and collects cycles through them, like any object. Under precise counting, sealing moves the stack's counts to the continuation; the other strategies count the frames' values in, once per frame sealed.
   - **An underflow flag copies frames back, one at a time.** The outermost live frame's position word has `UNDERFLOW` when its caller is sealed. Returning from it copies the caller's own slots to the top of the now empty segment, with the same flag unless the caller's caller is the host, and resumes the copy. The isolate remembers which continuation holds the caller, and moves to its parent as frames are copied past its own. Frames never returned through are never copied. Copying a chunk of several frames at a time, as Chez does, is a later optimisation.
   - **Reinstating** drops the live stack and returns the value as if from a frame whose caller is the continuation's first frame. A continuation whose caller is the host returns to whichever host is running the VM now; there is no barrier yet, since Rust cannot nest a run (item 10).
   - **`%catch` records which continuation holds its frame** once the frame is sealed. A continuation keeps a copy of the catches waiting when it was captured, and reinstating restores them. Raising to a catch whose frame is sealed drops the live stack and copies the frame in before calling the handler.
   - **Every assigned local variable is boxed** (ADR 0005 #4), since a frame that a continuation copies twice would otherwise hold two copies of a variable.
   - **Cost:** the return path gains one test of a flag that was tested before. Without collection, fib and tak are unchanged; under precise counting, fib measured about 3% slower, inside the noise of a shared machine.
9. **A task can suspend at any call.** When Scheme calls an async bridge, the task switches back to the thread's stack and its segments stay as they are. The isolate then runs another of its ready tasks, or, if none is ready, its future returns `Pending` (ADR 0008). When the bridge's future completes, the task becomes ready, and the host polls the isolate again, possibly on another thread, which switches the task back in. This is how Wasmtime's fibers work.
10. **Rust code is never in the middle of a Scheme stack.** A bridge returns a value, asks the runtime to tail-call a Scheme procedure, or returns a future. When Rust really must call Scheme and wait for the answer (a comparison callback inside a Rust sort, say), it starts a nested run behind a continuation barrier. Continuations captured inside the nested run can escape out of it, but cannot re-enter it once it has returned.
11. **WebAssembly needs a target of its own,** designed in a later ADR. It has no standard stack switching yet, and running Vali's VM inside a WebAssembly engine's own VM is at best a fallback. A real target probably compiles Scheme to WebAssembly directly.

## Consequences

- GDB, LLDB, perf and similar tools unwind every frame natively, and name compiled frames and, through marker stubs, interpreted ones. JIT code must be registered with them, which is left to a later ADR on debugging and profiling.
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
  - GDB and LLDB extensions that show the current line of interpreted frames and print values;
  - a sampling profiler that walks task stacks.

## References

- Robert Hieb, R. Kent Dybvig and Carl Bruggeman, *Representing Control in the Presence of First-Class Continuations*, PLDI 1990.
- Carl Bruggeman, Oscar Waddell and R. Kent Dybvig, *Representing Control in the Presence of One-Shot Continuations*, PLDI 1996.
- Andrew Kennedy, *Compiling with Continuations, Continued*, ICFP 2007.
- SRFI 226, *Control Features*.
- The Go runtime: goroutine stacks, the contiguous-stacks design for Go 1.3, and `runtime-gdb.py`.
- The Wasmtime documentation: async support and fibers.
- The Cranelift documentation: the `tail` calling convention, `return_call`, stack-limit checks and user stack maps.
