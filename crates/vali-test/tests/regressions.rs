//! The regression list: one test per row of the requirements table, in
//! `tests/regressions/`. Run one with
//! `cargo test -p vali-test --test regressions NAME`.

use std::path::Path;
use std::sync::Arc;

fn main() {
    let args = libtest_mimic::Arguments::from_args();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/regressions");
    let files =
        vali_test::discover(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    assert!(
        !files.is_empty(),
        "no regression tests in {}",
        dir.display()
    );
    let trials = vali_test::trials(files, Arc::new(vali_test::ReadOnly));
    libtest_mimic::run(&args, trials).exit();
}
