# 0023. Resource limits

- **Status:** Accepted
- **Date:** 2026-11-18
- **Related:** 0003 (memory management), 0008 (isolates, #6 shared symbols), 0009 (item 21, resource limits), 0015 (scopes and syntax objects), 0016 (Rust objects and the finalize hook), 0020 (constants outside the heap)

## Context

ADR 0009 item 21 gives every evaluation a fuel budget and a memory limit. Only the memory limit is configurable, as `HeapConfig::max_bytes`, and it bounds what the heap takes from the system. Three things grow without that bound seeing them:

1. **Interned symbols and keywords** live as long as their isolate. Each is a heap object, which the memory limit counts, plus an entry in `cx.symbols` or `cx.keywords`: a copy of the name and a table slot, which it does not. A host that interns untrusted input (`string->symbol` on request keys) grows the isolate for good (#18).
2. **Runtime syntax objects** are Rust values in `cx.syntaxes`, behind a one-field heap object. The Rust side is not counted, and under the counting strategies the slot outlives the heap object (#44).
3. **Scope bit sets** that speed up subset tests on large scope sets are capped by a constant, `BIT_SET_BUDGET` (256 MiB), per `ScopeContext`. A build makes several contexts and an isolate that runs `eval` makes more, so the real total is a multiple of it, and a host can neither lower nor see it (#16).

The caps for 1 and 2 must hold under every memory strategy, `NoCollection` included, where nothing is ever freed, so they cannot wait for a weak symbol table (#46). #16 asks for the same kind of knob. The host should configure them in one place.

## Decision

1. **One configuration type holds every limit.** The type is `HeapConfig`, which the host passes to `Isolate::new`, `Cx::new` and `Repl::new`, and the `vali` facade re-exports. Its name predates the caps, and a rename can come later. Each field is an `Option`, and `None` means unlimited, except where a default already exists.

   | Field | Bounds | Default |
   |---|---|---|
   | `max_bytes` | Heap pages and large objects (ADR 0009 item 21), plus the Rust-side bytes of symbols, keywords and syntax objects (item 3) | unlimited |
   | `max_symbols` | Interned symbols and keywords together, including those the isolate interns for itself (item 7) | unlimited |
   | `max_syntaxes` | Runtime syntax objects alive at once | unlimited |
   | `scope_bit_set_bytes` | Scope bit-set memory, shared by everything the isolate or a build expands | 256 MiB, today's `BIT_SET_BUDGET` |

   Fuel stays a per-call option (ADR 0026 #9), since a host sets it per run and not per isolate; the one isolate-wide fuel setting, `metered`, which fuel limits need (ADR 0026 #8), is a field here and defaults to `false`.

2. **A cap is checked on a miss, before anything is allocated.** `intern`, `keyword_raw` and `syntax_raw` look the name up first, and a hit costs nothing. A miss that would pass `max_symbols` or `max_syntaxes`, or the memory limit counting the Rust-side bytes below, fails before it allocates. A call that makes one object, `string->symbol` or a read, leaves the isolate as it was. A module load that reaches a cap partway fails cleanly, keeping what it already built: the constants of ADR 0020 are never freed, and the next load does not need to rebuild them.

3. **What is charged.** `max_symbols` and `max_syntaxes` count entries. The memory limit also counts what the heap cannot see: for a symbol or keyword, the name's bytes, the key's box and the table entry; for a syntax object, its slot in `cx.syntaxes`. These are fixed estimates per entry, not measurements of the allocator, and a freed syntax slot gives its estimate back.

   The charges live in the heap's page source, next to its own accounting of the pages it has handed out. A page is refused, and a charge fails, when the pages and the charges together would pass `max_bytes`, so the two never pass it between them. The heap's empty pages are what make room: a counting heap keeps the pages it has emptied for reuse, and when a charge does not fit next to the pages, it gives empty pages back to the source, one at a time, until it does. A heap that was full and has been emptied therefore still takes a new symbol, and one with no empty page to give fails the charge with `OutOfMemory`.

   A syntax object's Rust memory is freed only at an epoch, so under the counting strategies it is also counted as allocation, with `S::charge`, so that epochs come as often as it needs: with a cap, twice per cap's worth of objects. A cap reached while the isolate is idle runs an epoch and counts again before it fails, so the cap means live objects. While Scheme code runs, the epochs at calls do that.

4. **At a cap, a catchable Scheme error.** The error is `Error::LimitExceeded(Limit)`, with `Limit` naming the cap (`Symbols`, `Syntaxes`). Scheme handlers see an error object whose message says which limit was reached, and a host whose program did not handle it receives the typed error, as it receives `OutOfFuel` and `OutOfMemory`. It differs from those two in being **catchable**, because it leaves the isolate consistent and a program may reasonably report it or refuse the request that asked for the symbol. The memory limit and fuel keep ADR 0009 item 21's behaviour: not catchable, no `dynamic-wind` after-thunks run. A memory limit reached through the Rust-side charge is still `OutOfMemory`.

5. **The isolates that run macros share the caps.** The compiler makes isolates for phase-1 code, and an `eval` or REPL input that expands a macro runs it in one. They take `max_symbols` and `max_syntaxes` from the host's isolate, each counting its own tables, so a transformer cannot intern past what the host allowed. Compiling a program before any isolate exists is not capped by them.

6. **The scope budget is shared.** `scope_bit_set_bytes` is one counter shared by every `ScopeContext` a build makes and by every one an isolate's compiler makes at run time. `ScopeContext` already keeps its count in an atomic; sharing it is plumbing. A context's bit sets are returned to the budget when the context is dropped. The build entry points take the same type and read the fields that concern a build, and an isolate's `eval`, `environment` and REPL draw from the budget its configuration set. The field and the sharing are not wired up yet: `ScopeContext` still has its constant `BIT_SET_BUDGET` (#16). Past the budget, subset tests fall back to merging sorted lists, which is correct and slower, so this limit degrades speed and never raises an error.

7. **Weak tables count live entries.** When symbols or syntax objects are freed (#46 for symbols; syntax objects already are, #44), the caps count live entries and not every entry ever made. Until then `max_symbols` counts every symbol ever interned, and a long-lived isolate that interns without bound will meet it for good. That is the guarantee for now: a bound the host chooses. The meaning of the field does not change when the table becomes weak, only how long an entry counts. The count includes everything the isolate interns for itself: the symbol constants of every library it loads, the names of record types, keywords. A host sets the cap relative to its starting point, using `Cx::symbol_count`, the live count the cap is compared with.

## Consequences

- `max_bytes` counts the Rust-side estimates as well as the pages, so a host tuned close to its usage can reach `OutOfMemory` sooner.
- The runtime interns the few names of its own error-object type outside `max_symbols`, so a handler can always be given the error a cap raised.
- `cx.syntaxes` is a slab: when the strategy frees a syntax object's heap object, the finalize hook empties its slot for reuse, as it does for Rust objects (#44), and so does cycle collection, through the same finalize function. Under `NoCollection` nothing is freed, so the cap is the bound there.
- A host that interns untrusted input can bound it under every strategy.
- `HeapConfig` gains fields, so code that builds one with a struct literal and no `..Default::default()` stops compiling.
- `intern` and `keyword_raw` can fail with an error that is not `OutOfMemory`, so loading a module that interns past the cap fails the load with that error.
- The Rust-side charge is an estimate. A host that needs exact accounting of those tables has none.

## Alternatives considered

- **One byte budget for everything Rust-side.** Simple to state, but a host reasons about "how many symbols may a request make", and bytes depend on name lengths. Counts are what the issue asked for, and the byte charge goes to the memory limit.
- **Make the caps uncatchable like fuel and memory.** Rejected: nothing is left half-done at a cap, and a handler that reports it is useful.
- **A separate `Limits` type beside `HeapConfig`.** Two configuration arguments for one isolate, and strategies would take both. Rejected for now.

## Left open

- Depth limits for expansion and nesting (#5), and `ScopeContext::with_limit`'s scope count, are not host-configurable yet. They belong in this type when they are. The same goes for the datum labels a syntax export may define (16,384, fixed in the artifact decoder and encoder), which ADR 0001 #7 describes.
- Freeing the syntax data `eval` merges with its module (#45) and the weak symbol table (#46).

## References

- #16, #18, #43, #44.
