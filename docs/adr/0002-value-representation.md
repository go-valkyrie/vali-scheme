# 0002. Value representation

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, complex numbers are headered objects, as the full numeric tower requires (ADR 0007)
- **Amended:** 2026-11-17, object layouts: one header word split between the memory strategy and the runtime; pairs and flonums carry a header only if the strategy wants one; the immediate subtags; UTF-8 strings; the built-in type codes (items 7 to 13)
- **Related:** 0003 (memory management), 0006 (ABI version), 0008 (threads), 0010 (contracts), 0012 (distribution format)

## Context

On 64-bit targets, every Scheme value fits in one 64-bit word. The encoding of that word sets the cost of the most common operations in every tier: fixnum arithmetic, type tests, `car` and `cdr`, `eq?` and procedure calls. The encoding is part of the ABI (ADR 0006), so changing it means rewriting the VM's and the JIT's fast paths and rebuilding native code. It is not part of any file format: distribution artifacts store constants in a form independent of the architecture (ADR 0012), and run-format bytecode belongs to one machine.

Typical scripts and the benchmarks in the design overview (fib, list loops, queens, browse) mostly use small exact integers, pairs and lists, strings, symbols, records and closures. Floating-point arithmetic is less common, but it should not fall off a cliff.

Type tests are frequent. Every call checks that its callee is callable. `car`, `cdr` and list walking check for pairs. And contracts (ADR 0010) put predicates such as `string?` and `procedure?` at every library boundary. A contract check often doesn't go on to use the object, so a test that has to load the object's header can cost a cache miss that nothing else would have paid for.

## Options considered

- **NaN boxing** (LuaJIT, SpiderMonkey, JavaScriptCore): doubles are unboxed, and other values live in a quiet NaN's payload. Floating-point never allocates. But fixnums shrink to 32–48 bits, pointers must be decoded before use, and every pointer must fit in 48 bits. That clashes with hardware features that use a pointer's top bits: AArch64's top-byte-ignore, memory tagging, pointer authentication, and 57-bit address spaces.
- **Low-bit tags with 63-bit fixnums:** fixnums take every word whose low bit is 0, leaving three tags for heap objects.
- **Low-bit tags with 61-bit fixnums** (Chez Scheme, Racket CS): fixnums take one 3-bit tag, leaving six tags for heap objects plus one for immediates.

## Decision

**On 64-bit targets, values are tagged in their low 3 bits. Fixnums take one tag and keep 61 bits; the most-tested heap types get tags of their own.**

| Low bits | Type | Why it has a tag |
|---|---|---|
| `000` | Fixnum, 61-bit two's complement | Arithmetic and comparison work on the word directly; the value is the word shifted right by 3 |
| `001` | Pair | `car`, `cdr`, `pair?` and walking lists. A pair has no runtime header: two words, `car` and `cdr`, plus a header only if the memory strategy wants one (item 8) |
| `010` | Closure | Every call's "is this callable?" check, and `procedure?` in contracts. The code pointer sits at a fixed offset, so a call is a tag test and one load |
| `011` | Symbol | `symbol?`, and the case analysis that macros do |
| `100` | String | `string?`, probably the most common contract on data |
| `101` | Flonum | Boxed IEEE 754 binary64. Generic arithmetic's test after the fixnum test |
| `110` | Other heap object | Type in the header: vectors, bytevectors, records, bignums, ratnums, complex numbers, ports, Rust objects and so on |
| `111` | Immediate | Subtag in the next bits: booleans, the empty list, characters (Unicode scalar values), end of file, unspecified, the default object, internal markers |

1. **Heap objects are aligned to 8 bytes,** which frees the 3 low bits of every pointer. Field offsets absorb the tag, so dereferencing costs nothing extra.
2. **Vectors and records stay headered.** A vector access reads its length for the bounds check anyway, from the word after the header. A record predicate compares the record's type descriptor, in the word after the header, in the same cache line (item 7).
3. **`procedure?` has a fast and a slow path.** Records can be callable, so `procedure?` means "tagged closure, or a record whose type is applicable". Closures are a register test; callable records go through the header.
4. **`eq?` is a word comparison.** Equal fixnums, characters, booleans and empty lists are `eq?`. Equal flonums are `eqv?` but not necessarily `eq?`, as R7RS allows. Symbols are interned, so equal symbols are `eq?` (ADR 0008 says where they live).
5. **In Rust, a value is `#[repr(transparent)] struct Value(u64)`.** ADR 0003 governs holding one across anything that can reclaim memory.
6. **The tag table is part of the ABI version** (ADR 0006). Changing it rebuilds native code and run-format bytecode, but no distribution artifact. So are the layouts below.

### Object layouts

7. **A header is one 64-bit word, split between the memory strategy and the runtime:**

   ```
    63                       32 31                  8 7          0
   [ strategy's half (32 bits) | runtime data (24)   | type code  ]
   ```

   - **The type code** names the object's type. Every type code is odd, so bit 0 of a header is 1. An 8-byte-aligned pointer has bit 0 clear, so a moving collector can replace a header with a forwarding pointer, and code walking the heap can tell the two apart.
   - **The strategy's half** is the strategy's own (ADR 0003): a count and the shared mark under counting, mark bits, age and the state of the identity hash under tracing, nothing under no collection. A count that reaches its maximum sticks there.
   - **The runtime data** depends on the type: a closure's free-variable count, a record's field count, or flags such as *immutable* and *all ASCII*. Variable-length objects keep their length in a full word after the header, not here.
8. **Every heap object has a header, except pairs and flonums, which have one only if the strategy wants it.** Their type is known from their tag, and they live on pages of their own, so a collector can find them without a header. A tracing strategy keeps pairs at two words. A counting strategy needs a count for every pair, either in a header (three words per pair, with the count beside `car` and `cdr`) or in a table of count bytes on each pair page (two words per pair, with counts that stick at 255 and are recovered by the cycle collector, as in LXR). Milestone M1 measures both and keeps the faster (ADR 0003).
9. **Immediates** use the 5 bits above the tag as a subtag, and the 56 bits above that as the payload:

   | Subtag | Value | Word |
   |---|---|---|
   | 0 | Character | `scalar << 8 \| 0x07`, the payload being the Unicode scalar value |
   | 1 | `#f` and `#t` | `0x0F` and `0x10F`, one bit apart, so `boolean?` is a mask and a compare |
   | 2 | The empty list | `0x17` |
   | 3 | End of file | `0x1F` |
   | 4 | Unspecified | `0x27` |
   | 5 | The default object (an omitted optional argument) | `0x2F` |
   | 6 | Internal markers: an unbound variable, a `letrec` binding not yet initialised, a hash-table tombstone | `n << 8 \| 0x37`. The VM ensures they never reach Scheme code |
   | 7 to 31 | Reserved | |

10. **The objects with their own tag** are laid out as follows, after the header:

    | Tag | Fields | Runtime data in the header |
    |---|---|---|
    | Pair | `car`, `cdr` | None |
    | Flonum | The `f64` | None |
    | Closure | The code pointer, at a fixed offset, then the free variables | The free-variable count |
    | Symbol | The name (an immutable string) and a precomputed hash | Interned, uninterned or generated |
    | String | Byte length, character count, a pointer to the bytes, a pointer to index hints | Immutable, all ASCII |

11. **Strings are UTF-8, always.** Rust code borrows a string's bytes as a `&str` with no conversion. The bytes sit behind a pointer because `string-set!` can change how many bytes a character takes; immutable strings, including every literal, may later store them inline. Indexing by character:
    - **An all-ASCII string** indexes bytes directly, so `string-ref` is O(1). The flag is kept up to date by every mutation.
    - **Any other string** keeps index hints: the last character index looked up and its byte offset, so walking a string in order is O(1) per step, and, for long strings, bookmarks every 64 characters, built on the first random access and dropped by mutation. Hints are installed with an atomic compare-and-swap, so a string shared between tasks stays correct.
    - **SRFI 130 cursors** are the efficient interface, and the string procedures of `(scheme base)` use them internally.
12. **The built-in types under the "other" tag** are those the runtime or the code generator must know about:

    | Group | Types |
    |---|---|
    | Containers | Vector, bytevector, box (an assigned variable that is captured, ADR 0005) |
    | Numbers | Bignum, ratnum, exact complex, inexact complex |
    | Records | Record, laid out as header, type descriptor, fields; record type descriptor |
    | Names | Keyword (SRFI 88) |
    | Code and control | Code object (bytecode or native, with its debug information), continuation and stack segment (ADR 0004) |
    | References | Weak pair, ephemeron |
    | Host | Port, Rust object |

13. **Everything else is a record:** hash tables, promises, parameters, conditions, environments and syntax objects (ADR 0015). They need no type code of their own, and get `record?`, printing and inspection from the record system.

## Consequences

- Type tests for the common types are a mask and a compare on a register, with no memory access. That includes contract checks on strings, symbols and procedures.
- Fixnum fast paths are one or two instructions; generic arithmetic is a runtime call taken on overflow or on a non-fixnum.
- Fixnums turn into bignums beyond ±2⁶⁰ (about 1.15 × 10¹⁸).
- Every flonum result allocates in tier 0. The JIT can keep flonums unboxed within a function.
- A pair takes two words under tracing, and two or three under counting, whichever M1 finds faster. A collector finds pairs without a header by the pages they live on (ADR 0003).
- Every header holds both a strategy's metadata and the object's type, so one load gives a counting strategy both, and no strategy needs a second header word.
- `string-ref` on a string with non-ASCII characters costs more than in a Scheme with fixed-width strings. Code that walks strings in order, or uses cursors, does not pay it.
- Only the immediate subtags have room to grow. A future type that needs fast tests gets a header fast path, or a new ABI version that reassigns a tag.

## Alternatives considered

- **NaN boxing:** best for float-heavy code, but Scheme's exact integers are the case it serves worst, and its 48-bit pointer assumption clashes with current hardware (see the options above).
- **63-bit fixnums:** two more bits of fixnum range, but only three tags for heap objects. Strings and closures would need a header load for every type test, which contracts make frequent.
- **A tag for vectors instead of flonums:** vector accesses read the header anyway, while generic arithmetic gains from a flonum tag on every non-fixnum operation.
- **Separate header words for the strategy and the runtime:** a record test would be one load instead of two in the same cache line, but every headered object would carry two header words under counting strategies.
- **Fixed-width strings (UTF-32, as in Chez and Racket), or narrow and wide strings (Guile; Python's PEP 393):** O(1) `string-ref` always, but every string handed to or from Rust is converted, and UTF-32 takes four bytes per character. Vali chooses UTF-8 everywhere, as Chibi Scheme and Gauche do.
- **A tag or a symbol flag for keywords:** a tag is too scarce, and a flag would turn `symbol?` from a tag test into a header load.

## Left open

- **32-bit targets** (wasm32, armv7, i686). The choice is between word-sized values, with 29-bit fixnums and 2-bit or 3-bit tags, and 64-bit values on every target. Distribution artifacts are unaffected either way (ADR 0012). Must be settled before any 32-bit port.
- Whether pairs carry a header under the counting strategies (item 8): decided by measurement in M1.

## References

- David Gudeman, *Representing Type Information in Dynamically Typed Languages*, technical report TR 93-27, University of Arizona, 1993.
- The Chez Scheme source, `s/cmacros.ss`: its type tags on 64-bit targets.
- The Lean 4 runtime, `lean.h`: its object header, a 32-bit count beside the type.
- Rifat Shahriyar, Stephen M. Blackburn, Xi Yang and Kathryn S. McKinley, *Taking Off the Gloves with Reference Counting Immix*, OOPSLA 2013, and LXR (PLDI 2022): small counts kept outside the object, stuck counts recovered by tracing.
- SRFI 130, *Cursor-based string library*; Chibi Scheme's and Gauche's UTF-8 strings.
- R7RS-small, sections 6.1 (equivalence predicates) and 6.2 (numbers).
