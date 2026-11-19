//! Native debuggers unwind through interpreted frames (ADR 0004 #6).
//!
//! `debugger_child` runs a Scheme procedure that recurses four frames deep
//! and then calls a Rust procedure, which hits a breakpoint. `gdb_backtrace`
//! and `lldb_backtrace` run this test binary under each debugger, stopped
//! there, and check that the backtrace walks from the Rust procedure,
//! through the interpreter and the trampoline, through every interpreted
//! frame, named after its Scheme function, and back into the host.

use vali_bytecode::{Arity, Constant, FunctionBuilder, Instruction as I, ModuleBuilder};
use vali_runtime::{Cx, Error, Handle, HeapConfig, NoCollection, Value};

const CHILD: &str = "VALI_DEBUGGER_CHILD";

/// Where the debugger stops.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn vali_test_breakpoint() {
    std::hint::black_box(());
}

fn stop(_cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    // Taking its address keeps `args` on the stack, where the debuggers can
    // print it even when the tests are optimised.
    std::hint::black_box(&args);
    vali_test_breakpoint();
    Ok(args[0])
}

/// `(define (f n) (if (= n 0) (+ 0 (stop n)) (+ 1 (f (- n 1)))))`: neither
/// call is a tail call, so every frame stays on the stack.
fn run_scheme() -> i64 {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    // SAFETY: `stop` returns only immediates, and keeps nothing.
    unsafe { cx.define_native("stop", Arity::exactly(1), stop) }.unwrap();
    let mut m = ModuleBuilder::new("debugger");
    let f_var = m.variable("f");
    let stop_var = m.variable("stop");
    let mut f = FunctionBuilder::new(Some("f"), Arity::exactly(1), 7, 0);
    let recur = f.label();
    // Positions as if from source, for the extensions' line tables.
    let source = m.source("debugger.scm");
    f.position(source, 1, 0);
    f.emit(I::LoadInt { dst: 2, value: 0 });
    f.branch(
        I::IfNumEq {
            a: 1,
            b: 2,
            offset: 0,
        },
        recur,
    );
    f.position(source, 2, 4);
    f.emit(I::GetVariable {
        dst: 5,
        variable: stop_var,
    })
    .emit(I::Mov { dst: 6, src: 1 })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::Return { src: 5 });
    f.bind(recur);
    f.position(source, 3, 4);
    f.emit(I::GetVariable {
        dst: 5,
        variable: f_var,
    })
    .emit(I::SubImm {
        dst: 6,
        a: 1,
        imm: 1,
    })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::AddImm {
        dst: 5,
        a: 5,
        imm: 1,
    })
    .emit(I::Return { src: 5 });
    let f = m.function(f.finish().unwrap());
    let unspecified = m.constant(Constant::Unspecified);
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::Closure {
        dst: 1,
        function: f,
        free: vec![],
    })
    .emit(I::SetVariable {
        src: 1,
        variable: f_var,
    })
    .emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    cx.run(id).unwrap();
    let f = cx.variable(id, "f").unwrap();
    cx.call(&f, &[Handle::fixnum(3).unwrap()])
        .unwrap()
        .as_fixnum()
        .unwrap()
}

/// Whether the debugger `name` is installed. When it is not, that is a
/// skip on a developer's machine and a failure in CI, where `CI` is set:
/// a skipped test there would pass without checking anything.
#[cfg(target_os = "linux")]
fn available(name: &str) -> bool {
    if std::process::Command::new(name)
        .arg("--version")
        .output()
        .is_ok()
    {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{name} is not installed, and CI must run its tests"
    );
    eprintln!("{name} is not installed; skipping");
    false
}

#[test]
fn debugger_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    assert_eq!(run_scheme(), 3);
}

#[test]
#[cfg(all(feature = "native-frames", target_arch = "x86_64", target_os = "linux"))]
fn gdb_backtrace() {
    if !available("gdb") {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new("gdb")
        .args([
            "-batch",
            "-nx",
            "-ex",
            "break vali_test_breakpoint",
            "-ex",
            "run",
            "-ex",
            "bt",
            "--args",
        ])
        .arg(&exe)
        .args(["--exact", "debugger_child", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let text = transcript(&output);
    let frames: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
    let position = |name: &str| {
        frames
            .iter()
            .position(|f| f.contains(name))
            .unwrap_or_else(|| panic!("no {name} frame in:\n{}", frames.join("\n")))
    };
    let breakpoint = position("vali_test_breakpoint");
    // The trampoline is marked as a signal frame, which GDB shows this way.
    let trampoline = position("<signal handler called>");
    // f 0, f 1 and f 2 are named after their function through their marker
    // stubs; f 3, the bottom frame, keeps the shared stub.
    let named = frames.iter().filter(|f| f.contains(" in f ()")).count();
    let bottom = position("vali_interpreted_entry");
    let host = position("debugger_child");
    assert!(breakpoint < trampoline, "{}", frames.join("\n"));
    assert!(
        trampoline < bottom && bottom < host,
        "{}",
        frames.join("\n")
    );
    assert_eq!(named, 3, "{}", frames.join("\n"));
}

#[test]
#[cfg(all(feature = "native-frames", target_arch = "x86_64", target_os = "linux"))]
fn lldb_backtrace() {
    if !available("lldb") {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = std::process::Command::new("lldb")
        .args([
            "--batch",
            "-o",
            "breakpoint set -n vali_test_breakpoint",
            "-o",
            "process launch -- --exact debugger_child --test-threads=1",
            "-o",
            "bt",
        ])
        .arg(&exe)
        .env(CHILD, "1")
        .output()
        .unwrap();
    let text = transcript(&output);
    // The stop report repeats frame 0; the backtrace follows it.
    let frames: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("frame #"))
        .skip(1)
        .collect();
    let position = |name: &str| {
        frames
            .iter()
            .position(|f| f.contains(name))
            .unwrap_or_else(|| panic!("no {name} frame in:\n{}", frames.join("\n")))
    };
    let breakpoint = position("vali_test_breakpoint");
    let trampoline = position("vali_vm_enter");
    let bottom = position("vali_interpreted_entry");
    let host = position("debugger_child");
    // Named through the JIT interface: `JIT(0x…)\`f + 5`.
    let named = frames.iter().filter(|f| f.contains("`f + ")).count();
    assert!(breakpoint < trampoline, "{}", frames.join("\n"));
    assert!(
        trampoline < bottom && bottom < host,
        "{}",
        frames.join("\n")
    );
    assert_eq!(named, 3, "{}", frames.join("\n"));
}

/// What a debugger printed: its standard output, then its standard
/// error. The extensions report failures on standard error, so a failed
/// assertion shows why a command printed nothing.
#[cfg(target_os = "linux")]
fn transcript(output: &std::process::Output) -> String {
    format!(
        "{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The extensions in `tools/debugger/`.
#[cfg(target_os = "linux")]
fn extension(name: &str) -> String {
    format!("{}/../../tools/debugger/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// What the extensions show of the four frames of `f` and their slots,
/// with or without native frames: their names and positions, from the
/// line table, and their slots as Scheme writes them.
#[cfg(target_os = "linux")]
fn check_scheme_frames(text: &str) {
    assert!(text.contains("#0 f (debugger.scm:2:5)"), "{text}");
    for n in 1..=3 {
        assert!(
            text.contains(&format!("#{n} f (debugger.scm:3:5)")),
            "{text}"
        );
    }
    assert!(text.contains("slot 1 = 0"), "{text}");
    assert!(text.contains("slot 5 = #<procedure stop>"), "{text}");
    assert!(text.contains("slot 1 = 3"), "{text}");
    assert!(
        text.contains("slot 2 = (the header of the call it waits in)"),
        "{text}"
    );
    assert!(text.contains("slot 5 = #<procedure f>"), "{text}");
}

#[test]
#[cfg(target_os = "linux")]
fn gdb_shows_scheme_frames() {
    if !available("gdb") {
        return;
    }
    let output = std::process::Command::new("gdb")
        .args(["-batch", "-nx", "-ex"])
        .arg(format!("source {}", extension("vali_gdb.py")))
        .args([
            "-ex",
            "break vali_test_breakpoint",
            "-ex",
            "run",
            "-ex",
            "vali-bt",
            "-ex",
            "vali-frame 0",
            "-ex",
            "vali-frame 3",
            "-ex",
            "frame function debugger::stop",
            "-ex",
            "print args[0]",
            "-ex",
            "vali-print 0x17",
            "--args",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "debugger_child", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let text = transcript(&output);
    check_scheme_frames(&text);
    // The pretty-printer shows the value `stop` was called with, and
    // `vali-print` decodes a raw word: 0x17 is the empty list.
    assert!(text.contains("= 0\n"), "{text}");
    assert!(text.contains("\n()\n"), "{text}");
}

#[test]
#[cfg(target_os = "linux")]
fn lldb_shows_scheme_frames() {
    if !available("lldb") {
        return;
    }
    let output = std::process::Command::new("lldb")
        .args(["--batch", "-o"])
        .arg(format!(
            "command script import {}",
            extension("vali_lldb.py")
        ))
        .args([
            "-o",
            "breakpoint set -n vali_test_breakpoint",
            "-o",
            "process launch -- --exact debugger_child --test-threads=1",
            "-o",
            "vali-bt",
            "-o",
            "vali-frame 0",
            "-o",
            "vali-frame 3",
            "-o",
            "up",
            "-o",
            "frame variable args.data_ptr[0]",
            "-o",
            "vali-print 0x17",
        ])
        .arg(std::env::current_exe().unwrap())
        .env(CHILD, "1")
        .output()
        .unwrap();
    let text = transcript(&output);
    check_scheme_frames(&text);
    assert!(text.contains("args.data_ptr[0] = 0"), "{text}");
    assert!(text.contains("\n()\n"), "{text}");
}
