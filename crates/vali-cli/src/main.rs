//! `vali`: Vali's command. With no arguments, or with `repl`, it runs a
//! read-eval-print loop over standard input, in the interaction
//! environment, with R7RS-small imported; `(import ...)` brings in more of
//! Vali's libraries. Prompts appear when standard input is a terminal; input read from
//! a pipe runs as a script would, printing each value.
//!
//! The REPL uses deferred counting, for now the fastest strategy that frees
//! memory (ADR 0003).
//!
//! Vali's own libraries are loaded from the artifacts `vali-stdlib` built,
//! not expanded; setting `VALI_NO_PREBUILT` compiles them from source, to
//! compare.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use vali::strategy::DeferredHeadered;
use vali::{Error, HeapConfig, Repl};

fn main() -> ExitCode {
    if std::env::var_os("VALI_NO_PREBUILT").is_none() {
        vali::stdlib::install();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => repl(),
        [command] if command == "repl" => repl(),
        _ => {
            eprintln!("usage: vali [repl]");
            ExitCode::from(2)
        }
    }
}

fn repl() -> ExitCode {
    let mut repl = match Repl::<DeferredHeadered>::new(&HeapConfig::default(), None) {
        Ok(repl) => repl,
        Err(e) => {
            eprintln!("vali: {e}");
            return ExitCode::FAILURE;
        }
    };
    let interactive = std::io::stdin().is_terminal();
    let mut lines = std::io::stdin().lock().lines();
    let mut input = String::new();
    loop {
        if interactive {
            print!("{}", if input.is_empty() { "> " } else { "... " });
            let _ = std::io::stdout().flush();
        }
        let Some(line) = lines.next() else { break };
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("vali: reading standard input: {e}");
                return ExitCode::FAILURE;
            }
        };
        input.push_str(&line);
        input.push('\n');
        // A datum can span lines: wait for the rest.
        if Repl::<DeferredHeadered>::is_incomplete(&input) {
            continue;
        }
        let result = repl.eval(&input);
        input.clear();
        // What the code wrote to the console comes first.
        let (output, error_output) = repl
            .isolate()
            .with(|cx| (cx.take_output(), cx.take_error_output()));
        print!("{output}");
        eprint!("{error_output}");
        match result {
            Ok(values) => {
                for value in values {
                    println!("{value}");
                }
            }
            Err(Error::Exit(code)) => return ExitCode::from(code.clamp(0, 255) as u8),
            // The message alone: the frames would be the REPL's own.
            Err(Error::Scheme(e)) => eprintln!("error: {}", e.message),
            Err(e) => eprintln!("error: {e}"),
        }
        let _ = std::io::stdout().flush();
    }
    if interactive {
        println!();
    }
    ExitCode::SUCCESS
}
