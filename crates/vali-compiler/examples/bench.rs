//! The tier-0 benchmarks of `vali-runtime`'s `bench` example, compiled
//! from Scheme source instead of assembled by hand. Run with
//! `cargo run --release -p vali-compiler --example bench`, with
//! `--strategy deferred` or `--strategy deferred-side` for a counting
//! strategy.

use std::time::Instant;

use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, Handle, HeapConfig, NoCollection, Strategy,
};

const SOURCE: &str = "
(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))

(define (tak x y z)
  (if (not (< y x))
      z
      (tak (tak (- x 1) y z) (tak (- y 1) z x) (tak (- z 1) x y))))

(define (churn n)
  (let loop ((i 0) (keep '()))
    (if (= i n)
        (length keep)
        (let* ((pair (cons i i))
               (vec (vector pair pair))
               (thunk (lambda () vec)))
          (loop (+ i 1)
                (if (= 0 (remainder i 1000)) (cons (thunk) keep) keep))))))

(define (iota n acc) (if (= n 0) acc (iota (- n 1) (cons n acc))))
(define (sum xs acc) (if (null? xs) acc (sum (cdr xs) (+ acc (car xs)))))
";

/// Runs `run` once to warm up, then five times, and prints the best time
/// and the time per Scheme call.
fn time<S: Strategy>(
    cx: &mut Cx<S>,
    label: &str,
    expected: i64,
    mut run: impl FnMut(&mut Cx<S>) -> Option<i64>,
) {
    let before = cx.fuel_used();
    assert_eq!(run(cx), Some(expected), "{label}");
    let calls = (cx.fuel_used() - before) as f64;
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let result = run(cx);
        best = best.min(start.elapsed().as_secs_f64());
        assert_eq!(result, Some(expected), "{label}");
    }
    println!(
        "{label:20} {:8.2} ms  {:10} calls  {:5.1} ns per call",
        best * 1e3,
        calls,
        best * 1e9 / calls
    );
}

fn main() {
    let module = vali_compiler::compile(SOURCE, "bench").unwrap();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--listing") {
        for function in &module.functions {
            println!("{}", vali_bytecode::disassemble(function));
        }
    }
    // `--strategy none|deferred|deferred-side`: the memory strategy.
    let strategy = args
        .iter()
        .position(|a| a == "--strategy")
        .and_then(|i| args.get(i + 1))
        .map_or("none", String::as_str);
    println!("strategy: {strategy}");
    match strategy {
        "none" => bench::<NoCollection>(module),
        "deferred" => bench::<DeferredHeadered>(module),
        "deferred-side" => bench::<DeferredSideCounts>(module),
        other => panic!("no strategy {other}"),
    }
}

fn bench<S: Strategy>(module: vali_bytecode::Module) {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = cx.load(module).unwrap();
    cx.run(id).unwrap();
    let n = |n: i64| Handle::fixnum(n).unwrap();
    let [fib, tak, churn, iota, sum] =
        ["fib", "tak", "churn", "iota", "sum"].map(|name| cx.variable(id, name).unwrap());

    time(&mut cx, "fib 30", 832_040, |cx| {
        cx.call(&fib, &[n(30)]).unwrap().as_fixnum()
    });
    time(&mut cx, "tak 24 16 8", 9, |cx| {
        cx.call(&tak, &[n(24), n(16), n(8)]).unwrap().as_fixnum()
    });
    time(&mut cx, "churn 1,000,000", 1000, |cx| {
        cx.call(&churn, &[n(1_000_000)]).unwrap().as_fixnum()
    });
    let list = cx
        .call(&iota, &[n(1_000_000), Handle::empty_list()])
        .unwrap();
    time(&mut cx, "sum 1,000,000", 500_000_500_000, |cx| {
        cx.call(&sum, &[list.clone(), n(0)]).unwrap().as_fixnum()
    });
    time(&mut cx, "build 1,000,000", 1, |cx| {
        let list = cx
            .call(&iota, &[n(1_000_000), Handle::empty_list()])
            .unwrap();
        Some(i64::from(list.is_pair()))
    });
    println!("heap: {} MiB", cx.bytes_used() / (1024 * 1024));
}
