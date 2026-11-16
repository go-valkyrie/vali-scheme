# 0002. Value representation

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, complex numbers are headered objects, as the full numeric tower requires (ADR 0007)
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
| `001` | Pair | `car`, `cdr`, `pair?` and walking lists. A pair has no header: two words, `car` and `cdr` |
| `010` | Closure | Every call's "is this callable?" check, and `procedure?` in contracts. The code pointer sits at a fixed offset, so a call is a tag test and one load |
| `011` | Symbol | `symbol?`, and the case analysis that macros do |
| `100` | String | `string?`, probably the most common contract on data |
| `101` | Flonum | Boxed IEEE 754 binary64. Generic arithmetic's test after the fixnum test |
| `110` | Other heap object | Type in the header: vectors, bytevectors, records, bignums, ratnums, complex numbers, ports, Rust objects and so on |
| `111` | Immediate | Subtag in the next bits: booleans, the empty list, characters (Unicode scalar values), end of file, unspecified, the default object, internal markers |

1. **Heap objects are aligned to 8 bytes,** which frees the 3 low bits of every pointer. Field offsets absorb the tag, so dereferencing costs nothing extra.
2. **Vectors and records stay headered.** A vector access reads the length from the header for its bounds check anyway. A record predicate compares the record's type descriptor, which is in the header either way.
3. **`procedure?` has a fast and a slow path.** Records can be callable, so `procedure?` means "tagged closure, or a record whose type is applicable". Closures are a register test; callable records go through the header.
4. **`eq?` is a word comparison.** Equal fixnums, characters, booleans and empty lists are `eq?`. Equal flonums are `eqv?` but not necessarily `eq?`, as R7RS allows. Symbols are interned, so equal symbols are `eq?` (ADR 0008 says where they live).
5. **In Rust, a value is `#[repr(transparent)] struct Value(u64)`.** ADR 0003 governs holding one across anything that can reclaim memory.
6. **The tag table is part of the ABI version** (ADR 0006). Changing it rebuilds native code and run-format bytecode, but no distribution artifact.

## Consequences

- Type tests for the common types are a mask and a compare on a register, with no memory access. That includes contract checks on strings, symbols and procedures.
- Fixnum fast paths are one or two instructions; generic arithmetic is a runtime call taken on overflow or on a non-fixnum.
- Fixnums turn into bignums beyond ±2⁶⁰ (about 1.15 × 10¹⁸).
- Every flonum result allocates in tier 0. The JIT can keep flonums unboxed within a function.
- A pair takes two words. The collector must find pairs without a header, by the pages they live on (ADR 0003).
- Only the immediate subtags have room to grow. A future type that needs fast tests gets a header fast path, or a new ABI version that reassigns a tag.

## Alternatives considered

- **NaN boxing:** best for float-heavy code, but Scheme's exact integers are the case it serves worst, and its 48-bit pointer assumption clashes with current hardware (see the options above).
- **63-bit fixnums:** two more bits of fixnum range, but only three tags for heap objects. Strings and closures would need a header load for every type test, which contracts make frequent.
- **A tag for vectors instead of flonums:** vector accesses read the header anyway, while generic arithmetic gains from a flonum tag on every non-fixnum operation.

## Left open

- **32-bit targets** (wasm32, armv7, i686). The choice is between word-sized values, with 29-bit fixnums and 2-bit or 3-bit tags, and 64-bit values on every target. Distribution artifacts are unaffected either way (ADR 0012). Must be settled before any 32-bit port.
- The immediate subtag layout.

## References

- David Gudeman, *Representing Type Information in Dynamically Typed Languages*, technical report TR 93-27, University of Arizona, 1993.
- The Chez Scheme source, `s/cmacros.ss`: its type tags on 64-bit targets.
- R7RS-small, sections 6.1 (equivalence predicates) and 6.2 (numbers).
