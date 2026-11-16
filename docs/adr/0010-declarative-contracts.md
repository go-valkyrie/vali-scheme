# 0010. Declarative contracts

- **Status:** Accepted
- **Date:** 2026-11-16
- **Related:** 0001 (interface sections), 0005 (removing checks), 0006 (AOT)

## Context

Four needs point at the same information:

- Configuration code wants a compile-time error when a value breaks its schema, such as a replica count of `0` against `(integer-in 1 …)`.
- The compiler wants to drop checks it can prove.
- AOT code wants argument types so it can dispatch statically.
- Tools want machine-readable schemas.

Schemas written with predicates such as `integer-in`, `one-of` and `string?` are contracts already. An arbitrary predicate can only be checked at run time, but contracts built from a fixed vocabulary can be reasoned about at compile time.

## Decision

1. **Declarative contracts** are built from a fixed vocabulary: type predicates, record predicates, `integer-in`, `one-of`, regular expressions, `or`, `and`, list-of and map-of, plus procedure signatures built from these. They are data, recorded in each library's interface section (ADR 0001).
2. **Opaque contracts** are any other predicate. They are always checked at run time.
3. **A violation names who broke the contract** (blame): the side of the boundary that supplied the bad value, at its source location.
4. **Contracts are checked where values cross library boundaries, and trusted inside a library.** An exported procedure with a contract gets two entry points. The checked one is used by default. Compiled callers may use the unchecked one where the contract is proven at the call site.
5. **A compiler pass sorts each check** into proven (removed), refuted (a compile-time error at the source location) or unknown (kept as a run-time check).
6. **AOT static dispatch** uses export signatures, bindings known to be constant, and sealed generic functions. AOT code keeps a generic slow path wherever a type isn't proven.
7. **Schemas for tools** are generated from the same data.

## Consequences

- Native code that trusts its argument types stays memory-safe when untyped code calls it, because the checked entry point stands between them.
- Contracts are part of a library's interface hash, so changing one rebuilds the library's importers (ADR 0001).
- A refuted contract can stop a program from compiling even if the bad path would never run. That suits code checked at build time, such as configuration; whether it should be only a warning elsewhere is open.
- Two entry points per contracted export cost some code size.

## Alternatives considered

- **Contracts as arbitrary predicates only** (Racket's default): maximally flexible, but nothing can be proven or turned into a schema.
- **A full static type system:** more than Vali needs now. A typed dialect can be built later on the replaceable module body.

## Left open

- The surface syntax of contracts and the exact set of combinators.
- How higher-order contracts on procedure arguments and results wrap values and keep blame.
- Whether refuted contracts can be downgraded to warnings.
- Type declarations as optimiser hints (CHICKEN-style), code specialised per argument type, and a typed dialect.

## References

- Robert Bruce Findler and Matthias Felleisen, *Contracts for Higher-Order Functions*, ICFP 2002.
- Andrew K. Wright and Robert Cartwright, *A Practical Soft Type System for Scheme*, TOPLAS 1997.
- Racket's contract system; sealing in Dylan.
