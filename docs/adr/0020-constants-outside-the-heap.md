# 0020. Constants live outside the isolate heap

- **Status:** Proposed
- **Date:** 2026-11-18
- **Related:** 0002 (value representation, #7, #8, #10, #11), 0003 (memory management, the strategy contract), 0005 (bytecode, #4 constants), 0006 (ABI version), 0008 (isolates; #6 shared literals and symbols), 0009 (resource limits), 0012 (distribution format), 0015 (syntax objects), 0017 (the validator)

This record started as the question "where does the immutable bit of a literal pair live" (#60) and was widened to one mechanism for every kind of constant. It also settles what #59 and #66 (literal vectors) and the existing literal strings and bytevectors do, and takes up the question that falls out of it: what a constant that mentions a symbol points to.

## Context

R7RS makes literal constants immutable: mutating one is an error, though an implementation need not signal it. Vali enforces it for literal strings and bytevectors with the `data::IMMUTABLE` bit in the object's header, #66 does the same for vectors, and pairs are the open case (#60): most pairs have no header (ADR 0002 #8). #17 removed the optimiser's `car` and `cdr` folds because literals could change under it.

As built, every constant of a module is **allocated in the isolate's heap** when the module loads (`Cx::load`, `fn constant` in `vm.rs`) and held there for the isolate's life. That has three consequences:

1. Immutability has to be a property of an *object*, set after allocation, wherever the object has room for it. A pair has none under no collection or side counts.
2. Constants are copied into every isolate, though ADR 0008 #6 says immutable literals are shared. ADR 0008 #6 and the doc comment of `code.rs` also say the loaded module is "shared and immutable once built, outside every heap". In fact `Cx::load` builds a fresh `Loaded` per call, so nothing is shared yet.
3. Counting pays for them. Each constant is a counted heap object, and every `car` of a quoted list that is stored into a heap object counts a reference to an object that cannot die.

### What the code looks like now

- `Constant` has these kinds: fixnum, flonum, boolean, character, `()`, unspecified, string, symbol, keyword, pair, bignum, ratnum, complex, bytevector, vector. No record, closure, code or syntax object. **Symbols and keywords are the only kinds that are isolate-specific**: `fn constant` calls `intern` and `keyword_raw`, which look in the isolate's table and `hold` what they allocate for the isolate's life.
- A pair constant is built by recursion, so every pair of a quoted tree is made there. A vector constant holds constants.
- `read`, the macro phases (`datum_raw`), host bridges and `syntax->datum`'s inverse all build fresh, mutable pairs through `cons_raw`. Quasiquote expands to `cons`. The SRFI 1 and 132 `!` procedures are aliases of the non-destructive ones. These need no change, and they must not start sharing structure with constants.
- Pages are 32 KiB, aligned to 32 KiB, with a header word for the counting heap's space (`page_heap.rs`) and none under no collection (`no_collection.rs`). Objects over 8 KiB are *large*: allocated one by one, 8-byte aligned, with **no page and no page header**. Nothing moves objects.
- Strings keep their bytes in a separate bytevector object, and the string has a hints field (ADR 0002 #11), unused so far. Syntax objects are Rust values that name identifiers by string, not by runtime symbol, so the expander does not depend on symbol identity.

### Where IMMUTABLE is set today

`data::IMMUTABLE` is set by `make_immutable` on literal strings and bytevectors, and, through `string_raw(.., true)`, on: the name string of every symbol and keyword (so `symbol->string` returns an immutable string), a few error-message strings (`vm.rs`), the string `keyword->string` returns, and `symbol->string`'s result (`strings.rs`). #66 adds literal vectors. So the flag has two jobs: "this is a constant" and "this runtime-made object is read-only". Only the first is what this record replaces.

## Design

**Constants are built into pool pages that belong to the loaded module, not to any heap. A pool page's header says so, and a write to an object on a pool page is an error. Reads, and everything else the VM and primitives do with a constant, are unchanged.**

### 1. What a pool page is

- **The page header.** Every page that can hold a pair starts with a header, and no object starts at the page's first word. The header gains a field, `frozen`, at a fixed offset: nonzero on a frozen page, zero on every other. The write check tests it. It is a field of its own and not a value of the existing first word, because that word is `Page::space`, which `free` uses to find the page's size class (`self.classes[p.space][p.class]` in `page_heap.rs`), and heap pages hold 0, 1 or 2 in it, so a comparison against "heap" would be a range or bit test. Under no collection, whose pages have no header today, each page gains one.
- **Frozen pair pages.** Under M2 (section 3) only pairs need the page check, so the pages in question are *frozen pair pages*: one size class, which is what makes 32 KiB the right page size, and one object size under each layout (16 bytes, or 24 with a header). A frozen pair page is either a **pool page**, built with the module and outside every heap, or a **frozen heap page** (section 2). Both answer the write check the same way. A pool pair page under side counts lays out `blocks`, `block_bytes`, `side` and `stride` as `Page` does, because `side_byte` reads them, and presets the count bytes to 255. Pool pages for everything else (strings, vectors, numbers) need only a header that says what they are: objects with a header are frozen by the header flag, not by a page check.
- **Allocator rules for frozen heap pages.** A frozen page never serves an ordinary allocation: it is never a size class's current page, never on the list of pages with free blocks that `allocate` pops, and its free list is never used by `cons`. Literal pairs are allocated from a class of their own. `free` returns a block to its page's free list as now, and when a frozen page empties, its `frozen` field is cleared *before* the page joins the list of empty pages that any class takes. Pool pages are not in the heap's lists at all.
- Objects on a pool page have **exactly the layout the strategy gives the same object in the heap**: a pair is `car`, `cdr` (and a header under headered counting), a flonum its `f64`, a vector a header, a length word and elements, and so on. So `car`, `cdr`, `vector-ref`, `string-ref`, `pair?`, `eq?` and every list walk run unchanged: they read, and a read of a pool object needs no check. Type tests that read a header read it where it always is.
- **Headers**: an object that has one in the heap has one in the pool, with the type code and the runtime data as built. The strategy's half is frozen: zero under no collection; under counting a count at its maximum, no colour, no flags.
- **What the strategies need from a pool object** (a pool object is built once and then only ever read, even by the isolates that may share it later):

| Strategy | Pool pair | What must hold |
|---|---|---|
| No collection | 2 words, no header | Nothing. Pool pages are never freed while the module lives |
| Counting, headered | 3 words, header first | Count word at its maximum (`COUNT`, the low 28 bits of the strategy half). **`increment` is `*Self::half(v) += 1` as built, which carries out of the count into the colour bits that start at bit 28, so the first reference a heap object took to a pool constant would corrupt it; and `survived` recognises a stuck count only for side counts, so a pool object whose count dropped would be buffered as a cycle candidate.** Increment and decrement therefore test for the sticky value, as side counts do: increment adds unless the count is at the maximum, decrement returns "stuck" at the maximum. This is in stage 1, not only once pools are shared, and it gives the property that a pool object is never a candidate. A heap object whose count reaches the maximum becomes immortal too, as a side-counted pair does at 255 |
| Counting, side counts | 2 words, side table on the pool page | Count bytes preset to 255. `increment` and `count_down` already skip a sticky byte without writing. The flags byte stays zero, because every path that writes it is guarded by "not stuck" (below) |
| Tracing (future) | 2 words, no header | Pool pages are outside the collected spaces. **A pool object never points into the heap** (the one exception, symbols and keywords, is in the Symbols section), because a write into it is an error, so the tracer needs no remembered set and roots need not include pool pages. A moving tracer finds the isolate's heap objects by tracing and never visits a pool object. Identity hashes of pool objects must come from the address, not a field in the header, because the header is not written. Under MMTk the page-header invariant of section 3 does not hold, and the check becomes an address-range test |

- **Cycle collection treats an object with a stuck count as a leaf.** Trial deletion reaches pool objects from heap candidates. As built, `mark_gray` (`cycles.rs`) decrements every child, colours it gray and pushes it whatever its count, `scan_black` colours each child black and increments it, and `collect_white` visits children by colour. A pool object would be coloured (in its header under headered counting, in its flags byte under side counts) and its whole constant graph walked on every collection; with shared pools that is a write to a shared page. So in all three, **a child whose count is stuck is skipped: no decrement or increment, no colour, no push.** `collect_white` then never meets one, since an uncoloured object is never white. This is sound because an object that is never freed keeps alive everything it reaches: its references to its children are never subtracted, so the children keep the counts that include it. It changes nothing for a heap object whose count stuck, which trial deletion could never have freed. It needs a `stuck(v)` query next to `Counted::count`, and ADR 0003's contract row says so.
- **Epochs and `free`** never see a pool object: the heap did not allocate it, so it is never in the zero-count table.
- **Reads must not write.** Two things in the design write on read: the strings' index hints (ADR 0002 #11, not built) and identity-hash state. A pool string must carry complete hints, built with the pool, or none and a scan; hints are never installed into a pool object.

### 2. How the pool is built

- **From the module's constants, once per load.** `Cx::load` today converts each `Constant` into heap objects. It instead calls a pool builder, `Pool::build::<S>(&Module)`, which allocates every constant into pool pages in a bump region of its own and returns the table of constant words that `Instance::constants` holds now. Nothing is `hold`-ed and nothing is counted. The builder is per strategy, like the run-format bytecode (ADR 0003 #2, ADR 0006): the pool is part of the run format, never shipped, and keyed by the ABI version. The distribution format does not change (ADR 0012).
- **Packing.** Objects are packed in order of construction, children before parents, into pages of their kind: pair pages (with a side table under side counts, which needs a fixed stride), and general pages for everything else. Nothing needs a size class or a free list: a pool never frees. An object larger than the rest of a page starts a fresh page *at the page's start plus the header*, so any pool object's address masked to a page boundary is its page. A large pool object spans consecutive pages and its pointer masks to the first.
- **Owner and lifetime.** The pool belongs to the loaded module (`Loaded`). While `Loaded` is per isolate, as built, the pool is too, and the pages are charged to the isolate's limit like heap pages. If `Loaded` is shared by `Arc` (ADR 0008 #6), the pool is shared with it, built once per process per strategy and freed with the last reference. Sharing needs the decision on symbols in the Symbols section below.
- **Unloading, and #31.** Eval modules stay loaded today and #31 will free a module "once nothing refers to it". Constants break that test: a heap object can hold a pointer to a pool constant (`(cons 1 '(2 3))`), and a value can escape to a handle, and pool objects are not counted, so nothing can say when the last reference died. **A pool can only be freed with the isolate.** That is fine for libraries and programs, but `eval` in a loop (the case #31 exists for) would then keep every evaluation's constants for ever. So the design has two kinds of frozen page:
  - **Pool pages** for libraries and programs: outside the heap, immortal for the isolate (or the process, if shared).
  - **Frozen heap pages** for `eval` and REPL input: ordinary counted objects on frozen pair pages in the heap (section 1) and, for headered objects, with the header flag. They are held by the module's constant table and freed with it by #31, like the heap objects of today. This is the page bit on literal pages that an earlier draft of this record measured, and it costs 32 KiB per isolate that has any such constant.

  Both give the same answer to the write check: the page's `frozen` field is set.
- **Memory limits (ADR 0009).** Pool pages count toward `bytes_used` while each isolate has its own. If they are shared later, they are charged to nobody, which an embedder that sandboxes by memory limit should know.

### 3. The write check, and what it says about large objects

The check measured below, for pairs: clear the low 15 bits of the object's address, load the page's `frozen` field, branch. 4 instructions and 1 load. Writes check; reads do not. The write paths are listed in section 7.

**The invariant the check imposes on every heap.** It is correct only if every space that can hold a pair has a header at the start of each 32 KiB-aligned region, with no object starting there. That holds for `page_heap` and, with this record, for `no_collection`. It does not hold for an MMTk Immix space, whose block metadata lives on the side and whose objects start at the block's start. ADR 0003 states the invariant, and a tracing strategy under MMTk replaces the page-header test with a test of whether the address is in the VM space that holds the frozen pairs.

There is a catch the pair measurements did not show. **Vectors, strings and bytevectors can be large**, and a large heap object has no page and no page header, so masking its address reads memory that is not a header: an unmapped page, or by chance a word equal to the pool kind. Two ways out:

- **M1, one mechanism, the page word for every kind.** Large heap objects get a page of their own: allocated 32 KiB-aligned with the header at the start, the object just after. Costs an over-aligned system allocation for every object over 8 KiB, up to 32 KiB of address space of padding each, most of it never touched.
- **M2, the page word for pairs only, and the header bit for objects that have a header.** A vector, string or bytevector already has a header, so "frozen" is one bit of it, which the pool builder sets and which `mutable_string`, `mutable_bytevector` and #66's vector check already test. Pairs are never large and have no header, so they use the page. The check on a headered object is 2 instructions on a word that is in the object's first cache line.

M2 is decided (see Decisions), for three reasons: it needs no allocator change; it is what #66, now merged, does, so nothing is thrown away; and it is cheaper for the objects that have a header. It does not reintroduce the problem this record set out to remove, because the bit is set only by the pool builder (and for the runtime-made immutable objects below), never by a path that has to remember to. The cost of M1 is the large-object allocator change and nothing else.

**An alternative recorded and not decided: a reserved address range.** Reserve one virtual range (`mmap` with no access, committed as pool pages are built) and put every pool object in it. The check is then `addr - base < len`: two instructions on a value already in a register and no load, so the scattered-mutation cost measured below disappears. It works for every kind and size of object, including large ones, which is M1's single mechanism with no over-aligned large objects and no header on every page. Its costs: an address-space reservation, a hard cap on the pool's size, a platform dependency, and the page-header check kept as the fallback on wasm32, which cannot reserve. It covers pool pages only, so frozen heap pages (section 2), which are ordinary heap pages, would still need the page field or the header flag. Whether it is worth that, for removing a cost that is lost in noise on contiguous pairs, is a later decision, to be made with measurements on a real workload.

### 4. What happens to `data::IMMUTABLE`

It cannot go yet, and under M2, the decision, it stays as the pool's mark on headered objects. Besides literals, these read-only objects carry it:

- **`symbol->string` and `keyword->string` results.** Each call makes a fresh immutable copy of the name (`strings.rs`), so the result is a heap object that is read-only for a reason that is not "it is a constant". The name string inside a symbol or keyword is made immutable the same way.
- **Error-message strings** the VM makes.
- Literal vectors, strings and bytevectors, which the pool now owns.

Under M1 the flag could go once symbol names are frozen by location. Under M2, which is decided, **it stays, with one meaning: "this object is frozen".** Error-message strings stay immutable, as do `symbol->string` and `keyword->string` results. The flag is no longer set by `make_immutable` on a heap object after the fact: the pool builder sets it, and the runtime sets it on the few objects above when it makes them.

### 5. What a constant may point to

**A pool object points only to other pool objects.** That is what lets the tracer and the sticky counts ignore the pool. Every constant kind is self-contained, except two:

- **Symbols**, which `fn constant` interns in the isolate's table. A quoted list of symbols is the commonest constant there is.
- **Keywords**, which are interned the same way.

Everything else a constant holds is data: numbers, strings and their byte objects, bytevectors, vectors, pairs. There are no records, closures, code objects, ports or syntax objects in a constant, and constants cannot be cyclic.

So the whole question of sharing reduces to symbols (and keywords, which follow whatever symbols do), covered in the Symbols section below.

### 6. Semantics

- **`eq?` of literals.** One literal is one object per module, as #61 pins: a `quote` always yields the same pool object, and two equal literals in one module are distinct objects. If the pool is shared, the object is the same word in every isolate. That is not observable: isolates exchange only copies (ADR 0008 #5), and a copy of a constant is a new, mutable object. Equal constants in two different modules stay distinct, as R7RS allows. A later pass could merge equal immutable constants across modules; it is not needed.
- **`read`, `syntax->datum`, quasiquote, `list-copy`** make fresh mutable objects, as above. A macro that builds a pair at phase 1 and quotes it produces a *copy* in the module's pool; the phase-1 original stays mutable.
- **Strings.** A pool string is read-only by location, and every mutator already refuses an immutable string. The bytes of a pool string are a pool bytevector, so replacing them (`set_string_text`) is never reached.
- **Artifacts and the round trip.** Unchanged: the format stores `Constant`s, and the pool is built from them at load.

### 7. Every mutation path that must check

| # | Path | Where | Reaches a constant? |
|---|---|---|---|
| 1 | `set-car`, `set-cdr` instructions | `vm.rs` | Yes |
| 2 | `vector-set!` instruction | `vm.rs`, the `VectorSet` arm (checks bounds and type; gains the check) | Yes |
| 3 | `set-car!`, `set-cdr!` primitives | `primitives.rs` | Yes: not inlined, `apply`, procedure values, SRFI 1 re-exports. `list-set!` goes through them |
| 4 | `vector-set!`, `vector-fill!`, `vector-copy!` (destination), `vector-sort!`, `vector-merge!` and the other vector mutators | `primitives.rs`, `base.sld`, `srfi/132.sld`; #66 adds `%check-mutable-vector` | Yes |
| 5 | `string-set!`, `string-fill!`, `string-copy!` (destination) | `strings.rs` `mutable_string` | Yes |
| 6 | `bytevector-u8-set!`, `bytevector-copy!` (destination) | `strings.rs` `mutable_bytevector` | Yes |
| 7 | Host `Handle::set_car`, `set_cdr`, `vector_set` | `cx.rs` | Yes: a host can hold a constant it got from Scheme. Needs an error to return |
| 8 | `Cx::set_car`, `Cx::set_cdr`, `Cx::vector_set` | `cx.rs` | `unsafe` and unchecked, used by the reader's cycle patching and by builders on fresh objects. Documented as for fresh objects |
| 9 | A future inline `set-car`, `vector-set!` in native code | `vali-codegen` | Yes. This is not a memory-safety check, so tier-up must not drop it (ADR 0005 #7 removes only those) |
| 10 | `define-values`, `circular-list`, `force`, SRFI 1 and 132 `!` procedures | `base.sld`, `srfi/*.sld`, `lazy.sld` | No: each mutates an object it just made, or is a non-destructive alias. This holds only while no optimiser pass folds an allocation (`cons`, `list`, `vector`) into a constant. **A rule to write down and test** |
| 11 | Port reads into a buffer | `ports.rs` | None exist that write into a caller's bytevector or string today. A new one would be a mutator |
| 12 | Cycle collection, epochs, freeing | `cycles.rs`, `epoch.rs` | Never write a field; counts only, and a pool object's are sticky |

## Symbols

Everything above works with a per-isolate pool whatever symbols do. Sharing a pool between isolates needs one more answer: **what word is a symbol in a constant, and is it the word `string->symbol` returns in every isolate?** `eq?` on symbols is word equality (ADR 0002 #4), and a pool constant is built before any isolate runs.

ADR 0008 #6 already sketches an answer (option B): a global table of the symbols that appear in compiled code, and a table per isolate for symbols interned at run time, with `string->symbol` looking in the global one first. This section works out what that costs and what the alternatives are.

| | A. One process-wide table | B. Pool symbols plus per-isolate tables (0008 #6) | C. Symbols as immediates | D. Per-isolate pools | E. Share symbol-free constants only |
|---|---|---|---|---|---|
| **How it works** | All interned symbols live in a global table, outside every heap. A pool symbol is a global symbol | The global table holds symbols that modules' pools mention. `string->symbol` looks there first, then in the isolate's own table, else makes a local symbol | A symbol is an immediate: a subtag and an id in the word. Constants hold ids, no pointers | Each isolate builds its own pool for each module, pointing at its own symbols (what loading does today, with pages) | The pool splits. Constants that reach no symbol (strings, numbers, bytevectors, symbol-free lists and vectors) are shared. Any constant that reaches a symbol, with every parent of it, is built per isolate |
| **`eq?` and identity across isolates** | One word per name, process-wide | One word per name if the module loaded before anyone interned the name locally. Otherwise two (the hard case below) | One id per name if issued from one table; the same hard case if there are two id spaces | Per isolate, as today | Per isolate for symbols, as today |
| **`string->symbol`** | Hash, shared lock or lock-free read, on every call | One more probe (the global table, lock-free read), then the local one | Table lookup for the id | As today | As today |
| **`symbol->string`** | One load | One load | Table lookup (id to name): 2 loads and a branch | One load | One load |
| **Comparison** | Word | Word | Word | Word | Word |
| **Memory** | One copy per name, forever | One copy of compiled-in names, plus a local copy of each other name in each isolate | One name per id | A copy per isolate | Shares the heavy data; copies the symbol-bearing part |
| **Freeing symbols** | Never, unless the table is weak and every isolate reports liveness. **A `string->symbol` of untrusted input grows a process-global table until the process ends: a denial of service the embedder cannot attribute or cap.** ADR 0008 chose per-isolate tables for runtime symbols for exactly this | Global table: names from loaded libraries and programs only, which a host chose. Runtime names stay local, freed and limited with the isolate. `eval` of untrusted text must not add to the global table: its constants take the frozen-heap-page route (section 2) and use local symbols | Ids can never be freed: a value in a heap holds one and nothing traces it | With the isolate | With the isolate |
| **Threads** | Sync on every intern and, unless lock-free, every lookup | Sync on module load (writes) and a lock-free read otherwise | Same as A | None | None |
| **WebAssembly and single-thread** | Trivial: no sync | Trivial | Trivial | Trivial | Trivial |
| **Syntax objects and scope sets** | No effect: they name identifiers by string, converting at `datum_raw` and lowering | No effect | No effect | No effect | No effect |
| **ABI** | None | None | Changes the tag table (ADR 0002, 0006): the symbol tag goes, `symbol?` is a mask and compare either way. Frees a tag for something else. Uninterned and generated symbols (the types `TypeCode::Symbol` already has kinds for) need a heap representation anyway, so there are two kinds of symbol | None | None |
| **Main problem** | The DoS. Also sync cost on every `string->symbol` | The load-order hard case | Two kinds of symbol, an ABI change, ids that never die | Gives up sharing | Gives up sharing of the commonest constant (quoted symbol lists) |

### The hard case in B

A module is loaded into an isolate that already has a *local* symbol `foo` (the isolate called `(string->symbol "foo")` or read `foo`), and the module's pool, built once and shared, holds a global `foo`. The isolate now has two words for one name, and a value of the local one may be anywhere in its heap, in eq-tables and in handles. It cannot be rewritten. Options:

1. **Detect at load, and fall back for that module in that isolate.** Loading checks each of the pool's symbols against the isolate's local table. On a conflict the isolate does not use the shared pool for that module; it builds a per-isolate pool as in D, resolving each symbol to the local one. Correct, and the fallback is code D needs anyway. It costs memory for that module in that isolate only, and conflicts need a local symbol to exist before a module that mentions it loads: a host that loads its libraries and programs first never sees one.
2. **Make the local symbol a forwarding record.** Impossible without a read barrier on `eq?`.
3. **Intern every pool symbol into every isolate at creation.** Needs every module's symbols in advance.

So B needs option 1, which means **B contains D**, and a loader that decides per module and isolate.

### Recommendation on symbols

**Build D first, and keep B as the way to share** (decided: see Decisions). Concretely:

1. Pools are per `Loaded`, which is per isolate as built. Symbols and keywords in constants are the isolate's own, interned as today and immortal in the isolate. A pool object may therefore point to a heap symbol or keyword: the one exception to "pool points only to pool". Those are never freed or moved while the isolate lives (nothing frees interned symbols today; a moving tracer would pin them). Everything in sections 1 to 4 and 6 to 7 works with no global state, no sync and nothing for embedders to cap.
2. Sharing `Loaded` between isolates is a separate step. When it lands, B with the conflict fallback is the design ADR 0008 already chose; the pool builder produces a shared pool when no symbol conflicts and a private one otherwise. E is the same machinery with a finer cut and is the better answer if most of a pool's bytes are symbol-free data.
3. A is rejected for runtime interning because of the denial of service, and C because it changes the ABI to solve a problem B solves without it, and cannot free ids.

**What to measure to decide step 2:** (a) the share of pool bytes, over the standard libraries and over the benchmarks, that is symbol-free (what E could share) against everything (what B could share); (b) the number of isolates a host runs, which turns that into bytes; (c) the cost of the extra global probe in `string->symbol` against the current one, with and without the read lock; (d) how often a conflict would occur in a host that loads libraries first (it should be never) and one that does not.

## Measurements

All on a local prototype of the check (branch `immutable-literal-pairs-prototype`, not for merge), release builds, Apple M-series Mac running Defender. Wall times are noisy: each figure is the minimum of many runs, interleaved across variants, and differences under about 4% are noise. Hardware counters were unavailable without a privileged tool, so instruction counts are from the disassembly.

### The write check, static

| Strategy | Page word | Header bit | Side-table flags byte |
|---|---|---|---|
| Instructions | 4 (`and`, `ldr`, `cmp`, branch) | 2 (`ldrb`, `tbnz`) | 11, one a `udiv` |
| Extra loads | 1, the page header | 1, on the object's own line | 5, one in the side table |

The side table's flags byte is the obvious home under side counts and the wrong one: `side_byte` divides by a block size the compiler does not know. The page word is the same answer in a third of the work.

### The check in a loop that does nothing else (page word, minimum of runs, ms)

| Benchmark | Strategy | Baseline | With check |
|---|---|---|---|
| `mutate-list`: `set-car!` down a 10,000-element list, 300 times (contiguous pairs) | none | 14.1 | 13.5 (-4.3%) |
| | deferred | 14.2 | 14.1 (-1.0%) |
| | deferred, side counts | 15.6 | 16.6 (+6.5%) |
| | precise | 16.8 | 16.7 (-0.6%) |
| | precise, side counts | 21.3 | 21.3 (+0.4%) |
| `mutate-scattered`: 2,000,000 `set-car!` on 1,000,000 pairs in pseudo-random order, about 500 pages | none | 76.1 | 90.1 (+18.4%) |
| | deferred | 107.2 | 116.2 (+8.4%) |
| | deferred, side counts | 99.2 | 114.2 (+15.2%) |
| | precise | 133.1 | 141.0 (+5.9%) |
| | precise, side counts | 176.7 | 176.0 (-0.4%) |

Read with a noise floor of about 5% (a variant that changes no code on the `none` path measured +5.3%). **The locality argument holds where the page header is hot: with pairs mostly contiguous, as a bump-allocated list is, the check is lost in the noise. It does not hold in the worst case built for the measurement, where every mutation hits a pair on a different page of 500: the check then costs 0 to 18% of a loop that is nothing but cache-missing mutations**, about 3 to 7 ns a mutation in the cases that moved. That is not a representative program, but it is the bound. Static counts are above; hardware counters were not available.

### Ordinary benchmarks (never mutate a pair)

`deriv`, `queens` and `closures` under all five strategies: every variant within -1.9% and +3.1% of the baseline, noise.

### Cost of the sticky test in headered counting

Needed from stage 1 (section 1). Increment becomes "add unless at the maximum" and decrement "return stuck at the maximum", as side counts have. Same benchmarks, headered strategies: `deriv` +1.5% (deferred) and +2.2% (precise), `closures` +0.3% and +2.1%, `queens` +0.5% and +0.1%; `mutate-scattered` and `mutate-list` within noise. So a compare and a branch on a count update costs at most about 2%, on code that counts more than any real program. This is the cost the decision accepts.

### How much a pool holds

A bare isolate that imports `(scheme base)`, `(scheme write)`, `(scheme cxr)`, `(scheme char)`, `(srfi 1)`, `(srfi 132)`, `(scheme lazy)` and `(scheme eval)` loads 2 pair constants, 5 strings, 17 symbols and 1 flonum, about 1.5 KiB as objects in the heap (an estimate from the constant kinds), out of a 288 KiB isolate. About two thirds of those bytes are in constants that reach a symbol. **So giving up sharing (D) costs about 1.5 KiB an isolate for the standard libraries, and nearly all of it is symbol-bearing, which E could not share either.** Sharing pays only for programs that carry large literal tables. The `eval` and frozen-heap-page route costs 32 KiB per isolate with such constants (measured on the first prototype: 96 to 128 KiB bare under no collection, 288 to 320 KiB under counting, for two pair literals).

### Existing tests

On the first prototype, with the page-word check on for pairs under every strategy, `cargo test --workspace --no-fail-fast` ran 400 tests: the regression `mutated-literals-same-at-every-opt-level` failed (it mutates literals on purpose) and so did two no-collection page-density tests (the page word takes 8 bytes: 4096 pairs a page become 4094). Nothing in the standard libraries and no other test mutates a literal pair.


## Consequences

- Writes to every constant, of every kind, are errors under every strategy and optimisation level, and a constant is one object in memory, outside the heap. Programs that mutate quoted data, which R7RS already calls erroneous, break. Nothing in the standard libraries or the test suite does (measured on the prototype for pairs; the vector and string tests change with #66).
- Counting no longer pays for constants: no counts to set up, none to maintain, none to free. A pool object costs nothing at epoch time.
- Every write pays a 2 to 4 instruction check. A native tier pays the same and cannot drop it as a memory-safety check; it can drop it where the object is known fresh.
- Headered counting gains a sticky test on every count update (at most about 2%). Side counts already have it.
- Trial deletion skips objects with stuck counts, which also saves time on a literal-heavy heap.
- The optimiser may fold `car`, `cdr` and `vector-ref` of constants again (removed in #17) once every tier checks, including `VALI_NO_OPT`. A follow-up.
- `fn constant`'s heap allocation is replaced by a pool builder. `Instance::constants` holds pool words. `make_immutable` on heap objects goes away.
- `eval` and REPL constants take the frozen-heap-page route so that #31 can free them.
- No format changes: the distribution format and artifacts are untouched, and `VALI_ROUND_TRIP=1` needs nothing.

### Amendments this record needs when accepted

- **ADR 0002:** #8 (pairs and flonums: the pool and frozen pages; constants have the heap layout), #7 (the strategy's half of a pool header is frozen; the meaning of `IMMUTABLE`), #10 and #11 (a pool string never installs hints), and the identity-hash rule.
- **ADR 0003:** the strategy contract gains a row: pool objects are outside the strategy (sticky counts, in headered counting too; **an object with a stuck count is a leaf in trial deletion's `mark_gray`, `scan_black` and `collect_white`, with the reason it is sound**; not roots; not moved; identity hash from the address). It also states the page-header invariant (every space that can hold a pair has a header at the start of each 32 KiB-aligned region, with no object starting there) and says that under MMTk the check becomes a VM-space address-range test.
- **ADR 0005 #4:** constants are built into pool pages as the artifact loads.
- **ADR 0006:** the ABI version covers the page size, the page header and its `frozen` field, and the sticky count.
- **ADR 0008 #6:** which constants are shared, and that the table of compiled-in symbols is the answer chosen above (or its replacement). Mark it *planned*: nothing is shared yet.
- **ADR 0009:** pool pages and the limit.
- **ADR 0012:** none, unless the pool is later cached as a run-format image keyed by the ABI version (an option, not a part of this decision).

## Decisions

Decided while the record is Proposed. They replace the questions this record opened with.

1. **M2.** The page word is for pairs only. Objects that have a header (vectors, strings, bytevectors) are frozen by the `data::IMMUTABLE` header flag, which the pool builder sets. #66 (literal vectors) is merged and does exactly this, so it is consistent with this record and nothing is undone. `IMMUTABLE` stays, with one meaning: "this object is frozen". Error-message strings the VM makes stay immutable, and so do `symbol->string` and `keyword->string` results. Section 3 records the large-object reason; M1 is not taken, and a reserved address range stays an open alternative (section 3).
2. **Per-isolate pools first (option D).** Pools belong to `Loaded`, which is per isolate as built, and symbols and keywords in constants are the isolate's own. Sharing pools between isolates comes later, with option B (ADR 0008 #6) and its load-time fallback, once `Loaded` itself is shared. Rejected: **A**, because `string->symbol` on untrusted input would grow a process-global table that no embedder can attribute or cap, and every intern would synchronise; **C**, because it changes the ABI to solve what B solves without it, and an immediate id can never be freed, since nothing traces it.
3. **`eval` and REPL constants** are decided together with #31. Until then they live until the isolate drops. Frozen heap pages (section 2) are the likely route, so that #31 can free them.
4. **Pools are charged to the isolate's memory limit** (ADR 0009), which is natural while each isolate has its own.
5. **Host errors.** A host write to a constant returns a Scheme error with a message that names the object, as #66 did for `Handle::vector_set` (`Error::scheme(format!("the vector {} is immutable", ..))`). `Handle::set_car` and `Handle::set_cdr` do the same for a pair, and no new `Error` variant is added.
6. **The `car` and `cdr` folds removed in #17 are not reinstated now.** They may be, later, once every tier checks writes, including `VALI_NO_OPT`.
7. **The sticky test is taken in stage 1** for headered counting, and trial deletion treats an object with a stuck count as a leaf (section 1).
8. **No API for hosts to build constants** for now.

## Left open

- How a moving tracer pins interned symbols, and whether pool pages are an MMTk VM space or a space of the binding's own.
- Caching a pool as a run-format image on disk, mapped read-only into several processes.
- Merging equal immutable constants across modules.
- Whether to replace the page check for pool pages with a reserved address range (section 3).

## References

- R7RS-small, 3.4 (storage model) and 4.1.2 (quotation).
- Issues #17, #31, #59, #60, #61 and PR #66. Commit 8375826.
- ADR 0002 #7, #8; ADR 0003; ADR 0005 #4, #7; ADR 0008 #5, #6; ADR 0015 #13.
- Immix (Blackburn and McKinley, PLDI 2008) and MMTk's immortal and VM spaces.
