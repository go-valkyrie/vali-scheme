//! `vali-bench`: runs the programs in `benchmarks/` and compares runs.
//!
//! - `vali-bench time [--runs N] [--all-strategies] [--out DIR] [NAME...]`
//!   runs each benchmark once to warm up, then `N` times (9 by default),
//!   each on a fresh isolate, under deferred counting or every strategy,
//!   and writes the median, fastest and slowest run to `DIR/times.tsv`.
//! - `vali-bench count [--out DIR] [NAME...]`, built with the `counters`
//!   feature, runs each once and writes the runtime's exact counts to
//!   `DIR/counts.tsv`: they do not change from run to run, so a change in
//!   them is the program's, not the machine's.
//! - `vali-bench compare OLD NEW` prints both side by side: counts with
//!   their exact change, times flagged only when each median lies outside
//!   the other's range of runs.
//!
//! Each benchmark is a program whose header says what its value must
//! print as (`;;; expect: ...`); a run that gives anything else fails.
//! The standard libraries load prebuilt, as they do for every host.
//! `DIR` defaults to `target/vali-bench/latest`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Program, Source};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};

/// Serves nothing: benchmarks use only Vali's own libraries.
struct NoLibraries;

impl Loader for NoLibraries {
    fn load(
        &self,
        _name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

/// One benchmark program.
struct Benchmark {
    name: String,
    source: String,
    expect: String,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The benchmarks in `benchmarks/`, those named in `only` if any, sorted.
fn discover(only: &[String]) -> Result<Vec<Benchmark>, String> {
    let dir = root().join("benchmarks");
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|e| e != "scm") {
            continue;
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !only.is_empty() && !only.contains(&name) {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|e| format!("{name}: {e}"))?;
        let expect = source
            .lines()
            .find_map(|l| l.strip_prefix(";;; expect: "))
            .ok_or_else(|| format!("{name}: no `;;; expect:` line"))?
            .to_owned();
        found.push(Benchmark {
            name,
            source,
            expect,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

fn compile(b: &Benchmark) -> Result<Program, String> {
    vali_compiler::compile_program(&b.source, &Identity::new(&b.name), &NoLibraries)
        .map_err(|e| format!("{}: {e}", b.name))
}

/// Loads and runs `b` once on a fresh isolate: how long running took, and
/// the isolate, to read its counts from.
fn run<S: Strategy>(b: &Benchmark) -> Result<(Duration, Cx<S>), String> {
    let program = compile(b)?;
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    let failed = |e: vali_runtime::Error| format!("{} under {}: {e}", b.name, S::NAME);
    cx.install_primitives().map_err(failed)?;
    let id = vali_compiler::load(&mut cx, program).map_err(failed)?;
    cx.reset_counters();
    let start = Instant::now();
    let result = cx.run(id).map_err(failed)?;
    let elapsed = start.elapsed();
    let written = cx.write_string(&result).map_err(failed)?;
    if written != b.expect {
        return Err(format!(
            "{} under {}: gave {written}, not {}",
            b.name,
            S::NAME,
            b.expect
        ));
    }
    Ok((elapsed, cx))
}

/// The median, fastest and slowest of `runs` runs after one to warm up,
/// in milliseconds.
fn time<S: Strategy>(b: &Benchmark, runs: usize) -> Result<[f64; 3], String> {
    run::<S>(b)?;
    let mut times: Vec<f64> = (0..runs)
        .map(|_| run::<S>(b).map(|(t, _)| t.as_secs_f64() * 1000.0))
        .collect::<Result<_, _>>()?;
    times.sort_by(f64::total_cmp);
    Ok([times[times.len() / 2], times[0], times[times.len() - 1]])
}

/// The commit being measured, if git knows it.
fn commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The options common to `time` and `count`.
struct Options {
    out: PathBuf,
    runs: usize,
    all_strategies: bool,
    only: Vec<String>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        out: root().join("target/vali-bench/latest"),
        runs: 9,
        all_strategies: false,
        only: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => options.out = PathBuf::from(args.next().ok_or("--out needs a directory")?),
            "--runs" => {
                options.runs = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|&n| n > 0)
                    .ok_or("--runs needs a positive number")?;
            }
            "--all-strategies" => options.all_strategies = true,
            name => options.only.push(name.to_owned()),
        }
    }
    Ok(options)
}

fn write(dir: &Path, file: &str, header: &str, rows: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(file);
    let text = format!("# commit {}\n{header}\n{rows}", commit());
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn time_all(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    let mut rows = String::new();
    for b in discover(&options.only)? {
        let mut timings = vec![(
            DeferredHeadered::NAME,
            time::<DeferredHeadered>(&b, options.runs)?,
        )];
        if options.all_strategies {
            timings.push((NoCollection::NAME, time::<NoCollection>(&b, options.runs)?));
            timings.push((
                DeferredSideCounts::NAME,
                time::<DeferredSideCounts>(&b, options.runs)?,
            ));
            timings.push((
                PreciseHeadered::NAME,
                time::<PreciseHeadered>(&b, options.runs)?,
            ));
            timings.push((
                PreciseSideCounts::NAME,
                time::<PreciseSideCounts>(&b, options.runs)?,
            ));
        }
        for (strategy, [median, min, max]) in timings {
            println!(
                "{:16} {strategy:14} {median:9.2} ms  ({min:.2} to {max:.2})",
                b.name
            );
            writeln!(
                rows,
                "{}\t{strategy}\t{median:.3}\t{min:.3}\t{max:.3}",
                b.name
            )
            .expect("writing to a string");
        }
    }
    write(
        &options.out,
        "times.tsv",
        "# benchmark\tstrategy\tmedian ms\tfastest ms\tslowest ms",
        &rows,
    )
}

/// The counters, by name, in the order they are printed.
fn counted(c: &vali_runtime::Counters) -> [(&'static str, u64); 6] {
    [
        ("instructions", c.instructions),
        ("calls", c.calls),
        ("native calls", c.native_calls),
        ("allocations", c.allocations),
        ("words", c.words),
        ("largest frame", c.largest_frame),
    ]
}

fn count_all(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    let mut rows = String::new();
    for b in discover(&options.only)? {
        let (_, cx) = run::<DeferredHeadered>(&b)?;
        let Some(counters) = cx.counters() else {
            return Err(
                "counting needs the counters feature: build with --features counters".into(),
            );
        };
        for (name, value) in counted(&counters) {
            println!("{:16} {name:14} {value:>14}", b.name);
            writeln!(rows, "{}\t{name}\t{value}", b.name).expect("writing to a string");
        }
    }
    write(
        &options.out,
        "counts.tsv",
        "# benchmark\tcounter\tvalue",
        &rows,
    )
}

/// The rows of a results file, by benchmark and metric, if it exists.
fn read(path: &Path) -> Result<BTreeMap<(String, String), Vec<f64>>, String> {
    let mut rows = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(rows);
    };
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut fields = line.split('\t');
        let (Some(name), Some(metric)) = (fields.next(), fields.next()) else {
            return Err(format!("{}: a short line: {line}", path.display()));
        };
        let values = fields
            .map(|f| f.parse::<f64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{}: {e}: {line}", path.display()))?;
        rows.insert((name.to_owned(), metric.to_owned()), values);
    }
    Ok(rows)
}

fn compare(old: &Path, new: &Path) -> Result<(), String> {
    let (old_counts, new_counts) = (
        read(&old.join("counts.tsv"))?,
        read(&new.join("counts.tsv"))?,
    );
    if !old_counts.is_empty() || !new_counts.is_empty() {
        println!(
            "{:16} {:14} {:>14} {:>14} {:>9}",
            "counts", "", "old", "new", "change"
        );
        for ((name, metric), new_value) in &new_counts {
            let Some(old_value) = old_counts.get(&(name.clone(), metric.clone())) else {
                continue;
            };
            let (a, b) = (old_value[0], new_value[0]);
            let change = if a == b {
                "=".to_owned()
            } else if a == 0.0 {
                "new".to_owned()
            } else {
                format!("{:+.1}%", (b - a) / a * 100.0)
            };
            println!("{name:16} {metric:14} {a:>14} {b:>14} {change:>9}");
        }
        println!();
    }
    let (old_times, new_times) = (read(&old.join("times.tsv"))?, read(&new.join("times.tsv"))?);
    if !old_times.is_empty() || !new_times.is_empty() {
        println!(
            "{:16} {:14} {:>10} {:>10} {:>9}",
            "times (ms)", "", "old", "new", "change"
        );
        for ((name, strategy), new) in &new_times {
            let Some(old) = old_times.get(&(name.clone(), strategy.clone())) else {
                continue;
            };
            let (a, b) = (old[0], new[0]);
            // Real only if each median is outside the other's range.
            let outside = (b < old[1] || b > old[2]) && (a < new[1] || a > new[2]);
            let change = if outside {
                format!("{:+.1}%", (b - a) / a * 100.0)
            } else {
                "~".to_owned()
            };
            println!("{name:16} {strategy:14} {a:>10.2} {b:>10.2} {change:>9}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    vali_stdlib::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "time" => time_all(rest),
        Some((command, rest)) if command == "count" => count_all(rest),
        Some((command, [old, new])) if command == "compare" => {
            compare(Path::new(old), Path::new(new))
        }
        _ => Err(
            "usage: vali-bench time [--runs N] [--all-strategies] [--out DIR] [NAME...]\n\
                  \x20      vali-bench count [--out DIR] [NAME...]\n\
                  \x20      vali-bench compare OLD-DIR NEW-DIR"
                .to_owned(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
