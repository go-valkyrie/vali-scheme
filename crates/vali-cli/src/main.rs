//! `vali`: Vali's command. It runs a program file, `vali run FILE [ARGS]`
//! or `vali FILE [ARGS]`, or with no file, or with `repl`, a read-eval-print
//! loop over standard input, in the interaction environment, with R7RS-small
//! imported; `(import ...)` brings in more of Vali's libraries. Prompts
//! appear when standard input is a terminal; input read from a pipe runs as
//! a script would, printing each value. `vali --help` documents the
//! options.
//!
//! A program reaches only what it is granted (ADR 0009): libraries from
//! the search path given with `-I`, and the file, environment and clock
//! capabilities its flags grant, none by default. [`host`] is where that
//! is decided.
//!
//! The REPL and programs use deferred counting, for now the fastest
//! strategy that frees memory (ADR 0003).
//!
//! Libraries compiled from source are saved to a cache and loaded from it
//! on later runs (ADR 0001 #10), unless `--no-cache` says not to; `vali
//! build` fills it ahead of time, and `vali cache` looks after it.
//!
//! Vali's own libraries are loaded from the artifacts `vali-stdlib` built,
//! not expanded; setting `VALI_NO_PREBUILT` compiles them from source, to
//! compare.

mod args;
mod host;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use vali::cache::Cache;
use vali::load::Identity;
use vali::strategy::DeferredHeadered;
use vali::{Error, HeapConfig, Isolate, Repl};

use args::{CacheCommand, Command, HELP, Options};
use host::Host;

fn main() -> ExitCode {
    if std::env::var_os("VALI_NO_PREBUILT").is_none() {
        vali::stdlib::install();
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, mut options) = match args::parse(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("vali: {message}\nTry `vali --help`.");
            return ExitCode::from(2);
        }
    };
    if let Some(paths) = std::env::var_os("VALI_LIBRARY_PATH") {
        options.library_path.extend(std::env::split_paths(&paths));
    }
    match command {
        Command::Help => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("vali {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Repl => repl(options),
        Command::Run { file, args } => run(&file, args, options),
        Command::Build { file } => build(&file, options),
        Command::Cache(which) => cache_command(which, &options),
    }
}

/// The library cache this run uses, if it uses one: in `--cache-dir`, or
/// `VALI_CACHE_DIR`, or the user's cache directory.
fn open_cache(options: &Options) -> Option<Cache> {
    if options.no_cache {
        return None;
    }
    let dir = options.cache_dir.clone().or_else(Cache::user_dir)?;
    let mut cache = Cache::new(dir);
    if let Some(mebibytes) = std::env::var("VALI_CACHE_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        cache = cache.with_max_bytes(mebibytes.saturating_mul(1 << 20));
    }
    Some(cache)
}

/// Says what a compile did with the cache, if `VALI_CACHE_TRACE` is set.
fn trace_cache(report: &vali::cache::CacheReport) {
    if std::env::var_os("VALI_CACHE_TRACE").is_none() {
        return;
    }
    for library in &report.hits {
        eprintln!("vali: cache: loaded {library}");
    }
    for (library, why) in &report.refused {
        eprintln!("vali: cache: refused {library}: {why}");
    }
    for library in &report.stored {
        eprintln!("vali: cache: stored {library}");
    }
    for (library, why) in &report.failed {
        eprintln!("vali: cache: could not store {library}: {why}");
    }
}

/// `vali cache`.
fn cache_command(which: CacheCommand, options: &Options) -> ExitCode {
    let Some(cache) = open_cache(options) else {
        eprintln!("vali: no cache directory: set VALI_CACHE_DIR or pass --cache-dir");
        return ExitCode::from(2);
    };
    match which {
        CacheCommand::Dir => {
            println!("{}", cache.root().display());
            return ExitCode::SUCCESS;
        }
        CacheCommand::Stats => match cache.stats() {
            Ok(stats) => println!(
                "{}: {} entries, {} bytes (kept under {})",
                cache.root().display(),
                stats.entries,
                stats.bytes,
                cache.max_bytes()
            ),
            Err(e) => {
                eprintln!("vali: {}: {e}", cache.root().display());
                return ExitCode::FAILURE;
            }
        },
        CacheCommand::Clean => match cache.clear() {
            Ok(n) => println!("deleted {n} entries from {}", cache.root().display()),
            Err(e) => {
                eprintln!("vali: {}: {e}", cache.root().display());
                return ExitCode::FAILURE;
            }
        },
    }
    ExitCode::SUCCESS
}

/// Reads the program `file`, and sets up the host that serves its
/// libraries: the text with a `#!` line blanked, its identity and the
/// host.
fn prepare(
    file: &str,
    args: Vec<String>,
    mut options: Options,
) -> Result<(String, Identity, Arc<Host>), ExitCode> {
    let mut source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("vali: {file}: {e}");
            return Err(ExitCode::from(2));
        }
    };
    // A `#!` line is for the operating system, not the reader, but a
    // `#!fold-case` directive is the reader's.
    if source.starts_with("#!")
        && !source.starts_with("#!fold-case")
        && !source.starts_with("#!no-fold-case")
    {
        let end = source.find('\n').unwrap_or(source.len());
        source.replace_range(..end, "");
    }
    // The program's own directory is searched last.
    let dir = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    options.library_path.push(dir);
    let command_line: Vec<String> = std::iter::once(file.to_owned()).chain(args).collect();
    let host = Arc::new(Host::new(
        options.library_path,
        options.grants,
        command_line,
    ));
    let identity = Identity::new(file);
    host.serve_program(&identity, Path::new(file));
    Ok((source, identity, host))
}

/// `vali build`: compiles the libraries of the program in `file` into the
/// cache, or the directory `-o` names, without running it.
fn build(file: &str, mut options: Options) -> ExitCode {
    if options.no_cache {
        eprintln!("vali: build makes a cache: --no-cache cannot apply");
        return ExitCode::from(2);
    }
    if let Some(output) = options.output.take() {
        options.cache_dir = Some(output);
    }
    let Some(cache) = open_cache(&options) else {
        eprintln!("vali: no directory to build into: pass -o DIR or --cache-dir DIR");
        return ExitCode::from(2);
    };
    let (source, identity, host) = match prepare(file, Vec::new(), options) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let program = match vali::compile::compile_program_cached(&source, &identity, &*host, &cache) {
        Ok(program) => program,
        Err(e) => {
            eprintln!("vali: {e}");
            return ExitCode::FAILURE;
        }
    };
    trace_cache(&program.cache);
    let report = &program.cache;
    for (library, why) in &report.failed {
        eprintln!(
            "vali: {}: could not write {library}: {why}",
            cache.root().display()
        );
    }
    if !report.failed.is_empty() {
        return ExitCode::FAILURE;
    }
    println!(
        "{}: {} libraries compiled, {} already there",
        cache.root().display(),
        report.stored.len(),
        report.hits.len()
    );
    ExitCode::SUCCESS
}

/// Runs the program in `file`, whose command line is `file` and `args`.
fn run(file: &str, args: Vec<String>, options: Options) -> ExitCode {
    let cache = open_cache(&options);
    let (source, identity, host) = match prepare(file, args, options) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let compiled = match &cache {
        Some(cache) => vali::compile::compile_program_cached(&source, &identity, &*host, cache),
        None => vali::compile::compile_program(&source, &identity, &*host),
    };
    let program = match compiled {
        Ok(program) => program,
        Err(e) => {
            eprintln!("vali: {e}");
            return ExitCode::FAILURE;
        }
    };
    trace_cache(&program.cache);
    let mut isolate = Isolate::<DeferredHeadered>::new(&HeapConfig::default());
    let result = isolate.with(|cx| {
        cx.install_primitives()?;
        // What standard input holds is what the program reads, unless
        // someone is typing it.
        if !std::io::stdin().is_terminal() {
            let mut input = Vec::new();
            if std::io::stdin().read_to_end(&mut input).is_ok() {
                cx.give_input(&String::from_utf8_lossy(&input))?;
            }
        }
        let id = vali::compile::load_with(cx, program, host)?;
        cx.run(id).map(|_| ())
    });
    let (output, error_output) = isolate.with(|cx| (cx.take_output(), cx.take_error_output()));
    print!("{output}");
    eprint!("{error_output}");
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Exit(code)) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(Error::Scheme(e)) => {
            eprintln!("vali: error: {}", e.message);
            for frame in &e.backtrace {
                eprintln!("  at {frame}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("vali: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn repl(options: Options) -> ExitCode {
    let host = Host::new(
        options.library_path,
        options.grants,
        vec!["vali".to_owned()],
    );
    let mut repl = match Repl::<DeferredHeadered>::new(&HeapConfig::default(), Some(Arc::new(host)))
    {
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
