//! Linking module variables to library instances (ADR 0005 #4).

use vali_bytecode::{Arity, FunctionBuilder, Instruction as I, Module, ModuleBuilder};
use vali_runtime::{Cx, Handle, HeapConfig, NoCollection};

fn cx() -> Cx<NoCollection> {
    let mut cx = Cx::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    cx
}

/// A library defining `counter` as 0 and `bump!`, which adds one to it,
/// exporting both.
fn counter_library() -> Module {
    let mut m = ModuleBuilder::new("counter");
    let counter = m.defined("counter");
    let bump = m.defined("bump!");
    m.export("counter", counter).export("bump!", bump);
    let mut f = FunctionBuilder::new(Some("bump!"), Arity::exactly(0), 3, 0);
    f.emit(I::GetVariable {
        dst: 1,
        variable: counter,
    })
    .emit(I::AddImm {
        dst: 1,
        a: 1,
        imm: 1,
    })
    .emit(I::SetVariable {
        src: 1,
        variable: counter,
    })
    .emit(I::Return { src: 1 });
    let f = m.function(f.finish().unwrap());
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::LoadInt { dst: 1, value: 0 })
        .emit(I::SetVariable {
            src: 1,
            variable: counter,
        })
        .emit(I::Closure {
            dst: 1,
            function: f,
            free: vec![],
        })
        .emit(I::SetVariable {
            src: 1,
            variable: bump,
        })
        .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    m.finish(body)
}

/// A module whose body calls the imported `bump!` twice and returns the
/// imported `counter`.
fn importer() -> Module {
    let mut m = ModuleBuilder::new("importer");
    let counter = m.imported("counter-lib", "counter");
    let bump = m.imported("counter-lib", "bump!");
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 6, 0);
    for _ in 0..2 {
        body.emit(I::GetVariable {
            dst: 4,
            variable: bump,
        })
        .emit(I::Call { base: 1, argc: 0 });
    }
    body.emit(I::GetVariable {
        dst: 1,
        variable: counter,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    m.finish(body)
}

#[test]
fn an_import_shares_the_exporters_cell() {
    let mut cx = cx();
    let library = cx.load(counter_library()).unwrap();
    cx.run(library).unwrap();
    cx.register_library("counter-lib", library);
    let program = cx.load(importer()).unwrap();
    // The library's own assignments are seen through the import.
    let result = cx.run(program).unwrap();
    assert_eq!(result.as_fixnum(), Some(2));
    let counter = cx.variable(library, "counter").unwrap();
    assert_eq!(counter.as_fixnum(), Some(2));
}

#[test]
fn imports_must_resolve() {
    let mut cx = cx();
    let Err(e) = cx.load(importer()) else {
        panic!("expected the library to be missing");
    };
    assert!(
        e.to_string().contains("library counter-lib is not loaded"),
        "{e}"
    );
    let library = cx.load(counter_library()).unwrap();
    cx.register_library("counter-lib", library);
    let mut m = ModuleBuilder::new("bad");
    let missing = m.imported("counter-lib", "reset!");
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::GetVariable {
        dst: 1,
        variable: missing,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let Err(e) = cx.load(m.finish(body)) else {
        panic!("expected the export to be missing");
    };
    assert!(e.to_string().contains("does not export reset!"), "{e}");
}

#[test]
fn an_importer_cannot_assign() {
    let mut m = ModuleBuilder::new("assigner");
    let counter = m.imported("counter-lib", "counter");
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::LoadInt { dst: 1, value: 5 })
        .emit(I::SetVariable {
            src: 1,
            variable: counter,
        })
        .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let error = vali_bytecode::validate(&m.finish(body)).unwrap_err();
    assert!(error.message.contains("is imported"), "{error}");
}

#[test]
fn a_global_defined_after_loading_is_seen() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("late");
    let late = m.variable("late");
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::GetVariable {
        dst: 1,
        variable: late,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    assert!(cx.run(id).is_err(), "unbound before the host defines it");
    cx.define("late", &Handle::fixnum(7).unwrap()).unwrap();
    assert_eq!(cx.run(id).unwrap().as_fixnum(), Some(7));
}
