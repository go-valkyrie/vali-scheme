//! The `vali` command's library cache and `vali build`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// `vali`, with a cache directory of its own so tests never touch the
/// user's.
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vali"));
    command
        .env(
            "VALI_CACHE_DIR",
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cache-default"),
        )
        .env_remove("VALI_LIBRARY_PATH")
        .stdin(Stdio::null());
    command
}

fn vali(args: &[&str]) -> Output {
    command().args(args).output().expect("vali runs")
}

/// A fresh directory for one test, with `files` (relative path, text)
/// written in it.
fn directory(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cache-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, text).expect("a file");
    }
    dir
}

fn text(path: &Path) -> &str {
    path.to_str().expect("UTF-8")
}

const HELLO: &str = "(define-library (greet hello)
  (import (scheme base))
  (export hello)
  (begin (define (hello who) (string-append \"hello, \" who))))";

/// A program importing two libraries, one of them from the other.
fn program(name: &str) -> PathBuf {
    directory(
        name,
        &[
            ("lib/greet/hello.sld", HELLO),
            (
                "lib/greet/twice.sld",
                "(define-library (greet twice)
                   (import (scheme base) (greet hello))
                   (export hello-twice)
                   (begin (define (hello-twice who)
                            (string-append (hello who) (hello who)))))",
            ),
            (
                "main.scm",
                "(import (scheme base) (scheme write) (greet twice))
                 (display (hello-twice \"x\"))",
            ),
        ],
    )
}

/// Runs the program in `dir` with its cache traced, in `cache`: its exit
/// code, standard output, and the trace lines, which name each library
/// and what happened to it.
fn traced(dir: &Path, cache: &Path, flags: &[&str]) -> (Option<i32>, String, Vec<String>) {
    let (lib, main) = (dir.join("lib"), dir.join("main.scm"));
    let output = command()
        .args(["--cache-dir", text(cache)])
        .args(flags)
        .args(["-I", text(&lib), text(&main)])
        .env("VALI_CACHE_TRACE", "1")
        .output()
        .expect("vali runs");
    let trace = String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(str::to_owned)
        .collect();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        trace,
    )
}

/// How many libraries the trace says `what` of.
fn count(trace: &[String], what: &str) -> usize {
    let prefix = format!("vali: cache: {what} ");
    trace.iter().filter(|l| l.starts_with(&prefix)).count()
}

fn entries(cache: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in std::fs::read_dir(cache).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            found.push(file.path());
        }
    }
    found
}

#[test]
fn a_second_run_loads_libraries_from_the_cache() {
    let dir = program("warm");
    let cache = dir.join("cache");
    let (code, out, trace) = traced(&dir, &cache, &[]);
    assert_eq!((code, out.as_str()), (Some(0), "hello, xhello, x"));
    assert_eq!(
        (count(&trace, "stored"), count(&trace, "loaded")),
        (2, 0),
        "{trace:?}"
    );
    let (code, out, trace) = traced(&dir, &cache, &[]);
    assert_eq!((code, out.as_str()), (Some(0), "hello, xhello, x"));
    assert_eq!(
        (count(&trace, "stored"), count(&trace, "loaded")),
        (0, 2),
        "{trace:?}"
    );
    // An edit compiles that library again; its importer, whose view of
    // it is the same, is loaded.
    std::fs::write(
        dir.join("lib/greet/hello.sld"),
        HELLO.replace("hello, ", "hi, "),
    )
    .unwrap();
    let (_, out, trace) = traced(&dir, &cache, &[]);
    assert_eq!(out, "hi, xhi, x");
    assert_eq!(
        (count(&trace, "stored"), count(&trace, "loaded")),
        (1, 1),
        "{trace:?}"
    );
    // --no-cache neither reads nor writes.
    let (_, out, trace) = traced(&dir, &cache, &["--no-cache"]);
    assert_eq!((out.as_str(), trace.len()), ("hi, xhi, x", 0));
}

#[test]
fn a_damaged_entry_is_compiled_again() {
    let dir = program("damaged");
    let cache = dir.join("cache");
    traced(&dir, &cache, &[]);
    let files = entries(&cache);
    assert_eq!(files.len(), 2);
    for file in &files {
        std::fs::write(file, b"valicach, but not an entry").unwrap();
    }
    let (code, out, trace) = traced(&dir, &cache, &[]);
    assert_eq!((code, out.as_str()), (Some(0), "hello, xhello, x"));
    assert_eq!(count(&trace, "stored"), 2, "{trace:?}");
    let (_, _, trace) = traced(&dir, &cache, &[]);
    assert_eq!(count(&trace, "loaded"), 2, "{trace:?}");
}

#[test]
fn build_makes_what_a_fresh_process_loads() {
    let dir = program("build");
    let (lib, main, out) = (dir.join("lib"), dir.join("main.scm"), dir.join("out"));
    let build = || vali(&["-I", text(&lib), "build", "-o", text(&out), text(&main)]);
    let output = build();
    assert!(output.status.success(), "{output:?}");
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        said.contains("2 libraries compiled, 0 already there"),
        "{said}"
    );
    assert_eq!(entries(&out).len(), 2);
    // A fresh process, which has only that directory to go on.
    let (code, printed, trace) = traced(&dir, &out, &[]);
    assert_eq!((code, printed.as_str()), (Some(0), "hello, xhello, x"));
    assert_eq!(
        (count(&trace, "loaded"), count(&trace, "stored")),
        (2, 0),
        "{trace:?}"
    );
    // Building again finds them there.
    let said = String::from_utf8_lossy(&build().stdout).into_owned();
    assert!(
        said.contains("0 libraries compiled, 2 already there"),
        "{said}"
    );
    // An error in a library is reported, and exits 1.
    std::fs::write(
        dir.join("lib/greet/hello.sld"),
        "(define-library (greet hello) (import (nope)))",
    )
    .unwrap();
    let output = build();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("(nope)"));
}

#[test]
fn the_cache_directory_comes_from_the_flag_or_the_variable() {
    let dir = program("where");
    let (lib, main) = (dir.join("lib"), dir.join("main.scm"));
    let (by_variable, by_flag) = (dir.join("by-variable"), dir.join("by-flag"));
    let output = command()
        .args(["-I", text(&lib), text(&main)])
        .env("VALI_CACHE_DIR", &by_variable)
        .output()
        .expect("vali runs");
    assert!(output.status.success());
    assert_eq!(entries(&by_variable).len(), 2);
    // The flag wins.
    let output = command()
        .args(["--cache-dir", text(&by_flag), "-I", text(&lib), text(&main)])
        .env("VALI_CACHE_DIR", &by_variable)
        .output()
        .expect("vali runs");
    assert!(output.status.success());
    assert_eq!(entries(&by_flag).len(), 2);
    // `vali cache` looks after it.
    let cache = |what: &str| {
        let output = vali(&["--cache-dir", text(&by_flag), "cache", what]);
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(cache("dir").trim(), text(&by_flag));
    assert!(cache("stats").contains("2 entries"));
    assert!(cache("clean").contains("deleted 2 entries"));
    assert!(entries(&by_flag).is_empty());
}
