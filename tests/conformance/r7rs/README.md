# R7RS-small conformance suite

`r7rs-tests.scm` is chibi-scheme's R7RS test suite, vendored unmodified from
https://github.com/ashinn/chibi-scheme (`tests/r7rs-tests.scm`) at commit
`9b0c19aa2eb92e3652f02c782c2e29f3672b40f6`. It is under chibi-scheme's BSD
licence, in `COPYING` beside it (copied from the same commit).

It runs as `cargo test -p vali-test --test conformance`, as one test that
runs the whole file under every memory strategy.

## How it runs

The suite imports `(chibi test)`, which is not one of Vali's standard
libraries, so `lib/chibi/test.sld` stands in for it, served by the
`library-path` host fixture (see `vali-test`'s documentation). It
implements the few macros the suite uses (`test`, `test-begin`,
`test-end`, `test-assert`, `test-values`, `test-error`), compares as chibi
does (floats within 1e-5), and ends the run with exit status 1 if a case
fails or a known failure passes. It prints a summary line, and the key of
every failing case.

## Known failures

A case Vali gets wrong is listed in `known-failures.scm` with the reason,
and the rest of the suite still gates the build. Cases are keyed by the
expression and expected value as the shim prints them. A listed case that
passes fails the build, so delete its entry when you fix it. The file says
which entries are gaps in Vali and which are expectations specific to chibi
that Vali is allowed to differ on.

`skipped-lines.txt` lists upstream lines the harness blanks out before
running (line numbers stay the same). It is only for lines whose failure
ends the run, such as a literal the reader rejects, which no per-case
list can catch.

## Updating

1. Copy `tests/r7rs-tests.scm` and `COPYING` from a new chibi-scheme
   checkout over the files here, and update the commit hash above.
2. Run the test. Add a case that now fails to `known-failures.scm`, with a
   reason, if it is a gap, or fix Vali. Remove entries that now pass, and
   lines from `skipped-lines.txt` that are gone.
