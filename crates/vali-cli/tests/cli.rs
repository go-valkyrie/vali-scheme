//! The `vali` command, run on piped input.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `vali`, with a cache directory of its own so tests never touch the
/// user's.
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vali"));
    command.env(
        "VALI_CACHE_DIR",
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli-default-cache"),
    );
    command
}

/// Runs `vali` with `input` on standard input: its exit code, standard
/// output and standard error.
fn vali(input: &str) -> (Option<i32>, String, String) {
    let mut child = command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("vali runs");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(input.as_bytes())
        .expect("vali reads its input");
    let output = child.wait_with_output().expect("vali ends");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_session() {
    let (code, out, err) = vali(
        "(define (fact n)
           (if (= n 0) 1 (* n (fact (- n 1)))))
         (fact 20)
         (display \"hi\") (newline)
         (car 1)
         (values 1 2)
         (import (srfi 1))
         (iota 3)
         (define x 1)
         (exit 4)
         (display \"not reached\")",
    );
    assert_eq!(code, Some(4));
    assert_eq!(out, "2432902008176640000\nhi\n1\n2\n(0 1 2)\n");
    assert_eq!(err, "error: car: expected a pair, got 1\n");
}

#[test]
fn end_of_input_ends_it() {
    let (code, out, _) = vali("(+ 1 2)");
    assert_eq!((code, out.as_str()), (Some(0), "3\n"));
}

#[test]
fn usage() {
    let output = command().arg("--nonsense").output().expect("vali runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option --nonsense"));
}

#[test]
fn help_documents_the_flags() {
    let output = command().arg("--help").output().expect("vali runs");
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--library-path",
        "VALI_LIBRARY_PATH",
        "--allow-read",
        "--allow-write",
        "--allow-file",
        "--allow-env",
        "--allow-clock",
        "--allow-all",
    ] {
        assert!(help.contains(flag), "{flag} is documented");
    }
}

/// A fresh directory for one test, with `files` (relative path, text)
/// written in it.
fn directory(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, text).expect("a file");
    }
    dir
}

/// Runs `vali` with `args`, with nothing on standard input.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = command()
        .args(args)
        .env_remove("VALI_LIBRARY_PATH")
        .env("VALI_TEST_VARIABLE", "granted")
        .stdin(Stdio::null())
        .output()
        .expect("vali runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn path(dir: &Path, file: &str) -> String {
    dir.join(file).to_str().expect("UTF-8").to_owned()
}

const GREETER: &str = "(define-library (greet hello)
  (import (scheme base))
  (export hello)
  (begin (define (hello who) (string-append \"hello, \" who))))";

#[test]
fn a_program_imports_a_library_from_the_search_path() {
    let dir = directory(
        "library",
        &[
            ("lib/greet/hello.sld", GREETER),
            (
                "main.scm",
                "(import (scheme base) (scheme write) (greet hello))
                 (display (hello \"world\")) (newline)",
            ),
        ],
    );
    let (lib, main) = (path(&dir, "lib"), path(&dir, "main.scm"));
    let attached = format!("-I{lib}");
    for args in [
        vec!["-I", &lib, &main],
        vec!["run", "--library-path", &lib, &main],
        vec!["run", &attached, &main],
    ] {
        let (code, out, err) = run(&args);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (Some(0), "hello, world\n", "")
        );
    }
    // Without the path, the library is not found, though the program's
    // own directory is searched.
    let (code, _, err) = run(&[&main]);
    assert_eq!(code, Some(1));
    assert!(err.contains("library (greet hello) not found"), "{err}");
}

#[test]
fn libraries_include_files_beside_them_and_the_program_its_own_directory() {
    let dir = directory(
        "include",
        &[
            (
                "lib/greet/hello.sld",
                "(define-library (greet hello)
                   (import (scheme base))
                   (export hello)
                   (include \"body.scm\"))",
            ),
            ("lib/greet/body.scm", "(define (hello who) (list 'hi who))"),
            // Found because it is beside the program.
            (
                "extra/local.sld",
                "(define-library (local) (import (scheme base)) (export five)
                   (begin (define five 5)))",
            ),
            (
                "extra/main.scm",
                "(import (scheme base) (scheme write) (greet hello) (local))
                 (write (hello five))",
            ),
        ],
    );
    let (code, out, err) = run(&["-I", &path(&dir, "lib"), &path(&dir, "extra/main.scm")]);
    assert_eq!((code, out.as_str(), err.as_str()), (Some(0), "(hi 5)", ""));
}

#[test]
fn the_library_path_variable_is_searched() {
    let dir = directory(
        "variable",
        &[
            ("lib/greet/hello.sld", GREETER),
            (
                "main.scm",
                "(import (scheme base) (scheme write) (greet hello)) (display (hello \"v\"))",
            ),
        ],
    );
    let output = command()
        .arg(path(&dir, "main.scm"))
        .env("VALI_LIBRARY_PATH", path(&dir, "lib"))
        .stdin(Stdio::null())
        .output()
        .expect("vali runs");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello, v");
}

#[test]
fn the_command_line_and_the_exit_status() {
    let dir = directory(
        "args",
        &[
            (
                "args.scm",
                "(import (scheme base) (scheme write) (scheme process-context))
                 (write (cdr (command-line)))
                 (exit (length (cdr (command-line))))",
            ),
            (
                "fail.scm",
                "(import (scheme base) (scheme write)) (display \"before\") (car 1)",
            ),
            ("bad.scm", "(import (scheme base)) (display"),
            (
                "emergency.scm",
                "(import (scheme process-context)) (emergency-exit 9)",
            ),
            ("fine.scm", "(import (scheme base))"),
        ],
    );
    // Options after the file are the program's.
    let (code, out, _) = run(&[&path(&dir, "args.scm"), "a", "-I", "--allow-all"]);
    assert_eq!(
        (code, out.as_str()),
        (Some(3), "(\"a\" \"-I\" \"--allow-all\")")
    );
    let (code, out, _) = run(&["run", &path(&dir, "args.scm")]);
    assert_eq!((code, out.as_str()), (Some(0), "()"));

    let (code, out, err) = run(&[&path(&dir, "fail.scm")]);
    assert_eq!((code, out.as_str()), (Some(1), "before"));
    assert!(err.contains("error: car: expected a pair, got 1"), "{err}");

    let (code, _, err) = run(&[&path(&dir, "bad.scm")]);
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(run(&[&path(&dir, "emergency.scm")]).0, Some(9));
    assert_eq!(run(&[&path(&dir, "fine.scm")]).0, Some(0));
    // A program that cannot be read is a usage error.
    let (code, _, err) = run(&[&path(&dir, "missing.scm")]);
    assert_eq!(code, Some(2));
    assert!(err.contains("missing.scm"), "{err}");
}

#[test]
fn a_program_reads_standard_input() {
    let dir = directory(
        "stdin",
        &[(
            "cat.scm",
            "(import (scheme base) (scheme write)) (write (read-line))",
        )],
    );
    let mut child = command()
        .arg(path(&dir, "cat.scm"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("vali runs");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(b"one\ntwo\n")
        .expect("vali reads");
    let output = child.wait_with_output().expect("vali ends");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "\"one\"");
}

const PROBE: &str = r#"
(import (scheme base) (scheme write) (scheme file) (scheme process-context) (scheme time))
(define (attempt name thunk)
  (display name)
  (display ": ")
  (display (guard (e (#t (if (error-object? e) (error-object-message e) 'raised)))
             (thunk)))
  (newline))
(attempt "read" (lambda () (call-with-input-file (list-ref (command-line) 1) read-line)))
(attempt "write" (lambda ()
  (call-with-output-file (list-ref (command-line) 2) (lambda (p) (write-string "written" p)))
  'ok))
(attempt "env" (lambda () (get-environment-variable "VALI_TEST_VARIABLE")))
(attempt "clock" (lambda () (exact? (current-jiffy))))
"#;

fn probe(dir: &Path, flags: &[&str]) -> String {
    let main = path(dir, "probe.scm");
    let input = path(dir, "input.txt");
    let output = path(dir, "output.txt");
    let _ = std::fs::remove_file(&output);
    let mut args: Vec<&str> = flags.to_vec();
    args.extend([main.as_str(), input.as_str(), output.as_str()]);
    let (code, out, err) = run(&args);
    assert_eq!(code, Some(0), "{err}");
    out
}

#[test]
fn capabilities_are_denied_by_default() {
    let dir = directory(
        "denied",
        &[("probe.scm", PROBE), ("input.txt", "contents\n")],
    );
    let out = probe(&dir, &[]);
    assert!(
        out.contains("read: call-with-input-file: needs the file capability"),
        "{out}"
    );
    assert!(
        out.contains("write: call-with-output-file: needs the file capability"),
        "{out}"
    );
    assert!(
        out.contains("env: get-environment-variable: needs the environment capability"),
        "{out}"
    );
    assert!(
        out.contains("clock: current-jiffy: needs the clock capability"),
        "{out}"
    );
    assert!(!dir.join("output.txt").exists());
}

#[test]
fn each_capability_is_granted_by_its_flag() {
    let dir = directory(
        "granted",
        &[("probe.scm", PROBE), ("input.txt", "contents\n")],
    );
    let all = "read: contents\nwrite: ok\nenv: granted\nclock: #t\n";
    let out = probe(&dir, &["--allow-file", "--allow-env", "--allow-clock"]);
    assert_eq!(out, all);
    assert_eq!(
        std::fs::read_to_string(dir.join("output.txt")).expect("written"),
        "written"
    );
    assert_eq!(probe(&dir, &["-A"]), all);

    // Reading alone leaves writing, the environment and the clock denied.
    let out = probe(&dir, &["--allow-read"]);
    assert!(out.contains("read: contents"), "{out}");
    assert!(
        out.contains("write: open-output-file: needs the file capability (write access)"),
        "{out}"
    );
    assert!(
        out.contains("env: get-environment-variable: needs the environment"),
        "{out}"
    );
    assert!(out.contains("clock: current-jiffy: needs"), "{out}");
    assert!(!dir.join("output.txt").exists());

    // The environment, by name.
    let out = probe(&dir, &["--allow-env=OTHER"]);
    assert!(
        out.contains("env: get-environment-variable: needs the environment"),
        "{out}"
    );
    let out = probe(&dir, &["--allow-env=VALI_TEST_VARIABLE"]);
    assert!(out.contains("env: granted"), "{out}");
}

#[test]
fn file_access_can_be_limited_to_a_directory() {
    let dir = directory(
        "limited",
        &[
            ("probe.scm", PROBE),
            ("input.txt", "contents\n"),
            ("open/other.txt", "x"),
        ],
    );
    let open = path(&dir, "open");
    let out = probe(&dir, &[&format!("--allow-file={open}")]);
    assert!(out.contains("outside the directories granted"), "{out}");
    assert!(!dir.join("output.txt").exists());
    let out = probe(&dir, &[&format!("--allow-file={}", path(&dir, ""))]);
    assert!(out.contains("read: contents"), "{out}");
    assert!(out.contains("write: ok"), "{out}");
    // A path that climbs out of the directory is not inside it.
    let climbing = path(&dir, "open/../input.txt");
    let (code, out, _) = run(&[
        &format!("--allow-read={open}"),
        &path(&dir, "probe.scm"),
        &climbing,
        &path(&dir, "output.txt"),
    ]);
    assert_eq!(code, Some(0));
    assert!(out.contains("outside the directories granted"), "{out}");
}

const LOADER: &str = r#"
(import (scheme base) (scheme write) (scheme eval) (scheme load) (scheme process-context)
        (scheme repl) (scheme r5rs))
(define file (list-ref (command-line) 1))
(define (attempt thunk)
  (display (guard (e (#t (if (error-object? e) (error-object-message e) 'raised)))
             (thunk)))
  (newline))
(attempt (lambda ()
  (load file)
  (list (eval 'loaded-value (interaction-environment))
        (eval '(loaded-twice 4) (interaction-environment)))))
(attempt (lambda ()
  (load file (scheme-report-environment 5))
  'loaded-into-an-environment))
;; A datum that raises ends the load with its error, the ones before it
;; having run; the file is closed either way, and loading again works.
(attempt (lambda ()
  (load (list-ref (command-line) 2))
  'not-reached))
(attempt (lambda ()
  (load file)
  (eval 'before-the-error (interaction-environment))))
"#;

#[test]
fn load_reads_a_file_only_when_reading_is_granted() {
    let dir = directory(
        "load",
        &[
            ("loader.scm", LOADER),
            ("bad.scm", "(define before-the-error 1)\n(car 5)\n"),
            (
                "loaded.scm",
                "(define loaded-value (* 6 7))\n(define (loaded-twice n) (* 2 n))\n",
            ),
        ],
    );
    let loaded = path(&dir, "loaded.scm");
    let bad = path(&dir, "bad.scm");
    let (code, out, err) = run(&[&path(&dir, "loader.scm"), &loaded, &bad]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("needs the file capability"), "{out}");

    // What the file defines is visible in the interaction environment.
    let (code, out, err) = run(&["--allow-read", &path(&dir, "loader.scm"), &loaded, &bad]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        out,
        "(42 8)\nloaded-into-an-environment\ncar: expected a pair, got 5\n1\n"
    );

    // A directory that does not hold the file leaves it out of reach.
    let elsewhere = path(&dir, "elsewhere");
    let (code, out, err) = run(&[
        &format!("--allow-read={elsewhere}"),
        &path(&dir, "loader.scm"),
        &loaded,
    ]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("outside the directories granted"), "{out}");
}

#[test]
fn the_repl_takes_the_same_flags() {
    let dir = directory("repl", &[("lib/greet/hello.sld", GREETER)]);
    let mut child = command()
        .args(["-I", &path(&dir, "lib"), "--allow-clock", "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("vali runs");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(
            b"(import (greet hello))\n(hello \"repl\")\n(exact? (current-jiffy))\n\
              (open-input-file \"/etc/hosts\")\n",
        )
        .expect("vali reads");
    let output = child.wait_with_output().expect("vali ends");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "\"hello, repl\"\n#t\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs the file capability"));
}
