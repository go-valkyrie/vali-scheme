# 0011. Licence and contribution policy

- **Status:** Accepted
- **Date:** 2026-11-16
- **Related:** 0007 (choice of bignum library)

## Context

Vali borrows almost all of its ideas and will port some code. The projects it learns from have different licences: Racket is MIT or Apache-2.0 at the user's choice, Chez Scheme is Apache-2.0, scheme-rs is MPL-2.0 and Guile is LGPL. Some of their test suites are candidates for Vali's. Vali is meant to be embedded in other people's tools, so its own licence should not constrain them.

## Decision

1. **Vali is licensed under the MIT licence,** in `LICENSE` at the repository root.
2. **Contributions are accepted under the same licence** (inbound = outbound). There is no contributor licence agreement, and contributors keep their copyright.
3. **Ideas can come from anywhere; code is ported only from MIT-compatible sources.** Code adapted from MIT or MIT-or-Apache-2.0 projects, such as Racket, keeps the original copyright notice in its file header and is listed in `THIRD-PARTY.md`. Code under any other licence is read for ideas but not copied into Vali's source. That covers Apache-2.0-only code (Chez Scheme), MPL-2.0 code (scheme-rs) and LGPL or GPL code (Guile).
4. **Third-party test suites** can be vendored under `tests/third-party/<name>/` with their own licence files. They stay unmodified, with any harness adapter in a separate file, and are never part of a published crate.
5. **Dependencies of published crates** must have permissive licences compatible with MIT: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD, ISC, Zlib or Unicode. Copyleft dependencies are not allowed. CI enforces this, for example with `cargo-deny`.

## Consequences

- Hosts can embed Vali in proprietary or permissively licensed tools with no obligation beyond keeping the notice.
- Code from Chez Scheme cannot be ported line by line, even where that would be convenient.
- The bignum and Unicode libraries must meet rule 5 (ADR 0007).
- scheme-rs's `tests/r7rs.scm` and `tests/r6rs.scm` can be used as vendored tests under rule 4.

## Left open

- Whether to require a Developer Certificate of Origin sign-off once there are outside contributors.
