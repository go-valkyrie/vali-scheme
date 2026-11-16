//! Vali's test harness.
//!
//! Scheme test files carry a [`Header`] of `;;; key: value` comment lines
//! and assert with SRFI 64. [`discover`] finds them, and [`trials`] turns
//! each into a `cargo test` test, run through a [`Backend`]. A test marked
//! `expect: fail` passes while it fails, and fails the build the moment it
//! passes, so a fixed regression can't stay marked as expected to fail.
//!
//! Until there is an evaluator, the only backend is [`ReadOnly`], which
//! checks that each file reads as Scheme.

mod header;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libtest_mimic::{Failed, Trial};
use vali_syntax::{SourceId, TextSize};

pub use header::{Header, HeaderError, Kind, parse_header};

/// A test file: its name, where it is, and its text.
#[derive(Clone, Debug)]
pub struct TestFile {
    /// The file name without `.scm`, which is also the test's name.
    pub name: String,
    /// The file's path.
    pub path: PathBuf,
    /// The file's contents.
    pub source: String,
}

/// What running a test file produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every assertion held.
    Passed,
    /// An assertion failed, or the code under test raised an error.
    Failed(String),
    /// The backend cannot run this kind of test yet.
    Unsupported(String),
    /// The test file itself is broken, for example it does not read as
    /// Scheme. Always a failure, even for a test expected to fail.
    Invalid(String),
}

/// Runs test files.
pub trait Backend {
    /// Runs `file`, whose header is `header`.
    fn run(&self, file: &TestFile, header: &Header) -> Outcome;
}

/// A backend that only reads each file. Reading errors make the file
/// [`Outcome::Invalid`]; otherwise it is [`Outcome::Unsupported`], since
/// nothing can evaluate it yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOnly;

impl Backend for ReadOnly {
    fn run(&self, file: &TestFile, _header: &Header) -> Outcome {
        let read = vali_syntax::read(&file.source, SourceId(0));
        if read.errors.is_empty() {
            return Outcome::Unsupported("no evaluator yet; the file reads as Scheme".to_owned());
        }
        let errors: Vec<String> = read
            .errors
            .iter()
            .map(|e| {
                let (line, column) = line_column(&file.source, e.range.start());
                format!("{}:{line}:{column}: {}", file.path.display(), e.kind)
            })
            .collect();
        Outcome::Invalid(errors.join("\n"))
    }
}

/// The 1-based line and column of `offset` in `source`, counting columns in
/// characters.
fn line_column(source: &str, offset: TextSize) -> (usize, usize) {
    let before = &source[..usize::from(offset)];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, column)
}

/// Every `.scm` file directly in `dir`, sorted by name.
pub fn discover(dir: &Path) -> std::io::Result<Vec<TestFile>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "scm") {
            let name = path
                .file_stem()
                .expect("a .scm file has a stem")
                .to_string_lossy()
                .into_owned();
            let source = std::fs::read_to_string(&path)?;
            files.push(TestFile { name, path, source });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Decides whether a test passed, given its header and what running it
/// produced.
pub fn verdict(header: &Header, outcome: Outcome) -> Result<(), String> {
    match (&header.expect_fail, outcome) {
        (_, Outcome::Invalid(message)) => Err(message),
        (Some(_), Outcome::Passed) => Err("now passes: remove its `expect: fail` line".to_owned()),
        (Some(_), Outcome::Failed(_) | Outcome::Unsupported(_)) => Ok(()),
        (None, Outcome::Passed) => Ok(()),
        (None, Outcome::Failed(message)) => Err(message),
        (None, Outcome::Unsupported(reason)) => Err(format!(
            "this backend cannot run it ({reason}); mark it `expect: fail` with a reason"
        )),
    }
}

/// One `cargo test` test per file, run through `backend`. Tests expected to
/// fail are labelled `xfail`.
pub fn trials(files: Vec<TestFile>, backend: Arc<dyn Backend + Send + Sync>) -> Vec<Trial> {
    files
        .into_iter()
        .map(|file| {
            let header = parse_header(&file.source);
            let expected_to_fail = header.as_ref().is_ok_and(|h| h.expect_fail.is_some());
            let backend = Arc::clone(&backend);
            let name = file.name.clone();
            let trial = Trial::test(name, move || {
                let header =
                    header.map_err(|e| Failed::from(format!("{}: {e}", file.path.display())))?;
                let outcome = backend.run(&file, &header);
                verdict(&header, outcome).map_err(Failed::from)
            });
            if expected_to_fail {
                trial.with_kind("xfail")
            } else {
                trial
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(expect_fail: Option<&str>) -> Header {
        Header {
            requirement: "r".to_owned(),
            found_in: None,
            kind: Kind::Scheme,
            host: None,
            expect_fail: expect_fail.map(str::to_owned),
        }
    }

    #[test]
    fn expected_failures() {
        let xfail = header(Some("until M1"));
        assert_eq!(verdict(&xfail, Outcome::Failed("x".into())), Ok(()));
        assert_eq!(verdict(&xfail, Outcome::Unsupported("x".into())), Ok(()));
        assert!(
            verdict(&xfail, Outcome::Passed)
                .unwrap_err()
                .contains("now passes")
        );
        assert!(verdict(&xfail, Outcome::Invalid("broken".into())).is_err());
    }

    #[test]
    fn ordinary_tests() {
        let plain = header(None);
        assert_eq!(verdict(&plain, Outcome::Passed), Ok(()));
        assert!(verdict(&plain, Outcome::Failed("x".into())).is_err());
        assert!(
            verdict(&plain, Outcome::Unsupported("x".into()))
                .unwrap_err()
                .contains("expect: fail")
        );
    }

    #[test]
    fn read_only_backend() {
        let file = |source: &str| TestFile {
            name: "t".into(),
            path: "t.scm".into(),
            source: source.into(),
        };
        let h = header(Some(""));
        assert!(matches!(
            ReadOnly.run(&file("(display 1)"), &h),
            Outcome::Unsupported(_)
        ));
        match ReadOnly.run(&file("(display 1)\n  (oops"), &h) {
            Outcome::Invalid(message) => assert!(message.starts_with("t.scm:2:3:"), "{message}"),
            other => panic!("expected invalid, got {other:?}"),
        }
    }
}
