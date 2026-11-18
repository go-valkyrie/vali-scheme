# Architecture

How Vali's code is organised: its crates, its Rust and Scheme namespaces, the repository layout and the toolchain policy. The reasons behind the design are in the [ADRs](adr/README.md). This document changes as crates are added.

## Crates

Each boundary below exists for a reason recorded in an ADR:

- **A standalone executable must link the runtime without the expander and compiler** (ADR 0013).
- **The distribution format is stable and versioned on its own, while run-format bytecode is private** (ADRs 0005 and 0012).
- **The ABI has one source of truth that the code generator can read without instantiating a runtime** (ADR 0006).
- **The linker optimises across libraries without the expander** (ADR 0017), so the optimiser is a crate of its own.

| Crate | Holds | Depends on | Created in |
|---|---|---|---|
| `vali-num` | The numeric tower (ADR 0007): `Number`, `Integer`, `Rational`, `Complex`. Representation, parsing and printing come first; arithmetic and `std::ops` follow | the bignum library | M0 |
| `vali-syntax` | Spans and source identities, the lexer, the event parser and its sinks (ADR 0014), scope sets and syntax-object data structures | `vali-num` | M0 |
| `vali-test` | The test harness: `.scm` test files, the regression list, compliance suites, determinism checks and runs under each memory strategy. Not published | grows with the others | M0 |
| `vali-abi` | ABI facts: the tag table, object and frame layouts, the runtime function table, strategy descriptors, the ABI version (ADR 0006) | none | M1 |
| `vali-dist` | The distribution format (ADR 0012): the IR's types and encoding (ADR 0017), interfaces, version and feature flags | none yet; `vali-syntax` once syntax exports are encoded | M1 |
| `vali-opt` | The optimiser (ADR 0017): passes over the IR for each library, and the link-time optimiser across libraries. Today it contifies local functions, unboxes assigned variables, makes calls to known procedures direct, and simplifies each function | `vali-dist` | M1 |
| `vali-bytecode` | Run-format bytecode (ADR 0005): encoding, validator, disassembler, and conversion from the distribution format | `vali-abi`, `vali-dist` | M1 |
| `vali-artifact` | The artifact container, fingerprints and cache store (ADR 0001) | `vali-bytecode`, `vali-dist` | M1 |
| `vali-runtime` | Values, memory strategies, objects, frames and continuations, the trampoline, isolates and tasks, the VM, primitives, capability objects. The JIT is its `jit` feature | `vali-abi`, `vali-num`, `vali-syntax`, `vali-bytecode` | M1 |
| `vali-compiler` | The expander and the compiler's front half: core forms, conversion to the IR and optimisation of each library through `vali-opt` (ADR 0017), the contract pass, producing distribution code. One crate because the expander must compile and run transformers. Today it lowers the expander's tree to the IR, which `vali-bytecode` converts to run format; the optimiser's passes come later | `vali-runtime`, `vali-dist` and others | M1 |
| `vali-stdlib` | Vali's standard libraries compiled ahead of time: its build script compiles them with `vali-compiler` and embeds their artifacts, which `install` hands to the compiler, so starting up loads them instead of expanding them. A crate of its own because the compiler cannot depend on what its own build produces | `vali-compiler`, also as a build dependency | M1 |
| `vali` | The embedding API that hosts depend on: re-exports only. The compiler sits behind its default `eval` feature, the prebuilt standard libraries behind its default `prebuilt` feature | `vali-runtime`, `vali-macros`, `vali-num`, `vali-syntax`; `vali-compiler` and `vali-stdlib` by feature | M1 |
| `vali-cli` | The `vali` binary: REPL, scripts, `vali build`, `vali lsp` | `vali`, `vali-build` | M1 |
| `vali-bench` | The benchmark tool: runs `benchmarks/`, exact counts with the runtime's `counters` feature, medians of repeated timed runs, and comparisons between runs. Not published | `vali-compiler`, `vali-runtime`, `vali-stdlib` | M1 |
| `vali-macros` | `#[bridge]`. Procedural macros must be a crate of their own | none | M2 |
| `vali-codegen` | Cranelift code generation, taking CPU and memory strategy as targets, writing to memory (JIT) or object files (ADR 0013) | `vali-abi`, `vali-bytecode` | M3 |
| `vali-build` | `vali build`: whole-program compilation, runtime archives, linking (ADR 0013) | `vali-compiler`, `vali-codegen`, `vali-artifact` | M4 |
| `vali-ide` | Analysis for tools, built on Rowan trees and the expander's binding facts (ADR 0014) | `vali-syntax`, `vali-compiler` | When the language server starts |

## Rust namespace

Hosts depend on the `vali` crate, which re-exports what they need. Paths marked "not yet" are planned and not built:

| Path | Contents |
|---|---|
| `vali::{Isolate, Cx}` | `Isolate<S: Strategy = DefaultStrategy>`: a heap and its library instances, used by one thread at a time, entered through `call` and `with`; `Cx`, the context a host or bridge holds while inside it, with the isolate's current task (ADRs 0003 and 0008) |
| `vali::{Value, Handle}` | Raw values, for the runtime's own code, and handles: `Send`, `'static` roots used through their isolate's context (ADR 0003) |
| `vali::num`, also re-exported at the top level | `Number`, `Integer`, `Rational`, `Complex` (ADR 0007) |
| `vali::{bridge, object}` (macros), `vali::bridge` (module) | `#[bridge]` and `#[object]`, which work through `vali` alone; the module holds `NativeLibrary`, `FromScheme`, `IntoScheme`, `RustObject` and `Context`, the strategy-erased context bridges see (ADR 0016) |
| `vali::load` (`eval`) | `Loader`, `AsyncLoader`, `LibraryName`, `Identity`, `Source` (ADR 0009) |
| `vali::compile` and `vali::Repl` (`eval`) | `compile_program`, `load` and the REPL |
| `vali::stdlib` (`prebuilt`) | `install`, which makes the compiler load the standard libraries from their embedded artifacts |
| `vali::ports` | The console and the port types bridges take and return |
| `vali::cap` (not yet) | Capabilities: file, network, environment, clock, randomness |
| `vali::policy` (not yet) | Import policies and resource limits |
| `vali::diag` | `Span`, `SourceId`, `Sources` (ADR 0014); `Diagnostic` not yet |
| `vali::strategy` | The `Strategy` trait and the provided strategies |
| `vali::serde` (Cargo feature, not yet) | Data-format conversion, with the number policy (ADR 0007) |

## Scheme library namespace

| Prefix | Contents |
|---|---|
| `(scheme …)` | R7RS-small |
| `(srfi N)` | SRFIs. The SRFI 97 forms `(srfi :N)` and `(srfi :N name)` name the same library |
| `(rnrs …)` | R6RS names, as aliases where Vali has an equivalent (ADR 0007) |
| `(vali …)` | Vali's own public libraries, such as `(vali contracts)`, `(vali records)` (callable records, `class-of`) and `(vali syntax)` (static information, source locations) |
| `(vali internal …)` | The primitives the standard library is built from. User code cannot import them by default |

These prefixes are reserved: Vali's built-in loader owns them, and a host's loader cannot shadow them. Hosts name their own libraries under their own prefix.

## Repository layout

```
Cargo.toml                    workspace
rust-toolchain.toml           the pinned stable toolchain
crates/<crate>/               one directory per crate above
lib/                          Scheme sources of the standard libraries: scheme/, srfi/, rnrs/, vali/
docs/adr/                     architecture decision records
docs/architecture.md          this document
docs/bytecode.md              the run-format instruction set (ADR 0005)
docs/distribution-format.md   the distribution format specification (ADR 0012)
tests/regressions/            one test per row of the requirements list, failing until fixed
tests/third-party/<name>/     vendored test suites, each with its licence (ADR 0011)
tools/debugger/               GDB and LLDB extensions: Scheme backtraces, frames and values (ADR 0004 #6)
fuzz/                         cargo-fuzz targets, a workspace of its own that needs nightly; CI runs them on a schedule
bench/                        fib, a list loop, queens, browse
```

## Toolchain

- **Stable Rust only, pinned** in `rust-toolchain.toml`, so CI and contributors build with the same compiler. The pin moves forward deliberately. There is no older minimum supported version.
- **Edition 2024.**
- **Nightly features only as optional speed-ups,** behind a Cargo feature, never required. A host on stable must always be able to build Vali. The likeliest use is explicit tail calls for VM dispatch (ADR 0005).
