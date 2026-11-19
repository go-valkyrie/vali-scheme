//! Upstream conformance suites, vendored under `tests/conformance/`. Run
//! with `cargo test -p vali-test --test conformance`.
//!
//! Each suite is one directory holding the upstream file untouched, a
//! `lib/` directory of shim libraries standing in for the test library it
//! imports, and a `known-failures.scm` the shim reads (see the README in
//! each). The suite runs as a `library-path` host test, under every memory
//! strategy, and fails if a case fails that is not listed as a known
//! failure, or a listed one starts to pass.

use std::path::Path;
use std::sync::Arc;

use libtest_mimic::{Failed, Trial};
use vali_test::{Backend, Evaluate, Header, Kind, TestFile, verdict};

/// The suites: the directory under `tests/conformance/` and the upstream
/// file in it.
const SUITES: &[(&str, &str)] = &[("r7rs", "r7rs-tests.scm")];

/// Blanks out the lines `skipped-lines.txt`, next to the suite, lists
/// (`line<TAB>reason`), keeping every other line where it was so the
/// suite's line numbers still match upstream. A listed line that is no
/// longer in the suite is an error, so the list can't go stale.
fn skip_lines(source: &str, suite: &Path) -> Result<String, Failed> {
    let list = suite.with_file_name("skipped-lines.txt");
    let Ok(list) = std::fs::read_to_string(&list) else {
        return Ok(source.to_owned());
    };
    let skipped: Vec<&str> = list
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    let mut found = vec![false; skipped.len()];
    let kept: Vec<&str> = source
        .lines()
        .map(|line| match skipped.iter().position(|s| *s == line) {
            Some(i) => {
                found[i] = true;
                ""
            }
            None => line,
        })
        .collect();
    if let Some(i) = found.iter().position(|f| !f) {
        return Err(Failed::from(format!(
            "skipped-lines.txt: line not in {}: {}",
            suite.display(),
            skipped[i]
        )));
    }
    Ok(kept.join("\n") + "\n")
}

fn main() {
    let args = libtest_mimic::Arguments::from_args();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance");
    // Vali's own libraries load prebuilt, as they do for every host.
    vali_stdlib::install();
    let trials = SUITES
        .iter()
        .map(|&(name, file)| {
            let path = root.join(name).join(file);
            let evaluate = Arc::new(Evaluate);
            Trial::test(name, move || {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| Failed::from(format!("reading {}: {e}", path.display())))?;
                let source = skip_lines(&source, &path)?;
                let file = TestFile {
                    name: path.file_stem().unwrap().to_string_lossy().into_owned(),
                    path,
                    source,
                };
                let header = Header {
                    requirement: "the suite passes, apart from its known failures".to_owned(),
                    found_in: None,
                    kind: Kind::Host,
                    host: Some("library-path".to_owned()),
                    expect_fail: None,
                };
                verdict(&header, evaluate.run(&file, &header)).map_err(Failed::from)
            })
        })
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
