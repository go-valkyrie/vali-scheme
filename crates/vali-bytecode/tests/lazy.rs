//! A lazily converted module (ADR 0017 #2) converts, function by function,
//! to what converting it whole gives.

use std::sync::Arc;

use vali_bytecode::{Shape, convert, convert_lazy, validate_lazy};
use vali_dist::{Arity, Constant, ConstantId, FunId, Function, Module, Operand, Term, TermId};

fn returns(name: &str, constant: u32) -> Function {
    Function {
        name: Some(name.into()),
        arity: Arity::exactly(0),
        params: vec![],
        free: vec![],
        vars: 0,
        joins: 0,
        terms: vec![Term::Return(Operand::Const(ConstantId(constant)))],
        body: TermId(0),
        inlined: Vec::new(),
    }
}

fn module() -> Module {
    Module {
        name: "lazy".into(),
        constants: vec![Constant::Integer(1), Constant::Integer(2)],
        globals: vec![],
        exports: Vec::new(),
        functions: vec![returns("body", 0), returns("one", 1), returns("two", 0)],
        body: FunId(0),
        sources: Vec::new(),
    }
}

#[test]
fn only_the_body_is_converted_up_front() {
    let ir = module();
    let whole = convert(&ir).unwrap();
    let lazy = convert_lazy(Arc::new(ir)).unwrap();
    validate_lazy(&lazy.module).unwrap();
    assert_eq!(lazy.module.functions[0], whole.functions[0]);
    for f in &lazy.module.functions[1..] {
        assert!(f.code.is_empty());
    }
    // The stubs say what the others need of them.
    for (stub, whole) in lazy.module.functions.iter().zip(&whole.functions) {
        assert_eq!(
            (&stub.name, stub.arity, stub.free),
            (&whole.name, whole.arity, whole.free)
        );
    }
    assert_eq!(lazy.module.constants, whole.constants);
    assert_eq!(lazy.module.variables, whole.variables);
    assert_eq!(lazy.module.sources, whole.sources);
}

#[test]
fn each_function_converts_to_what_converting_the_module_gives() {
    let ir = module();
    let whole = convert(&ir).unwrap();
    let lazy = convert_lazy(Arc::new(ir)).unwrap();
    let shape = Shape::of(&lazy.module);
    for index in 1..3 {
        let converted = lazy.converter.convert(&shape, index).unwrap();
        assert_eq!(converted, whole.functions[index as usize]);
    }
}

#[test]
fn a_function_that_does_not_exist_is_an_error() {
    let lazy = convert_lazy(Arc::new(module())).unwrap();
    let error = lazy
        .converter
        .convert(&Shape::of(&lazy.module), 9)
        .unwrap_err();
    assert_eq!(error.function, Some(9));
}

#[test]
fn a_function_that_disagrees_with_its_stub_is_refused() {
    // Calls and closures were checked against the stub, and a closure is
    // allocated with the stub's free count, so code with another arity or
    // free count must not be accepted in its place.
    let lazy = convert_lazy(Arc::new(module())).unwrap();
    for change in [
        |f: &mut vali_bytecode::Function| f.free += 1,
        |f: &mut vali_bytecode::Function| f.arity = vali_bytecode::Arity::default(),
    ] {
        let mut stubs = lazy.module.clone();
        change(&mut stubs.functions[1]);
        if stubs.functions[1] == lazy.module.functions[1] {
            // `Arity::default` may be this function's arity already.
            stubs.functions[1].arity.required += 1;
        }
        let error = lazy.converter.convert(&Shape::of(&stubs), 1).unwrap_err();
        assert_eq!(error.function, Some(1));
        assert!(error.message.contains("entry"), "{}", error.message);
    }
}
