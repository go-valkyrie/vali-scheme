# 0015. Scopes and syntax objects

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-17, the expander: written in Rust, syntax objects as Rust values behind one heap type, `syntax-case` matching in Rust, and the core forms (items 12 to 17)
- **Amended:** 2026-11-17, `syntax-rules` stays built into the expander (item 14)
- **Amended:** 2026-11-17, the binding table keeps each symbol's entries by the newest scope in their sets, so resolving stays linear in the program (item 2)
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

   **Entries are kept by a scope, as Racket keeps bindings in their scopes.** Each symbol's entries are filed under the newest scope in their sets. An entry whose set is a subset of an identifier's has that scope among the identifier's, so resolving looks only at the entries filed under the identifier's own scopes. A flat list per symbol made expansion quadratic: every body binding a common name such as `x` added a candidate that every later reference to `x` tested. The no-quadratic-compiler-passes regression found it; compiling 16,000 chained definitions went from 4.2 s to 0.55 s.
3. **One scope set per syntax object; phases live in the binding table.** Each entry records the phases where it is visible: all phases, or only the listed ones. Under implicit phasing nearly every entry is visible at all phases. Only R6RS `for` imports restrict it. Resolution at phase *p* ignores entries not visible at *p*.

   Local bindings are visible at all phases. An identifier that resolves to a local binding from another phase, as when a transformer returns a quoted reference to one of its own locals, is reported as "used out of context", as Racket does. It is never resolved silently to a less specific binding.
4. **Scope kinds are for tools only.** Each scope records what made it: a macro step, a use site, a binding form, a library body, or something else. Debuggers and language servers show this. Resolution never looks at it.

### Representation

5. **Scopes and scope sets are numbered within an expansion context.**
   - **A context per library expansion.** Expanding a library creates a context, and scopes are numbered from zero within it. The numbers depend only on the library being expanded, so expansion stays deterministic.
   - **32-bit numbers, checked.** A scope and an interned scope set are each a 32-bit index into their context. Running out is an error, "too many scopes in one expansion", never a wrap-around. One library would need four billion macro steps and binding forms to reach it.
   - **Scope sets are interned and the operations memoised.** Equal sets get the same index, so comparing sets is comparing integers. Adding, flipping and removing a scope are each cached, so the same operation on the same set computes its result once.
   - **Remapping on load.** A stored syntax object's scopes are numbered densely within its artifact. Loading it allocates fresh scopes in the loading context and remaps them (ADR 0001 #7).
6. **Scope changes propagate lazily.** Adding or flipping a scope on a syntax object records the change at the top. It is pushed down to the children only when the syntax object is taken apart, as in Racket. So a macro that passes a large subform through untouched never copies or walks it. The syntax objects of item 13 do this.
7. **Every syntax object carries its span and its origin:** the macro use that produced it, and through that use, the use that produced it in turn (ADR 0014 #7). The chain is shared, not copied, between the syntax objects one macro step produces.
8. **M0 has a reference implementation for tests only.** The representation of syntax objects (item 13) waited for the runtime in M1. M0 builds the scope machinery, which doesn't depend on the heap, plus a plain Rust syntax type that propagates scopes eagerly. The reference type exists to test the scope machinery against known hygiene examples, and later to check the M1 implementation against it. It is compiled only for tests or with a `reference` Cargo feature, so nothing can come to depend on it.

### Syntax objects at run time

9. **Syntax objects are fully supported at run time.** Phase-0 code can create, take apart and compare syntax objects, and pass them to `eval`, with the same meaning as at expansion time.
10. **Only libraries that need it carry binding data at run time.** A library whose phase-0 code creates syntax objects (with `syntax`, `quasisyntax` or `quote-syntax` at phase 0) stores, with its run-time code, the scopes and binding-table entries those syntax objects can reach. A library that creates none stores nothing. This applies to invoke sections and to standalone executables alike (ADR 0013).
11. **Each library instance creates its expansion context for run-time syntax lazily,** the first time its run-time code needs fresh scopes or must resolve an identifier.

### The expander

12. **The expander is written in Rust,** in `vali-compiler`. Transformers are Scheme procedures, compiled and run on the VM at the phase above the code they expand. Racket's and Chez's expanders are written in Scheme, but Vali's would then run on the tier-0 interpreter during every cold build. In Rust it is fast from the start, needs no bootstrapping, and is the same code the language server uses. Racket's expander (`racket/src/expander`) is the structural reference.
13. **Syntax objects are Rust values, and Scheme sees them through one heap type.**
    - A syntax object holds its content behind an `Arc`, its scope set, its pending scope changes (item 6), its span and its origin. Leaves are datums; any other value, which only `datum->syntax` can put there, is held through a handle.
    - Taking a syntax object apart pushes its pending changes one level down: each child gets a new scope set and the composed pending changes, at constant cost, sharing its content.
    - Scheme code sees a syntax object as a heap object of type `Syntax` that points to the Rust value, so `syntax?` and `identifier?` are a header test. Taking one apart from Scheme allocates a new such object per piece; matching (item 14) avoids most of that.
    - The compiler consumes the same Rust values, so expanded code never round-trips through the heap.
14. **`syntax-case` and `syntax` are core forms, matched and built in Rust,** as psyntax does with `$syntax-dispatch`. The expander turns each pattern into a constant descriptor and a call to one primitive that returns `#f` or the pattern variables' matches, and each template into a call to one primitive that builds the output. Ellipsis depth and misused pattern variables are errors at expansion time. `syntax-rules` is built into the expander on the same matcher and builder: a `syntax-rules` macro runs no Scheme code at all, so it needs no phase-1 instance and expands at Rust speed. `with-syntax`, `quasisyntax` and the derived forms are macros, written in Scheme on top.
15. **The core forms are** `quote`, `quote-syntax`, `if`, `begin`, `lambda`, `let`, `letrec*`, `set!`, the procedure call, `define`, `define-syntax`, `let-syntax`, `letrec-syntax`, `syntax-case` and `syntax`, plus the library forms. `let` is core because the compiler wants it as a node, and a macro producing `((lambda ...) ...)` would only make the compiler rediscover it. `case-lambda`, multiple values and continuation marks join when the runtime has them. Everything else, `cond`, `case`, `do`, `and`, `or`, `let*`, named `let`, `define-record-type`, `parameterize` and the rest, is a macro.
16. **The expander produces the compiler's tree, with every identifier resolved.** References carry their binding, not their name. The compiler inlines a call to a standard procedure only when the callee resolves to that procedure's own binding, which importers cannot assign, so inlining is sound by construction rather than by checking that the program didn't redefine the name.
17. **The standard macros ship as Scheme source inside the binary,** expanded when a context first needs them, until cached artifacts (ADR 0001) make that a load.

## Consequences

- Hygiene follows from one rule (subset and largest set) rather than from renaming. Racket has shown that the rule handles `syntax-case`, `datum->syntax`, definition contexts and module-level bindings.
- Comparing scope sets is comparing two integers, and the cost of a macro step does not grow with the size of the syntax it passes through.
- Numbering per context keeps expansion deterministic. The price is remapping on every load of a stored syntax object.
- Run-time syntax objects cost nothing for programs that don't create them. A program that does create them pays for exactly the binding data its syntax objects can reach.
- M0's tests can check hygiene before the heap exists. The M1 implementation must then pass the same tests.
- Expansion runs at Rust speed except inside transformers. A transformer written with `syntax-case` spends most of its time in the Rust matcher and builder too.
- Two representations meet at the heap boundary. Scheme code that walks syntax by hand, with `syntax-e`-style unwrapping, allocates a heap object per piece; code that uses `syntax-case` does not.

## Alternatives considered

- **Renaming with marks and substitutions** (the R6RS reference expander, psyntax and Chez): proven, but resolution walks wrap lists, and the model is harder to explain and to extend to definition contexts. Sets of scopes are simpler and give the same answers on the standard examples.
- **A scope set per phase on each syntax object,** as Racket keeps for module-level scopes: needed when every phase has separate bindings. With implicit phasing, nearly every binding is visible at all phases, so recording phases in the binding table is simpler and smaller.
- **64-bit scope numbers:** never run out, but double the size of every scope reference in tables and artifacts. Overflow at 32 bits needs a pathological library and is reported, not silent.
- **One context per process, with global scope numbers:** simpler, but scope numbers would then depend on what was expanded before, which breaks determinism (ADR 0001 #4).
- **Eager propagation everywhere:** simplest, and kept for the reference type, but it costs a walk of every subform at every macro step.
- **An expander written in Scheme,** as Racket and Chez have: it would exercise the language and could later be compiled natively, but until then it runs on the interpreter during every cold build, and it needs hash tables and records before it can start. Porting to Scheme later remains possible, since the algorithm does not change.
- **Syntax objects as native heap records:** Scheme code takes them apart without allocating, but the Rust expander would drive them through handles, which a moving collector makes costly, and the compiler would convert them back out of the heap.
- **`syntax-rules` as a macro written with `syntax-case`,** as psyntax and Racket define it: one less built-in form, but every `syntax-rules` use would then run a Scheme transformer, and the matching is the same Rust code either way.
- **`syntax-case` as a macro that expands into unwrapping calls,** as Racket does: no matcher primitive, but each match becomes many small Scheme calls on the interpreter.
- **No syntax objects at run time:** smaller binaries, but `eval` and tools written in Scheme need them, and `syntax-case` lets any phase-0 code quote syntax.

## Left open

- How syntax objects move between expansion contexts at run time, for example from a library instance into an `eval` expansion. They are probably remapped as on load.
- The names and shape of the `(vali syntax)` procedures that expose spans, origins and scope kinds to Scheme code.
- How stored scopes and binding entries are laid out in the distribution format (ADR 0012).

## References

- Matthew Flatt, *Binding as Sets of Scopes*, POPL 2016.
- Matthew Flatt, Ryan Culpepper, David Darais and Robert Bruce Findler, *Macros that Work Together*, JFP 2012: definition contexts and use-site scopes.
- R. Kent Dybvig, *Syntactic Abstraction: The syntax-case expander*, in *Beautiful Code*, 2007.
- The Racket reference, section 1.2.3, "Syntax Objects", and the expander's implementation of lazy scope propagation.
