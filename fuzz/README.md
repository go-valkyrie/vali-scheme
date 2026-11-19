# Fuzzing

Fuzz targets for [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html). This is a
workspace of its own that needs a nightly toolchain, so building Vali never does. CI runs the
targets on a schedule (`.github/workflows/fuzz.yml`). `bin/mise exec -- cargo fuzz ...` runs the
pinned cargo-fuzz (mise installs it on first use); or `cargo install cargo-fuzz`.

| Target     | What it feeds                                                                                                          |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- |
| `bytecode` | Random run-format bytecode: validated, and if valid, run under every memory strategy. Runs under AddressSanitizer.      |
| `ir`       | Bytes decoded as an IR module (`vali_dist::decode`), then re-encoded, optimised, converted to bytecode and validated.   |
| `library`  | A mutated section of an artifact, loaded and used by a program that expands macros from the library: one of vali-stdlib's (trusted), or a small user library's (untrusted, so it meets the checks trusted ones skip). |

`ir` and `library` are seeded from vali-stdlib's prebuilt artifacts. Generate the seeds first, and
again whenever the compiler or the format changes (they are not committed, since they change with
both):

    cd fuzz
    cargo +nightly run --example seeds --no-default-features

That writes `corpus/ir/` (the `invoke` section of each artifact, which is an IR module) and
`corpus/library/` (for each artifact, of vali-stdlib and of a small user library, and each section:
a mode and library number, a section number, then the section's bytes). Then:

    cargo +nightly fuzz run ir -- -max_total_time=180 -rss_limit_mb=2048 -malloc_limit_mb=1024
    cargo +nightly fuzz run library -- -max_total_time=180 -rss_limit_mb=2048 -malloc_limit_mb=1024

The limits make an input that reserves memory out of proportion to its size a finding, not just a
slow case. `library` is structure-aware (the top bit of its first byte picks the stdlib or the user-library mode, so one corpus covers both): the artifact's container is rebuilt around the mutated
section so a mutation reaches that section's decoder, and a custom mutator overwrites bytes with
numbers that stress counts and indices. The deterministic version of this is
`replacing_any_number_in_an_artifact_never_panics` in
`crates/vali-compiler/tests/it/hostile.rs`, which runs in every check.

A failing input lands in `artifacts/<target>/`. Minimise it with
`cargo +nightly fuzz tmin <target> artifacts/<target>/<file>`, and add it as a regression test in
the crate that failed (`vali-dist`, `vali-bytecode`, `vali-opt` or `vali-compiler`).
