//! The `vali` command, run on piped input.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `vali` with `input` on standard input: its exit code, standard
/// output and standard error.
fn vali(input: &str) -> (Option<i32>, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vali"))
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
    let output = Command::new(env!("CARGO_BIN_EXE_vali"))
        .arg("nonsense")
        .output()
        .expect("vali runs");
    assert_eq!(output.status.code(), Some(2));
}
