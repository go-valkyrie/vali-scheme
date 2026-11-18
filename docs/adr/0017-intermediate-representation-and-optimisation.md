# 0017. Intermediate representation and where optimisation runs

- **Status:** Accepted, partly built: the IR, lowering and conversion (items 1 to 5 and 7). Items 11, 12 and 15, and specialisation, are planned but not scheduled
- **Date:** 2026-11-18
- **Amended:** 2026-11-18, after review: conversion inserts ownership operations, which distribution code never holds; the distribution format's validator is the security boundary; invalidated code lives while a continuation refers to it; per-library expansion is a prerequisite (items 6, 9, 10, 12 and 13)
- **Amended:** 2026-11-18, as built: everything compiled is lowered to the IR and converted to run format; the baseline compiler is gone (items 2 and 5)
- **Related:** 0002 (closure entries that tier-up replaces), 0003 (ownership inference), 0004 (return points become frames), 0005 (run-format bytecode), 0006 (one code generator), 0008 (background compilation), 0010 (contracts, checked entry points), 0012 (distribution format, linking), 0013 (`vali build`), 0014 (positions), 0015 (the expander's tree), 0018 (library ABI versions)

## Context

The baseline compiler goes from the expander's tree straight to run-format bytecode. It turns loops into jumps by special-casing them and inlines calls to standard procedures, and that is all. The optimising compiler needs an intermediate representation, and that choice drives the rest: how libraries are distributed, what the linker does, what the JIT reads, and how ownership inference (ADR 0003) is expressed.

Several requirements bear on it:

- **Distributed libraries must survive their dependencies being replaced** by another version. Every optimisation that freezes something about another library into distributed code (an inlined body, a folded constant, a resolved method, a type) breaks when that library changes. The JVM's one such optimisation, `javac` inlining `static final` constants, is the cautionary tale in ADR 0012.
- **The whole program is visible when it is loaded.** HotSpot gets its best optimisations by waiting until it can see the classes actually loaded, and recording what it assumed so that a later load can invalidate them.
- **Loading must stay fast.** ADR 0012 #2 keeps conversion from the distribution format to run format a single linear pass, and tier 0 must be able to start at once.
- **Distributed code must be validated quickly**, and stay memory-safe however it was produced (ADR 0012 #9).
- **First-class continuations exist,** and any unknown call may capture one (ADR 0004 #8).
- **A typed variant is planned,** with code specialised per type, falling back to dynamic dispatch where types are unknown.

The design overview and ADR 0004 said the compiler would convert to CPS. CPS with second-class continuations (Kennedy, *Compiling with Continuations, Continued*), A-normal form with join points (Maurer et al., *Compiling without Continuations*) and SSA are known to correspond (Kelsey; Appel): a join point is a continuation that is only jumped to, a join point's parameters are SSA block parameters, and lexical scope is dominance. They differ in how a non-tail call's return point is named, and in whether control flow is nested or a flat graph.

## Decision

### The IR

1. **The IR is A-normal form with join points: scoped SSA.**
   - Every intermediate value is bound once by `let`, and operands are variables or constants, so evaluation order is explicit.
   - `join j(p…) = …` binds a local continuation that is only jumped to and never escapes; `rec join` binds a loop. Join-point parameters play the role of φ-nodes.
   - A non-tail call is `let x = call f(a…)`, whose return point is what follows it. A tail call is `tail f(a…)`.
   - Control flow is nested: `if` tests a variable, and a name is visible only inside the scope that binds it. Dominance is lexical scope, so checking that every use is dominated by its definition is one walk.
   - **Everything compiled goes through it:** libraries, programs, `eval`, the REPL and macro transformers. They differ only in which passes run: evaluated code and REPL inputs get a cheap set (ADR 0013 #13), libraries the full one. There is no second path from the expander to bytecode. Rust procedures, from bridges and host-served libraries, are the only code that never passes through it; the IR refers to them by name.
   - In memory, nodes live in an arena and refer to each other by index, so passes run on worklists and never recurse on the program's nesting depth. A pass that wants a flat control-flow graph or a dominator tree builds one on demand.
2. **Return points become frames when the IR is converted to bytecode.** This is ADR 0004's commitment: continuations are never heap closures at run time. The IR is equivalent to CPS with second-class continuations, but no pass works on a CPS form.

   **As built:** `vali-dist` holds the IR's types, its validator and a printer; `vali-compiler` lowers the expander's tree to it; `vali-bytecode` converts it. Conversion validates the IR, lays each function out with a join point's body before its handler, gives each variable a live interval (to a loop's end if it is bound outside the loop), and assigns slots by linear scan. A call's window goes above every slot live across it and every slot in use while its operands are computed, and operands and results are hinted into it, so arguments are computed in place; parallel moves fill windows and join-point parameters. A comparison that only an `if` tests becomes one branch instruction. Generated code matches the baseline compiler's: the same instructions for `fib`, and benchmarks within noise.
3. **Operations on values are primitive operations defined by the IR's specification:** `car`, `+`, `null?`, `vector-ref` and the rest of the standard procedures the compiler can open-code. They are part of the format, not bodies inlined from `(scheme base)`, so code that uses them depends on the specification and not on any library's implementation. The compiler emits one when the callee resolves to the standard binding (ADR 0015 #16).
4. **Each primitive operation has an effect set:** `pure`, `alloc`, `read`, `write`, `may-raise` and `may-capture`. An unknown call has every effect. Passes move, merge or remove operations only as their effects allow; in particular nothing that reads a box moves across a call that may capture a continuation.
5. **Procedures and closures:**
   - `fun $f(p…) [free…]` is a function, with the free variables its closure holds; `closure $f [v…]` allocates one.
   - `call direct $f(…)` calls a known function without a procedure check or a closure load.
   - `dispatch g(…)` calls a generic function, resolved at run time unless the linker resolves it (item 11).
   - An assigned variable that is captured, or live across a call that may capture a continuation, is a `box`; any other assigned variable becomes join-point parameters, as in SSA construction (ADR 0005 #4 as amended).

   **As built:** lowering boxes every assigned variable, as the baseline compiler did; turning the others into join-point parameters is left to the optimiser. Loops that the compiler's loop analysis finds become `rec join`s at lowering, ahead of general contification. `call direct` and `dispatch` are not built: the optimiser and generic functions need them first. The primitive operations are those the baseline compiler open-coded.
6. **Ownership operations are never stored.** `dup x`, `drop x`, `let r = drop-reuse(x)` and constructors that build in a reuse token's memory (`cons@r`) exist only in the IR that conversion works on. Converting to run format inserts them, from liveness it computes itself, in a linear-time pass over each function, and only under strategies that count references. One extra `drop` in stored code would free an object still in use, so an untrusted artifact must have no way to say one. Distribution code carries each function's parameter conventions, owned or borrowed, only as hints from ADR 0003's ownership inference. A wrong hint costs speed, never safety, because conversion applies one convention on both sides of every call it compiles. Calls to other libraries and unknown calls always use the owned convention, so conventions are not part of a library's ABI.
7. **Every call and primitive that can fail carries a source position**, which conversion writes into the line table (ADR 0014 #9). An inlined call keeps the position it had in the code it came from.
8. **The IR's encoding is the distribution format's code** (ADR 0012). Variables are numbered implicitly as they are bound, so a binding costs no bytes; operands are variable numbers, written as small integers relative to the current binding; imports and constants are indexes into the artifact's reference and constant tables; `fun`, `join` and `if` are nested records with their lengths, so a reader can skip a function without decoding it. There is no stack encoding: the IR names every value, so a stack would save nothing.

### Where each optimisation runs

9. **The compiler optimises each library using only that library's own code.** It needs each library to be expanded on its own, from fresh expansion-time instances and with its own numbering (ADRs 0001 #2 and #4, and 0015 #5). The expander does, as of the work that made each unit expand with its own phase isolates, numbering and scope context. What the compiler does is everything that cannot go stale when a dependency changes:
   - contification of local functions into join points, and loops as `rec join`;
   - inlining, constant folding, copy propagation and dead-code elimination within the library;
   - closure conversion: known calls become direct calls, non-escaping functions are lambda-lifted, closures stay flat;
   - turning assigned variables back into SSA where no continuation can copy them;
   - type inference within the library, for fixnum and flonum fast paths;
   - ownership inference: which parameters can be borrowed, recorded as hints (item 6).

   Distribution code contains only checked operations, and refers to other libraries only by name. It never contains another library's body, constant values, resolved methods or types.
10. **Conversion to run format stays linear-time** (ADR 0012 #2): validating the IR, liveness for slot allocation, inserting ownership operations (item 6), instruction selection, positions. Liveness is computed in two passes over each function, using SSA liveness for reducible control flow (Boissinot et al.); loops are explicit `rec join`s, so control flow is always reducible. Its output is cached per artifact.
11. **The linker optimises across libraries when the program is loaded** (planned, not scheduled), with the versions of every library actually present:
    - inlining other libraries' procedures, and folding their constants;
    - resolving `dispatch` sites to direct calls where the types are known and the generic function's rules guarantee no later method can change the answer; other sites stay dynamic, optionally with an inline cache seeded by the linker;
    - instantiating specialisations of generic typed code, by converting the IR again with concrete types substituted. Each copy is linear to produce, copies are cached by function and type arguments, and their number is bounded, falling back to the shared version past the bound;
    - choosing a contracted export's unchecked entry point where the contract is proven at the call site (ADR 0010 #4).

    The link-time optimiser runs on a background thread. Tier 0 starts at once on each library's own converted code, and each optimised function replaces its entry when ready, as tier-up does (ADRs 0002 and 0008). Its results are cached under the hashes of exactly the libraries they relied on, so replacing a library makes those entries miss and they are linked again against the new version. A host can turn link-time optimisation off, leaving each library's own optimised code.
12. **Every link-time decision is proven,** never speculative (planned, with item 11). A library loaded later (dynamically, or through `eval`) may invalidate one, for example by adding a method to a generic function a site was resolved for. Each optimised function records what it relied on, as HotSpot records its dependencies; invalidating it puts its entry back to the library's own code and relinks it in the background. Frames already running in the old code finish there, and calls made after the invalidation see the new code. Invalidated code stays alive while any frame or continuation refers to it, since re-entering a continuation can return into it.
13. **The distribution format's validator is the security boundary.** Untrusted code arrives as source or as distribution artifacts. Run-format bytecode is produced only by Vali's own converter and linker, from validated IR and facts they derived themselves, and a cached copy is checked against its content hash before it is used. Loading a run-format module directly is not an entry point for untrusted input. Run format is still validated when loaded and keeps every memory-safety check (ADR 0005 #7), as defence in depth; only native code removes checks. The distribution format's validator is what gets fuzzed.
14. **The JIT reads run-format bytecode** (ADRs 0005 and 0006), including the link-optimised code. It rebuilds SSA for each function as it translates it to Cranelift (Braun et al.; Cranelift's frontend does this with `use_var` and `def_var`), which is enough for local work such as keeping flonums unboxed (ADR 0002). Facts that took analysis across functions reach it as instructions: direct calls, resolved dispatch, unchecked entry points, ownership operations. A side table of further facts in run format is added only when a JIT optimisation needs one that no instruction expresses.
15. **`vali build` runs the optimiser over the whole program** (planned, not scheduled), from distribution artifacts (ADR 0013 #2): every library is fixed, so it may seal generic functions and resolve every dispatch site the types allow, then converts to run format and hands it to the shared code generator. It is publishing an application, as `dotnet publish` with ahead-of-time compilation is, not a step in ordinary development.
16. **The optimiser lives in its own crate, `vali-opt`,** used by the compiler for each library and by the loading path for the linker. The IR's types and encoding live in `vali-dist`, since they are the distribution format's code. A host that turns link-time optimisation off, and a standalone executable without `eval`, need not link `vali-opt`'s link-time half.

## Consequences

- Replacing a library never invalidates code distributed by others, beyond the macros they expanded (ADR 0012 #6, as amended): everything else about a dependency is used only at link time, and cached against that dependency's hash.
- Untrusted artifacts can claim nothing that weakens safety: distribution code holds only checked operations and no ownership operations, and the linker introduces unchecked entry points and specialised code only from facts it established itself.
- With link-time optimisation off, code is still better than the baseline compiler's: each library's own code is optimised, and primitive operations are still open-coded.
- The first run of a new combination of libraries runs on per-library code while link-time optimisation runs in the background; later runs read the optimised code from the cache.
- Embedding hosts that compile source carry the optimiser. It is small beside the expander they carry already, and can be left out.
- The run format gains instructions: direct calls, dispatch sites with a link slot each, and `dup`, `drop`, `drop-reuse` and reuse constructors (ADR 0005, as amended). Dispatch sites' slots live in a per-module table, so linking fills tables and never rewrites code (ADR 0005 #10).
- The IR's specification, including every primitive operation and its effect set, is part of the distribution format's specification, and has the same compatibility promise.
- The baseline compiler is replaced, not kept beside it. It stays in use while the IR path is built; once the IR path passes the regression suite, everything switches over and the baseline code generator is deleted.
- Deoptimisation is not needed, because nothing is speculative. Speculation in the JIT would bring it back.

## Alternatives considered

- **A CPS graph of labelled continuations** ("CPS soup", as in Guile): every return point is a label, and contification is a renaming. It is equivalent for passes, but harder to specify as a stable format, inlined bodies need relabelling when spliced, and validating a flat graph needs a dominator computation where a scoped tree needs a walk.
- **Nested CPS terms** (Kennedy's `letcont` and `letval`): close to the paper, but every rewrite rebuilds the tree, and passes recurse on nesting depth.
- **Flat SSA basic blocks** (as in Cranelift or LLVM): close to the code generator, but validating them needs dominators, and the format loses the structure that keeps loading linear.
- **Optimising across libraries before distributing,** with inlining by consent (ADR 0012 #7 before this record): every cross-library fact is frozen into artifacts and checked on load, so replacing a library breaks or refuses its importers, and library authors must decide what to mark inlinable.
- **Speculative optimisation with deoptimisation,** as HotSpot does: more speed on hot code, but it needs profiling and a way to rebuild tier-0 frames from optimised ones. It remains possible in the JIT later.
- **Giving the JIT the IR as a second input:** richer facts, but two inputs to keep consistent, and warm runs would have to keep the IR reachable.

## Left open

- **Generic functions and the typed variant:** the rule that makes resolving a dispatch site final (an orphan rule, sealing declarations or both), whether the typed variant's types are sound inside typed code, and how its annotations are checked on load. Item 11 is built to accept either.
- The exact set of primitive operations and their effect sets, which belongs in the distribution format's specification rather than an ADR.
- The order of passes and their budgets, including the inlining heuristics and the bound on specialisations.
- Speculation and deoptimisation in the JIT.
- Whether the compiler also specialises its own generic code ahead of time, for types it knows will be used.

## References

- Andrew Kennedy, *Compiling with Continuations, Continued*, ICFP 2007.
- Luke Maurer, Paul Downen, Zena M. Ariola and Simon Peyton Jones, *Compiling without Continuations*, PLDI 2017.
- Richard A. Kelsey, *A Correspondence between Continuation Passing Style and Static Single Assignment Form*, IR '95.
- Andrew W. Appel, *SSA is Functional Programming*, SIGPLAN Notices 33(4), 1998.
- Matthias Braun, Sebastian Buchwald, Sebastian Hack, Roland Leißa, Christoph Mallon and Andreas Zwinkau, *Simple and Efficient Construction of Static Single Assignment Form*, CC 2013.
- Benoit Boissinot, Florian Brandner, Alain Darte, Benoît Dupont de Dinechin and Fabrice Rastello, *A Non-iterative Data-flow Algorithm for Computing Liveness Sets in Strict SSA Programs*, APLAS 2011.
- Alex Reinking, Ningning Xie, Leonardo de Moura and Daan Leijen, *Perceus: Garbage Free Reference Counting with Reuse*, PLDI 2021.
- The WebAssembly specification: structured control flow and single-pass validation.
- The HotSpot documentation: class hierarchy analysis and dependency-based invalidation of compiled methods.
- The Julia documentation: method invalidation and world age.
- Chez Scheme's `cp0`, and Racket CS's machine-independent compiled code.
- The Cranelift documentation: the frontend's `FunctionBuilder` and variables.
