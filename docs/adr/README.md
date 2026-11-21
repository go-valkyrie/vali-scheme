# Architecture decision records

Each record states one decision about Vali's design: the context that forced it, what was decided, and what follows from it.

## Conventions

- Files are named `NNNN-short-title.md` and numbered in the order they are written. Numbers are never reused.
- Until the first release (0.0.1), records are living documents. Each says what the design is now, and a change of design is an edit in place: no amendment notes, no history sections and no separate record for a follow-up. The date in a record's header is the day it was first written. A new record is written only for a new topic. How records change after 0.0.1 will be decided then.
- A record's status is **Proposed**, **Accepted**, **Rejected** or **Superseded by NNNN**. Accepted can be followed by how much of it is built: **Accepted, partly built** or **Accepted, not built**, with the parts named in the record's status line.
- A Proposed record lays out the options and a recommendation, and lists the questions to settle before it is accepted.
- Where something is not built yet, the record says so plainly in a line where it matters, such as "Not built yet: ...", and does not keep a separate section for it.
- A record may leave details open. They are listed under **Left open**, and settling one is an edit to the record.
- New records start from [template.md](template.md).

## Index

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-phasing-and-artifact-sections.md) | Phasing and artifact sections | Accepted |
| [0002](0002-value-representation.md) | Value representation | Accepted |
| [0003](0003-memory-management.md) | Memory management | Proposed |
| [0004](0004-frames-and-continuations.md) | Frames and continuations | Accepted |
| [0005](0005-bytecode-format.md) | Bytecode format | Accepted |
| [0006](0006-relocatable-code-and-abi-versioning.md) | Relocatable code and ABI versioning | Accepted, partly built |
| [0007](0007-language-scope.md) | Language scope | Accepted |
| [0008](0008-threading-model.md) | Threading model | Accepted |
| [0009](0009-loaders-and-capabilities.md) | Loaders and capabilities | Accepted |
| [0010](0010-declarative-contracts.md) | Declarative contracts | Accepted |
| [0011](0011-licence-and-contribution-policy.md) | Licence and contribution policy | Accepted |
| [0012](0012-distributing-compiled-libraries.md) | Distributing compiled libraries | Accepted |
| [0013](0013-standalone-executables.md) | Standalone executables | Accepted, partly built |
| [0014](0014-reader-and-source-representation.md) | Reader and source representation | Accepted |
| [0015](0015-scopes-and-syntax-objects.md) | Scopes and syntax objects | Accepted |
| [0016](0016-rust-bridges-and-objects.md) | Rust bridges and objects | Accepted |
| [0017](0017-intermediate-representation-and-optimisation.md) | Intermediate representation and where optimisation runs | Accepted, partly built |
| [0018](0018-library-abi-versions.md) | Library ABI versions | Accepted, not built |
| [0019](0019-webassembly-target.md) | WebAssembly target | Proposed |
| [0020](0020-constants-outside-the-heap.md) | Constants live outside the isolate heap | Accepted |
| [0021](0021-portable-artifacts-and-platform-features.md) | Portable artifacts and platform features | Accepted, not built |
| [0022](0022-lint-framework.md) | Lint framework | Accepted, not built |
| [0023](0023-resource-limits.md) | Resource limits | Accepted |
| [0026](0026-async-integration.md) | Async integration | Accepted, not built |

0003 stays a proposal: all five strategy variants stay built and tested until the optimising compiler's ownership inference can be measured too.
