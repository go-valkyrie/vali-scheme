//! The command line: what to run, with which library path and grants.

use std::path::PathBuf;

use crate::host::Grants;

/// What the command was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// A read-eval-print loop over standard input.
    Repl,
    /// Run a program file, with the rest of the command line as its
    /// arguments.
    Run { file: String, args: Vec<String> },
    /// Compile a program file's libraries to artifacts, without running
    /// it.
    Build { file: String },
    /// Look after the library cache.
    Cache(CacheCommand),
    /// Print the help text.
    Help,
    /// Print the version.
    Version,
}

/// What `vali cache` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum CacheCommand {
    /// Print the cache's directory.
    Dir,
    /// Print how many entries there are and their size.
    Stats,
    /// Delete every entry.
    Clean,
}

/// The options that go with a command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Directories to search for libraries, in order: `-I` ones first,
    /// then `VALI_LIBRARY_PATH`'s.
    pub library_path: Vec<PathBuf>,
    /// The capabilities granted.
    pub grants: Grants,
    /// Run without the library cache (`--no-cache`).
    pub no_cache: bool,
    /// The cache's directory (`--cache-dir`), if given.
    pub cache_dir: Option<PathBuf>,
    /// Where `vali build` puts what it makes (`-o`), if given.
    pub output: Option<PathBuf>,
}

/// The help text.
pub const HELP: &str = "\
vali: Vali Scheme

USAGE:
    vali [OPTIONS]                    read-eval-print loop over standard input
    vali [OPTIONS] repl               the same
    vali [OPTIONS] run FILE [ARGS]    run the R7RS program in FILE
    vali [OPTIONS] FILE [ARGS]        the same
    vali [OPTIONS] build FILE         compile FILE's libraries, without running it
    vali [OPTIONS] cache dir|stats|clean
                                      show, measure or empty the library cache

The program's ARGS are what (command-line) returns after FILE's own name.
Options come before FILE (or `run`'s FILE); everything after FILE is the
program's. A program that ends normally exits with 0; (exit) and
(emergency-exit) set the exit status; an uncaught error prints to standard
error and exits with 1. A usage error, or a FILE that cannot be read,
exits with 2.

LIBRARIES:
    -I DIR, --library-path DIR    search DIR for libraries; repeatable.
                                  (foo bar) is found as DIR/foo/bar.sld,
                                  and a library's includes are read from
                                  beside it, below DIR. Imports that are
                                  not Vali's own are found nowhere else.
    VALI_LIBRARY_PATH             more directories, separated as PATH is,
                                  searched after the -I ones.
    A program's own directory is searched last.

LIBRARY CACHE (ADR 0001 #10):
    Running a program saves each library it compiled from source, as an
    artifact, and loads it from there next time, which takes a fraction of
    the time. Vali's own libraries are not saved: they come with the
    command. A saved library is checked on every load as a compiled one
    always is, so one whose source, dependencies or compiler build have
    changed, or that was damaged or tampered with, is compiled again and
    its entry replaced. The cache is safe to delete, and to share between
    programs and concurrent runs.

    --no-cache             neither read nor write the cache
    --cache-dir DIR        keep the cache in DIR
    VALI_CACHE_DIR         the same; the flag wins. The default is
                           ~/Library/Caches/vali on macOS,
                           $XDG_CACHE_HOME/vali or ~/.cache/vali on Linux and
                           other Unix, %LOCALAPPDATA%\\vali\\cache on Windows.
    VALI_CACHE_SIZE        the most the cache may hold, in MiB (default 256);
                           the entries used least recently are deleted first.
    VALI_CACHE_TRACE       set it to print, for each run, which libraries were
                           loaded from the cache, compiled, or refused.

    vali build FILE compiles the libraries FILE imports and puts their
    artifacts in the cache, the same ones a run would, so a later run, here
    or where the directory is copied to, loads them. With -o DIR they go to
    DIR instead, laid out as a cache: run with --cache-dir DIR to use them.
    Loading still reads each library's source and checks the artifact
    against it. vali build reports an error in the program or its libraries
    as a run would, and exits 1.

    -o DIR, --output DIR   where vali build puts the artifacts

CAPABILITIES (ADR 0009):
    A program reaches nothing outside itself unless it is granted. By
    default it gets no files, environment variables or clock, so code of
    unknown origin is safe to try. Without a grant, the procedures in
    (scheme file), (scheme time) and get-environment-variable(s) in
    (scheme process-context) raise an error saying what is missing.
    (command-line) and (exit) always work.

    --allow-read[=DIR]     read files and test for them, anywhere or only
                           inside DIR (repeatable)
    --allow-write[=DIR]    write, create and delete files, likewise
    --allow-file[=DIR]     both of those
    --allow-env[=NAMES]    read environment variables, all or only the
                           comma-separated NAMES
    --allow-clock          read the clock (current-second, current-jiffy)
    -A, --allow-all        everything above, without limits

OTHER:
    -h, --help             print this text
    -V, --version          print the version

Setting VALI_NO_PREBUILT compiles Vali's own libraries from source
instead of loading the ones built with it.
";

/// Reads the command line (without the command's own name).
pub fn parse(args: &[String]) -> Result<(Command, Options), String> {
    let mut options = Options::default();
    let mut command = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, value) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with("--") => (flag, Some(value)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--" => {
                let Some(file) = iter.next() else {
                    return Err("no program file after --".into());
                };
                return run(file, iter.cloned().collect(), options);
            }
            "-h" | "--help" => return Ok((Command::Help, options)),
            "-V" | "--version" => return Ok((Command::Version, options)),
            "-A" | "--allow-all" => {
                no_value(flag, value)?;
                options.grants = Grants::all();
            }
            "--allow-clock" => {
                no_value(flag, value)?;
                options.grants.clock = true;
            }
            "--allow-read" => options.grants.read.grant(value.map(PathBuf::from)),
            "--allow-write" => options.grants.write.grant(value.map(PathBuf::from)),
            "--allow-file" => {
                options.grants.read.grant(value.map(PathBuf::from));
                options.grants.write.grant(value.map(PathBuf::from));
            }
            "--allow-env" => options
                .grants
                .env
                .grant(value.map(|v| v.split(',').map(str::to_owned).collect())),
            "--no-cache" => {
                no_value(flag, value)?;
                options.no_cache = true;
            }
            "--cache-dir" | "-o" | "--output" => {
                let dir = match value {
                    Some(dir) => dir,
                    None => iter
                        .next()
                        .ok_or_else(|| format!("{flag} needs a directory"))?,
                };
                if flag == "--cache-dir" {
                    options.cache_dir = Some(PathBuf::from(dir));
                } else {
                    options.output = Some(PathBuf::from(dir));
                }
            }
            "-I" | "--library-path" => {
                let dir = match value {
                    Some(dir) => dir,
                    None => iter
                        .next()
                        .ok_or_else(|| format!("{flag} needs a directory"))?,
                };
                options.library_path.push(PathBuf::from(dir));
            }
            _ if arg.starts_with("-I") && !arg.starts_with("--") => {
                options.library_path.push(PathBuf::from(&arg[2..]));
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option {flag}"));
            }
            "run" if command.is_none() => command = Some("run"),
            "build" if command.is_none() => command = Some("build"),
            "cache" if command.is_none() => {
                let which = match iter.next().map(String::as_str) {
                    Some("dir") => CacheCommand::Dir,
                    Some("stats") => CacheCommand::Stats,
                    Some("clean") => CacheCommand::Clean,
                    Some(other) => return Err(format!("unknown cache command {other}")),
                    None => return Err("cache needs dir, stats or clean".into()),
                };
                if let Some(extra) = iter.next() {
                    return Err(format!("cache takes one command, got {extra}"));
                }
                return Ok((Command::Cache(which), options));
            }
            file if command == Some("build") => {
                if let Some(extra) = iter.next() {
                    return Err(format!("build takes one file, got {extra}"));
                }
                return Ok((
                    Command::Build {
                        file: file.to_owned(),
                    },
                    options,
                ));
            }
            "repl" if command.is_none() => {
                if let Some(extra) = iter.next() {
                    return Err(format!("repl takes no arguments, got {extra}"));
                }
                return Ok((Command::Repl, options));
            }
            file => return run(file, iter.cloned().collect(), options),
        }
    }
    match command {
        Some("build") => Err("build needs a program file".into()),
        Some(_) => Err("run needs a program file".into()),
        None => Ok((Command::Repl, options)),
    }
}

fn run(file: &str, args: Vec<String>, options: Options) -> Result<(Command, Options), String> {
    Ok((
        Command::Run {
            file: file.to_owned(),
            args,
        },
        options,
    ))
}

fn no_value(flag: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        Some(_) => Err(format!("{flag} takes no value")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(args: &[&str]) -> Result<(Command, Options), String> {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_file_takes_the_rest() {
        let (command, options) =
            parse_all(&["-I", "lib", "--allow-clock", "x.scm", "-I", "a"]).unwrap();
        assert_eq!(
            command,
            Command::Run {
                file: "x.scm".into(),
                args: vec!["-I".into(), "a".into()]
            }
        );
        assert_eq!(options.library_path, [PathBuf::from("lib")]);
        assert!(options.grants.clock);
    }

    #[test]
    fn run_and_repl() {
        assert_eq!(parse_all(&[]).unwrap().0, Command::Repl);
        assert_eq!(parse_all(&["repl"]).unwrap().0, Command::Repl);
        assert!(matches!(
            parse_all(&["run", "-Ilib", "f"]).unwrap().0,
            Command::Run { .. }
        ));
        assert!(parse_all(&["run"]).is_err());
        assert!(parse_all(&["--nonsense"]).is_err());
    }

    #[test]
    fn build_and_cache() {
        let (command, options) = parse_all(&["build", "-o", "out", "--no-cache", "x.scm"]).unwrap();
        assert_eq!(
            command,
            Command::Build {
                file: "x.scm".into()
            }
        );
        assert_eq!(options.output, Some(PathBuf::from("out")));
        assert!(options.no_cache);
        assert!(parse_all(&["build"]).is_err());
        assert!(parse_all(&["build", "a", "b"]).is_err());
        assert_eq!(
            parse_all(&["cache", "clean"]).unwrap().0,
            Command::Cache(CacheCommand::Clean)
        );
        assert!(parse_all(&["cache"]).is_err());
        assert!(parse_all(&["cache", "nonsense"]).is_err());
        let (_, options) = parse_all(&["--cache-dir=c", "x.scm"]).unwrap();
        assert_eq!(options.cache_dir, Some(PathBuf::from("c")));
        // After the program's file, flags are the program's.
        let (command, _) = parse_all(&["x.scm", "--no-cache"]).unwrap();
        assert!(matches!(command, Command::Run { args, .. } if args == ["--no-cache"]));
    }

    #[test]
    fn grants_accumulate() {
        let (_, options) =
            parse_all(&["--allow-read=a", "--allow-read=b", "--allow-env=X,Y"]).unwrap();
        assert_eq!(
            options.grants.read,
            crate::host::Access::Within(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            options.grants.env,
            crate::host::EnvAccess::Names(vec!["X".into(), "Y".into()])
        );
    }
}
