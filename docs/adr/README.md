# Architecture decision records

Each record states one decision about Vali's design: the context that forced it, what was decided, and what follows from it.

## Conventions

- Files are named `NNNN-short-title.md` and numbered in the order they are written. Numbers are never reused.
- A record's status is **Proposed**, **Accepted**, **Rejected** or **Superseded by NNNN**.
- A Proposed record can be edited freely while it is argued. It lays out the options and a recommendation, and lists the questions to settle before it is accepted.
- While the design is being fleshed out, an Accepted record is amended in place, with an **Amended** line giving the date and what changed. A new record is written only for a new topic.
- Once the design phase ends, Accepted records are frozen. A change of mind is then a new record that supersedes the old one, and the only edit to the old one is its status line.
- A decision accepted but not yet built says so, in the status line or as **planned** at the item. Where built code differs from a record, an **As built** note says how.
- An Accepted record may leave details open. They are listed under **Left open**, and settling one takes a new record.
- New records start from [template.md](template.md).

## Index

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-phasing-and-artifact-sections.md) | Phasing and artifact sections | Accepted |
| [0002](0002-value-representation.md) | Value representation | Accepted |
| [0003](0003-memory-management.md) | Memory management | Proposed |
| [0004](0004-frames-and-continuations.md) | Frames and continuations | Accepted |
| [0005](0005-bytecode-format.md) | Bytecode format | Accepted |
| [0006](0006-relocatable-code-and-abi-versioning.md) | Relocatable code and ABI versioning | Accepted |
| [0007](0007-language-scope.md) | Language scope | Accepted |
| [0008](0008-threading-model.md) | Threading model | Accepted |
| [0009](0009-loaders-and-capabilities.md) | Loaders and capabilities | Accepted |
| [0010](0010-declarative-contracts.md) | Declarative contracts | Accepted |
| [0011](0011-licence-and-contribution-policy.md) | Licence and contribution policy | Accepted |
| [0012](0012-distributing-compiled-libraries.md) | Distributing compiled libraries | Accepted |
| [0013](0013-standalone-executables.md) | Standalone executables | Accepted |
| [0014](0014-reader-and-source-representation.md) | Reader and source representation | Accepted |
| [0015](0015-scopes-and-syntax-objects.md) | Scopes and syntax objects | Accepted |
| [0016](0016-rust-bridges-and-objects.md) | Rust bridges and objects | Accepted |
| [0017](0017-intermediate-representation-and-optimisation.md) | Intermediate representation and where optimisation runs | Accepted, partly built |
| [0018](0018-library-abi-versions.md) | Library ABI versions | Accepted, not built |
| [0019](0019-webassembly-target.md) | WebAssembly target | Proposed |

Milestone M0 ended with 0002, 0004, 0005 and 0008 accepted. 0003 stays a proposal. M1's measurements exist, but all five strategy variants stay built and tested until the optimising compiler's ownership inference can be measured too.
