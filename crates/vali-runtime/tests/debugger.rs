//! Native debuggers unwind through interpreted frames (ADR 0004 #6).
//!
//! `debugger_child` runs a Scheme procedure that recurses four frames deep
//! and then calls a Rust procedure, which hits a breakpoint. `gdb_backtrace`
//! and `lldb_backtrace` run this test binary under each debugger, stopped
//! there, and check that the backtrace walks from the Rust procedure,
//! through the interpreter and the trampoline, through every interpreted
//! frame, named after its Scheme function, and back into the host.

use std::process::Command;

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
    vali_test_breakpoint();
    Ok(args[0])
}

/// `(define (f n) (if (= n 0) (+ 0 (stop n)) (+ 1 (f (- n 1)))))`: neither
/// call is a tail call, so every frame stays on the stack.
fn run_scheme() -> i64 {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.define_native("stop", Arity::exactly(1), stop).unwrap();
    let mut m = ModuleBuilder::new("debugger");
    let f_var = m.variable("f");
    let stop_var = m.variable("stop");
    let mut f = FunctionBuilder::new(Some("f"), Arity::exactly(1), 7, 0);
    let recur = f.label();
    f.emit(I::LoadInt { dst: 2, value: 0 });
    f.branch(
        I::IfNumEq {
            a: 1,
            b: 2,
            offset: 0,
        },
        recur,
    );
    f.emit(I::GetVariable {
        dst: 5,
        variable: stop_var,
    })
    .emit(I::Mov { dst: 6, src: 1 })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::Return { src: 5 });
    f.bind(recur);
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

#[test]
fn debugger_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    assert_eq!(run_scheme(), 3);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn gdb_backtrace() {
    if Command::new("gdb").arg("--version").output().is_err() {
        eprintln!("gdb is not installed; skipping");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("gdb")
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
    let text = String::from_utf8_lossy(&output.stdout);
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn lldb_backtrace() {
    if Command::new("lldb").arg("--version").output().is_err() {
        eprintln!("lldb is not installed; skipping");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let output = Command::new("lldb")
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
    let text = String::from_utf8_lossy(&output.stdout);
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
