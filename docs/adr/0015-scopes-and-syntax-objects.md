# 0015. Scopes and syntax objects

- **Status:** Accepted
- **Date:** 2026-11-16
- **Related:** 0001 (phasing, determinism, stable scope identities), 0007 (`syntax-case` and the syntax procedures), 0012 (distribution format), 0013 (standalone executables, `eval`), 0014 (spans and macro origins)

## Context

Vali needs hygienic `syntax-case`, procedural macros exported from libraries, and implicit phasing with R6RS `for` imports as an override (ADR 0001). The expander needs a representation of binding that:

- **decides what every identifier refers to,** including identifiers a macro introduces, identifiers passed to a macro, and identifiers built with `datum->syntax`;
- **is deterministic.** Scope identities come from the library being expanded, never from global counters (ADR 0001 #4);
- **can be stored.** Syntax objects in visit sections and static information keep stable scope identities, remapped on load (ADR 0001 #7);
- **carries spans and macro origins** for errors, debuggers and language servers (ADR 0014 #7 and #8);
- **is cheap on large programs.** Implementations that copy or rename syntax eagerly at every macro step have quadratic cases, which the regression list tests for.

Syntax objects are values that macros manipulate. Most exist only at expansion time, but phase-0 code can create them too: a program that calls `eval`, or a tool written in Scheme, builds and compares syntax at run time.

## Decision

### Binding as sets of scopes

1. **Binding uses sets of scopes** (Flatt, POPL 2016). A scope is an opaque identity. Every syntax object carries one set of scopes. Expansion adds scopes for each binding form, flips a fresh scope on a macro's input and output to tell introduced identifiers from passed ones, and adds use-site scopes where definitions and uses share a context.
2. **The binding table maps a symbol and a scope set to a binding.** Resolving an identifier finds every entry for its symbol whose scope set is a subset of the identifier's. The entry whose set is a superset of all the others wins. If there is no such entry, the reference is ambiguous, which is an error that names the candidates.
3. **One scope set per syntax object; phases live in the binding table.** Each entry records the phases where it is visible: all phases, or only the listed ones. Under implicit phasing nearly every entry is visible at all phases. Only R6RS `for` imports restrict it. Resolution at phase *p* ignores entries not visible at *p*.

   Local bindings are visible at all phases. An identifier that resolves to a local binding from another phase, as when a transformer returns a quoted reference to one of its own locals, is reported as "used out of context", as Racket does. It is never resolved silently to a less specific binding.
4. **Scope kinds are for tools only.** Each scope records what made it: a macro step, a use site, a binding form, a library body, or something else. Debuggers and language servers show this. Resolution never looks at it.

### Representation

5. **Scopes and scope sets are numbered within an expansion context.**
   - **A context per library expansion.** Expanding a library creates a context, and scopes are numbered from zero within it. The numbers depend only on the library being expanded, so expansion stays deterministic.
   - **32-bit numbers, checked.** A scope and an interned scope set are each a 32-bit index into their context. Running out is an error, "too many scopes in one expansion", never a wrap-around. One library would need four billion macro steps and binding forms to reach it.
   - **Scope sets are interned and the operations memoised.** Equal sets get the same index, so comparing sets is comparing integers. Adding, flipping and removing a scope are each cached, so the same operation on the same set computes its result once.
   - **Remapping on load.** A stored syntax object's scopes are numbered densely within its artifact. Loading it allocates fresh scopes in the loading context and remaps them (ADR 0001 #7).
6. **Scope changes propagate lazily.** Adding or flipping a scope on a syntax object records the change at the top. It is pushed down to the children only when the syntax object is taken apart, as in Racket. So a macro that passes a large subform through untouched never copies or walks it. This arrives with the heap representation of syntax objects in M1.
7. **Every syntax object carries its span and its origin:** the macro use that produced it, and through that use, the use that produced it in turn (ADR 0014 #7). The chain is shared, not copied, between the syntax objects one macro step produces.
8. **M0 has a reference implementation for tests only.** The heap representation of syntax objects waits for the runtime in M1. M0 builds the scope machinery, which doesn't depend on the heap, plus a plain Rust syntax type that propagates scopes eagerly. The reference type exists to test the scope machinery against known hygiene examples, and later to check the M1 implementation against it. It is compiled only for tests or with a `reference` Cargo feature, so nothing can come to depend on it.

### Syntax objects at run time

9. **Syntax objects are fully supported at run time.** Phase-0 code can create, take apart and compare syntax objects, and pass them to `eval`, with the same meaning as at expansion time.
10. **Only libraries that need it carry binding data at run time.** A library whose phase-0 code creates syntax objects (with `syntax`, `quasisyntax` or `quote-syntax` at phase 0) stores, with its run-time code, the scopes and binding-table entries those syntax objects can reach. A library that creates none stores nothing. This applies to invoke sections and to standalone executables alike (ADR 0013).
11. **Each library instance creates its expansion context for run-time syntax lazily,** the first time its run-time code needs fresh scopes or must resolve an identifier.

## Consequences

- Hygiene follows from one rule (subset and largest set) rather than from renaming. Racket has shown that the rule handles `syntax-case`, `datum->syntax`, definition contexts and module-level bindings.
- Comparing scope sets is comparing two integers, and the cost of a macro step does not grow with the size of the syntax it passes through.
- Numbering per context keeps expansion deterministic. The price is remapping on every load of a stored syntax object.
- Run-time syntax objects cost nothing for programs that don't create them. A program that does create them pays for exactly the binding data its syntax objects can reach.
- M0's tests can check hygiene before the heap exists. The M1 implementation must then pass the same tests.

## Alternatives considered

- **Renaming with marks and substitutions** (the R6RS reference expander, psyntax and Chez): proven, but resolution walks wrap lists, and the model is harder to explain and to extend to definition contexts. Sets of scopes are simpler and give the same answers on the standard examples.
- **A scope set per phase on each syntax object,** as Racket keeps for module-level scopes: needed when every phase has separate bindings. With implicit phasing, nearly every binding is visible at all phases, so recording phases in the binding table is simpler and smaller.
- **64-bit scope numbers:** never run out, but double the size of every scope reference in tables and artifacts. Overflow at 32 bits needs a pathological library and is reported, not silent.
- **One context per process, with global scope numbers:** simpler, but scope numbers would then depend on what was expanded before, which breaks determinism (ADR 0001 #4).
- **Eager propagation everywhere:** simplest, and kept for the reference type, but it costs a walk of every subform at every macro step.
- **No syntax objects at run time:** smaller binaries, but `eval` and tools written in Scheme need them, and `syntax-case` lets any phase-0 code quote syntax.

## Left open

- The heap representation of syntax objects and of pending scope changes (M1, with ADR 0003).
- How syntax objects move between expansion contexts at run time, for example from a library instance into an `eval` expansion. They are probably remapped as on load.
- The names and shape of the `(vali syntax)` procedures that expose spans, origins and scope kinds to Scheme code.
- How stored scopes and binding entries are laid out in the distribution format (ADR 0012).

## References

- Matthew Flatt, *Binding as Sets of Scopes*, POPL 2016.
- Matthew Flatt, Ryan Culpepper, David Darais and Robert Bruce Findler, *Macros that Work Together*, JFP 2012: definition contexts and use-site scopes.
- R. Kent Dybvig, *Syntactic Abstraction: The syntax-case expander*, in *Beautiful Code*, 2007.
- The Racket reference, section 1.2.3, "Syntax Objects", and the expander's implementation of lazy scope propagation.
