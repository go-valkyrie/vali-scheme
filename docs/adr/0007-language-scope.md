# 0007. Language scope

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, decided: the full numeric tower, SRFI 125 with SRFI 128, partial `(rnrs …)` aliases, SRFI 226; adds the Rust interface to numbers
- **Amended:** 2026-11-17, standard libraries as embedded sources; programs without imports see `(scheme base)` (item 11)
- **Amended:** 2026-11-18, the internal library of primitives is Vali's own: no program, host library or `eval` environment can import it, and a program with no imports sees only `(scheme base)` (item 11)
- **Related:** 0001 (libraries and phasing), 0002 (tags), 0004 (continuations), 0008 (tasks), 0009 (capabilities), 0011 (licences of dependencies)

## Context

The design overview sets the baseline: R7RS-small, plus `syntax-case`, hygienic procedural macros exported from libraries, R6RS `library` forms and syntax parameters. The full R6RS standard library stays out of scope.

R7RS-small makes most of the numeric tower optional. It requires only "a coherent subset", gives "exact numbers are always integer" as an acceptable example, and encourages, without requiring, unlimited exact integers and rationals. When an operation cannot give an exact result, an implementation may raise an implementation-restriction violation or quietly return an inexact one. Feature identifiers (`ratios`, `exact-complex` and others) let portable code detect what is supported. In practice, every widely used Scheme provides at least exact rationals, and portable code and test suites assume them.

Evaluating Steel and scheme-rs turned up standard procedures that real programs needed and that one or both lacked: `list-sort`, `command-line`, string output ports, `keyword?` and `get-environment-variable`.

## Decision

### Numbers: the full tower

1. **Every kind of number R7RS describes:** exact integers of unlimited size, exact rationals, inexact reals (IEEE 754 binary64), and complex numbers, exact and inexact.
   - `(/ 1 3)` is `1/3`; `(exact 0.5)` is `1/2`; `(sqrt -4)` is `+2i`.
   - Vali reports the feature identifiers `ratios`, `exact-closed`, `exact-complex` and `ieee-float`.
   - `(scheme complex)` is fully implemented.
2. **Representation** (ADR 0002): fixnums and flonums have tags; bignums, ratnums and complex numbers are headered objects on the slow path of generic arithmetic. Ratnums are always in lowest terms, never integers. Complex numbers with a zero imaginary part are never stored as complex if they are exact, as R7RS requires.
3. **One implementation of the tower,** in Rust, serves the runtime's generic-arithmetic slow paths, the JIT's runtime calls and the embedding API, so Scheme and Rust can never disagree about what `(+ 1/2 0.5)` is. Fixnum and flonum fast paths stay inline in generated code and never reach it.
4. **The bignum library must have an MIT-compatible licence** (ADR 0011). That excludes GMP bindings and `malachite`; `dashu` and `num` are candidates.

### The Rust interface to numbers

5. **Bridges declare ordinary Rust types,** and conversion follows exactness:

   | Bridge parameter type | Accepts | Rejects, with an error naming the argument |
   |---|---|---|
   | `i64`, `u32` and other integer types | Exact integers within the type's range | Rationals, inexact numbers (even `2.0`), complex numbers, values out of range |
   | `f64` | Any real number; exact values are converted with correct rounding, as `inexact` does | Complex numbers with a non-zero imaginary part |
   | `vali::Integer` | Any exact integer | Everything else |
   | `vali::Rational` | Any exact real | Inexact numbers, complex numbers |
   | `vali::Number` | Every number | Non-numbers |

   Return values convert the other way, normalised: a `Rational` equal to `2` reaches Scheme as the integer `2`.
6. **`vali::Number` is an owned enum,** `#[non_exhaustive]`, with variants for exact integers (small values stored inline, so no allocation), rationals, reals and complex numbers. Vali's own `Integer`, `Rational` and `Complex` types wrap the bignum library, so the public API does not depend on that library's versions. Optional, additive Cargo features add conversions to and from `num` and `dashu` types.
7. **`Number` implements Rust's operators with Scheme's semantics:** `Add`, `Sub`, `Mul`, `Div` and `Neg`, owned and borrowed, and mixed with primitives.
   - Overflow promotes to bignums, results are normalised, and inexactness is contagious.
   - Division by exact zero panics, as Rust's integer division does; `checked_div` returns a `Result`. A panic that reaches a bridge boundary becomes the same Scheme condition `(/ 1 0)` raises, when panics unwind.
   - `PartialEq` follows Scheme's `=` (`1 == 1.0`), and `PartialOrd` orders reals and returns `None` for non-real complex numbers. `Number` implements neither `Eq`, `Ord` nor `Hash`; an `eqv()` method gives Scheme's `eqv?`.
   - Methods cover what operators can't: `exact`, `inexact`, `floor`, `round` (ties to even), `quotient`, `remainder`, `modulo`, `sqrt`, `expt`, `magnitude`, `angle`.
   - `Display` and `FromStr` use Scheme's numeric syntax (`1/3`, `+inf.0`, `1+2i`).
   - An optional Cargo feature implements `num-traits`.
8. **Converting to data formats that lack a number kind is a host policy.** Turning Scheme data into JSON, YAML or TOML meets rationals and complex numbers those formats can't express. The policy is chosen by the host:
   - **error** (the default), naming the path to the value;
   - **inexact**, the nearest float, for rationals;
   - **string**, Scheme syntax such as `"1/3"`.

### Libraries and syntax in scope

9. **All of R7RS-small:** every `(scheme …)` library, proper tail calls, full `call/cc`, `dynamic-wind`, `define-record-type`, parameters, exceptions (`raise`, `guard`, `error`), string ports and bytevectors. Libraries that reach outside the program, such as `(scheme file)`, `(scheme process-context)`, `(scheme time)` and `(scheme load)`, work through capabilities (ADR 0009).
10. **Macros:** `syntax-rules`, `syntax-case`, `datum->syntax`, `syntax->datum`, `free-identifier=?`, `bound-identifier=?`, `identifier?`, `generate-temporaries`, `with-syntax`, `quasisyntax`, `syntax-violation`, and syntax parameters (SRFI 139). Scheme code can read a syntax object's source location.
11. **Libraries:** `define-library` and R6RS `library`, which expand to the same module form.
    - **The standard libraries are `define-library` source files embedded in the binary,** which the built-in loader serves (ADR 0009 #3). Each imports one internal library of Rust primitives, `(vali primitive)`, re-exports what it needs, and defines in Scheme whatever is easier there. Only Vali's own libraries may import it, those served from the sources Vali carries: a program, a library a host's loader serves, and an `eval` environment get a compile error, so internal procedures such as `%raise` or `%call/cc` cannot be used to step around what a program's imports grant (ADR 0009). They go through the same machinery as a user's libraries, and until cached artifacts exist they are expanded when first imported.
    - **A program with no `import` form sees `(scheme base)`,** so scripts, tests and the REPL stay short. A program that has an `import` sees only what it imports, as R7RS requires. Either way an identifier bound by nothing it sees is a compile error; only the REPL binds unbound names, as its own variables.
12. **Reader extensions:** keywords (SRFI 88, with `keyword?`), `[ ]` as parentheses, block and datum comments, datum labels.
13. **Hash tables: SRFI 125, with SRFI 128 comparators.** SRFI 125 keeps SRFI 69's names as deprecated procedures, so most SRFI 69 code runs unchanged.
14. **Control: SRFI 226** (*Control Features*): prompts and delimited continuations, continuation marks, and parameters defined on them (ADR 0004).
15. **Other extensions from the start:** `list-sort` and the rest of SRFI 132 (SRFI 132 and R6RS take the same argument order), the SRFI 1 list library, callable records and `class-of`, and a condition hierarchy richer than R7RS's wherever error messages need it.
16. **R6RS library names as aliases, where Vali has an equivalent:** `(rnrs base)`, `(rnrs control)`, `(rnrs lists)`, `(rnrs sorting)`, `(rnrs syntax-case)`, `(rnrs exceptions)`, and `(rnrs hashtables)` as a thin layer over SRFI 125. `(import (rnrs))` exports the union of the aliases provided. R6RS's I/O, records, enumerations and bitwise-arithmetic libraries are not provided.

### Out of scope until a later ADR

- The rest of the R6RS standard library.
- The Scheme API for parallelism: isolates, channels and shared memory (ADR 0008). SRFI 18 threads are green threads within an isolate.

## Consequences

- Vali computes what Chez, Racket, Guile, Chibi, Gauche and CHICKEN compute, and compliance suites run without disabling numeric tests.
- Exact arithmetic can grow silently: code written as if `/` produced floats builds rationals whose numerators and denominators keep growing. Documentation should point this out; writing `2.0` or calling `inexact` avoids it.
- Code expecting `sqrt` of a negative number to fail or give NaN gets a complex number instead.
- Generic arithmetic dispatches over fixnums, bignums, ratnums, flonums and complex numbers. The tagged fast paths are unaffected.
- Small rationals could later be stored as immediates, avoiding allocation, if measurements show rational-heavy code matters. That would change the ABI version (ADR 0006), not any file format.
- Rust code gets Scheme's tower through ordinary operators, and data-format conversion fails loudly rather than silently losing exactness.

## Left open

- Which further extensions real programs need beyond the lists above.
- How fully the `(rnrs …)` aliases follow R6RS where R6RS and R7RS disagree on details.

## References

- R7RS-small, section 6.2 (numbers) and appendix B (standard feature identifiers).
- SRFIs 1, 88, 125, 128, 132, 139 and 226.
- R6RS, the standard libraries.
