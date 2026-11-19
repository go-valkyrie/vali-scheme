# Agent notes

Vali is a Scheme (R7RS-small plus SRFIs and some R6RS names) built to be embedded in Rust hosts. It is a Cargo workspace of crates under `crates/`, pinned to stable Rust in `rust-toolchain.toml`, edition 2024.

## Commands

Tasks live in `mise.toml` and run through `bin/mise`, which downloads a pinned mise on first use (`bin/mise install` sets up the toolchain and cargo-deny).

- `bin/mise run check`: everything CI runs (`lint`, `test`, `docs`, `deny`).
- `bin/mise run test`: `cargo test --workspace`, plus four more passes. The native debugger tests run with `--features native-frames`, the library tests run with `VALI_ROUND_TRIP=1`, the library and bridge tests run with `VALI_NO_OPT=1`, and so do the regressions.
- `bin/mise run lint`: `cargo fmt --all --check` and Clippy with `-D warnings`.
- `bin/mise run docs`: rustdoc with `-D warnings`, so a broken intra-doc link fails the build.
- `bin/mise run bench`, `bin/mise run bench-compare OLD NEW`: the programs in `benchmarks/`.
- One crate: `cargo test -q -p vali-opt`. One test file: `cargo test -q -p vali-compiler --test it libraries::` (vali-compiler's and vali-runtime's integration tests are modules of one `it` binary). One regression: `cargo test -p vali-test --test regressions NAME`.

`cargo test` builds with opt-level 1 (`[profile.test]` in `Cargo.toml`). Other builds stay unoptimised.

## Testing

The full suite (`bin/mise run test`, or `check`) takes a minute or two and loads every core, so keep it out of the edit-test loop.

- While working, run only the tests your change affects: `jouet test --affected`, or `cargo test -q -p <crate>`.
- Run the full suite once, just before you push or open a PR, or when the user asks for it.
- After a rebase that applied cleanly, re-run the affected tests and leave the full run to CI (`.github/workflows/check.yml`).

Scheme tests are `.scm` files that assert with SRFI 64 and start with `;;; key: value` header lines (`kind:`, `expect: fail`, …). `crates/vali-test/src/lib.rs` documents the header lines and the test kinds. A `scheme` test runs under every memory strategy. A test marked `expect: fail` fails the build once it starts passing, so take the marker off when you fix one. `tests/regressions/` holds one file per row of the requirements list.

## How code flows

Source goes through these stages:

1. `vali-syntax` reads it: lexer, event parser, spans, scope sets and syntax objects.
2. The expander in `vali-compiler` lowers it to the IR, whose types live in `vali-dist`.
3. `vali-opt` optimises the IR for each library.
4. `vali-bytecode` converts it to run-format bytecode and validates it.
5. The VM in `vali-runtime` runs it.

Two formats, two policies:
- **The distribution format (`vali-dist`) is stable and versioned.** It is what compiled libraries ship as.
- **Run-format bytecode (`vali-bytecode`) is private and never shipped.** It is produced at load time, so conversion can do the validation and safety checks the runtime would otherwise repeat.

Until 0.0.1 is released, change either format, and the IR, whenever a design calls for it, and don't bump `FORMAT_VERSION` in `vali-dist` or `vali-bytecode`. Leave format version bumps out of ADRs too. Stale artifacts are caught by their build stamp and source fingerprint, not the version.

The standard libraries' Scheme sources are in `crates/vali-compiler/lib/` (`scheme/`, `srfi/`, `vali/`). `vali-stdlib`'s build script compiles them ahead of time and embeds the artifacts, so editing a library rebuilds `vali-stdlib` and everything that depends on it. `VALI_NO_PREBUILT=1` makes the `vali` CLI compile the libraries from source instead. `VALI_NO_OPT=1` turns off the optimiser, and `VALI_ROUND_TRIP=1` round-trips artifacts through their encoding.

## Docs and decisions

- `docs/architecture.md`: the crate map and the reason each boundary exists, the Rust and Scheme namespaces, and the toolchain policy. Some crates in its table (`vali`, `vali-artifact`, `vali-codegen`, `vali-build`, `vali-ide`) are planned and don't exist yet.
- `docs/adr/`: one record per design decision, indexed in `docs/adr/README.md`, which also states how records are amended or superseded. Read the relevant ADR before changing a design. New records start from `docs/adr/template.md`.
- `docs/bytecode.md`: the run-format instruction set.

## Commits and review

Changes are reviewed on GerritHub (`ssh://gerrithub.io:29418/go-valkyrie/vali-scheme`), which replicates to GitHub. One change is one commit: push it with `git push gerrit HEAD:refs/for/main`, and answer review by amending the commit and pushing again, not by adding commits.

Commit messages are Conventional Commits, and `hk.pkl` checks them:

- **Subject:** `type(scope): description`, where `type` is one of `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `security`, `style` or `test`. `scope` is optional and must be one of the scopes listed in `hk.pkl`: the crate without its `vali-` prefix, or `adr`, `bench`, `debugger`, `fuzz` or `mise`. Add a scope there when a new crate or area appears. Start the description lower-case (acronyms such as ADR or IR are fine), and don't end it with a full stop.
- **Body:** wrap at 72 characters. URLs, indented or fenced code, and trailers may run longer.
- **Trailers:** only `Co-Authored-By`, plus the `Change-Id` the hook adds.

Install the hooks once per machine with `hk install --global --mise` (Git 2.54 or later), or `hk install --mise` in this clone. They run rustfmt before a commit, then check the message and add the `Change-Id` Gerrit needs.

## CI

Changes under review are checked by `.github/workflows/gerrit-verify.yml`. The gerrithub-ci Worker (https://github.com/go-valkyrie/gerrithub-ci) starts it for each new patch set from a trusted uploader, and for anyone else's once a trusted reviewer comments `recheck`. It runs the lint, test and docs checks and hk's commit-message checks on the patch set, then votes `Verified` on the change.

`.github/workflows/check.yml` runs the parts of `bin/mise run check` (lint, test, docs and deny) and the benchmarks as parallel jobs on PRs and pushes to `main`. It skips changes that only touch `docs/`, `*.md`, `.claude/`, `.jouet/` or `fuzz/`. `fuzz/` is a workspace of its own that needs nightly, and `fuzz.yml` runs it on a schedule.
