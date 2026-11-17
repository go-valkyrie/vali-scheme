//! The standard procedures implemented in Rust.

use vali_runtime::{Cx, Error, HeapConfig, NoCollection};

fn cx() -> Cx<NoCollection> {
    let mut cx = Cx::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    cx
}

/// Calls the global `name` with the datums in `args`, and writes the result.
fn call(cx: &mut Cx<NoCollection>, name: &str, args: &str) -> Result<String, Error> {
    let procedure = cx.global(name).unwrap_or_else(|| panic!("no {name}"));
    let args = cx.read(args).unwrap();
    let result = cx.call(&procedure, &args)?;
    cx.write_string(&result)
}

fn ok(cx: &mut Cx<NoCollection>, name: &str, args: &str) -> String {
    call(cx, name, args).unwrap_or_else(|e| panic!("({name} {args}): {e}"))
}

#[test]
fn arithmetic_and_comparison() {
    let mut cx = cx();
    let cases = [
        ("+", "", "0"),
        ("+", "1 2 3", "6"),
        ("*", "2 3 4", "24"),
        ("-", "10", "-10"),
        ("-", "10 1 2", "7"),
        ("+", "1152921504606846975 1", "1152921504606846976"),
        ("*", "1.5 2", "3.0"),
        ("<", "1 2 3", "#t"),
        ("<", "1 3 2", "#f"),
        (">", "3 2 1", "#t"),
        (">=", "3 3 1", "#t"),
        ("=", "2 2.0", "#t"),
        ("quotient", "-7 2", "-3"),
        ("remainder", "-7 2", "-1"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(ok(&mut cx, name, args), expected, "({name} {args})");
    }
}

#[test]
fn lists() {
    let mut cx = cx();
    let cases = [
        ("cons", "1 2", "(1 . 2)"),
        ("car", "(a b)", "a"),
        ("cdr", "(a b)", "(b)"),
        ("list", "1 \"two\" #\\3", "(1 \"two\" #\\3)"),
        ("length", "(1 2 3)", "3"),
        ("reverse", "(1 2 3)", "(3 2 1)"),
        ("append", "(1 2) (3) 4", "(1 2 3 . 4)"),
        ("append", "", "()"),
        ("null?", "()", "#t"),
        ("pair?", "()", "#f"),
        ("not", "#f", "#t"),
        ("not", "()", "#f"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(ok(&mut cx, name, args), expected, "({name} {args})");
    }
}

#[test]
fn equivalence() {
    let mut cx = cx();
    let cases = [
        ("eq?", "a a", "#t"),
        ("eq?", "(a) (a)", "#f"),
        ("eqv?", "2.0 2.0", "#t"),
        ("eqv?", "0.0 -0.0", "#f"),
        ("eqv?", "+nan.0 +nan.0", "#t"),
        ("eqv?", "2 2.0", "#f"),
        (
            "eqv?",
            "123456789012345678901234567890 123456789012345678901234567890",
            "#t",
        ),
        (
            "equal?",
            "(1 #(2 \"x\") #u8(3)) (1 #(2 \"x\") #u8(3))",
            "#t",
        ),
        ("equal?", "(1 #(2 \"x\")) (1 #(2 \"y\"))", "#f"),
        ("equal?", "#0=(a b . #0#) #1=(a b . #1#)", "#t"),
        ("equal?", "#0=(a b . #0#) #1=(a c . #1#)", "#f"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(ok(&mut cx, name, args), expected, "({name} {args})");
    }
}

#[test]
fn vectors() {
    let mut cx = cx();
    assert_eq!(ok(&mut cx, "vector", "1 a"), "#(1 a)");
    assert_eq!(ok(&mut cx, "make-vector", "2 x"), "#(x x)");
    assert_eq!(ok(&mut cx, "make-vector", "2"), "#(#f #f)");
    assert_eq!(ok(&mut cx, "vector-ref", "#(a b c) 1"), "b");
    assert_eq!(ok(&mut cx, "vector-length", "#(a b c)"), "3");
}

#[test]
fn errors() {
    let mut cx = cx();
    let message = |cx: &mut Cx<NoCollection>, name: &str, args: &str| match call(cx, name, args) {
        Err(Error::Scheme(e)) => e.message,
        other => panic!("({name} {args}) gave {other:?}"),
    };
    assert_eq!(message(&mut cx, "car", "1"), "car: expected a pair, got 1");
    assert!(message(&mut cx, "vector-ref", "#(a) 9").contains("out of range"));
    assert!(message(&mut cx, "length", "(1 . 2)").contains("a proper list"));
    assert_eq!(
        message(&mut cx, "quotient", "1 0"),
        "quotient: division by zero"
    );
    assert!(message(&mut cx, "+", "1 a").contains("expected a number"));
    assert!(message(&mut cx, "cons", "1").contains("wrong number of arguments"));
}

#[test]
fn strings_symbols_and_characters() {
    let mut cx = cx();
    let cases = [
        ("string-length", "\"héllo\"", "5"),
        ("string-ref", "\"abc\" 1", "#\\b"),
        ("string-ref", "\"héllo\" 1", "#\\é"),
        ("string=?", "\"a\" \"a\" \"a\"", "#t"),
        ("string=?", "\"a\" \"b\"", "#f"),
        ("string<?", "\"a\" \"b\" \"c\"", "#t"),
        ("string-append", "\"ab\" \"\" \"cd\"", "\"abcd\""),
        ("substring", "\"héllo\" 1 4", "\"éll\""),
        ("string->list", "\"ab\"", "(#\\a #\\b)"),
        ("list->string", "(#\\a #\\λ)", "\"aλ\""),
        ("symbol->string", "abc", "\"abc\""),
        ("string->symbol", "\"hello world\"", "|hello world|"),
        ("symbol?", "abc", "#t"),
        ("string?", "abc", "#f"),
        ("char->integer", "#\\A", "65"),
        ("integer->char", "955", "#\\λ"),
        ("char=?", "#\\a #\\a", "#t"),
        ("char<?", "#\\a #\\b", "#t"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(ok(&mut cx, name, args), expected, "({name} {args})");
    }
    // string->symbol interns: the same symbol as one read.
    let from_string = {
        let f = cx.global("string->symbol").unwrap();
        let arg = cx.read("\"abc\"").unwrap();
        cx.call(&f, &arg).unwrap()
    };
    let read = cx.read("abc").unwrap();
    assert_eq!(cx.get(&from_string), cx.get(&read[0]));
}

#[test]
fn searching_lists() {
    let mut cx = cx();
    let cases = [
        ("memq", "c (a b c d)", "(c d)"),
        ("memq", "z (a b)", "#f"),
        ("memv", "2.0 (1.0 2.0 3.0)", "(2.0 3.0)"),
        ("member", "(x) (a (x) b)", "((x) b)"),
        ("memq", "(x) (a (x) b)", "#f"),
        ("assq", "b ((a 1) (b 2))", "(b 2)"),
        ("assv", "2 ((1 . one) (2 . two))", "(2 . two)"),
        ("assoc", "\"k\" ((\"j\" . 1) (\"k\" . 2))", "(\"k\" . 2)"),
        ("assq", "z ((a 1))", "#f"),
        ("list-tail", "(a b c) 2", "(c)"),
        ("list-ref", "(a b c) 1", "b"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(ok(&mut cx, name, args), expected, "({name} {args})");
    }
}

#[test]
fn apply_tail_calls_any_procedure() {
    let mut cx = cx();
    let apply = cx.global("apply").unwrap();
    let plus = cx.global("+").unwrap();
    let list = cx.global("list").unwrap();
    let args = cx.read("1 2 (3 4)").unwrap();
    let all: Vec<_> = std::iter::once(plus).chain(args).collect();
    let sum = cx.call(&apply, &all).unwrap();
    assert_eq!(sum.as_fixnum(), Some(10));
    // apply of apply: (apply apply list '(1 (2 3)))
    let args = cx.read("(1 (2 3))").unwrap();
    let nested: Vec<_> = [apply.clone(), list].into_iter().chain(args).collect();
    let result = cx.call(&apply, &nested).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "(1 2 3)");
    // Errors from the applied procedure come back as usual.
    let car = cx.global("car").unwrap();
    let args = cx.read("(5)").unwrap();
    let Err(Error::Scheme(e)) = cx.call(&apply, &[car, args[0].clone()]) else {
        panic!("expected an error");
    };
    assert_eq!(e.message, "car: expected a pair, got 5");
}

/// `(define (g x) (apply + x '(1 2)))`, and `(define (h x) (+ 100 (g x)))`:
/// apply in tail position, and called normally from bytecode.
#[test]
fn apply_from_bytecode() {
    use vali_bytecode::{Arity, Constant, FunctionBuilder, Instruction as I, ModuleBuilder};
    let mut cx = cx();
    let mut m = ModuleBuilder::new("apply");
    let apply = m.variable("apply");
    let plus = m.variable("+");
    let g_var = m.variable("g");
    let h_var = m.variable("h");
    let tail = m.constant(Constant::Pair(
        Box::new(Constant::Integer(1)),
        Box::new(Constant::Pair(
            Box::new(Constant::Integer(2)),
            Box::new(Constant::EmptyList),
        )),
    ));
    let mut g = FunctionBuilder::new(Some("g"), Arity::exactly(1), 6, 0);
    g.emit(I::GetVariable {
        dst: 2,
        variable: apply,
    })
    .emit(I::GetVariable {
        dst: 3,
        variable: plus,
    })
    .emit(I::Mov { dst: 4, src: 1 })
    .emit(I::LoadConstant {
        dst: 5,
        constant: tail,
    })
    .emit(I::TailCall { proc: 2, argc: 3 });
    let g = m.function(g.finish().unwrap());
    let mut h = FunctionBuilder::new(Some("h"), Arity::exactly(1), 7, 0);
    h.emit(I::GetVariable {
        dst: 5,
        variable: g_var,
    })
    .emit(I::Mov { dst: 6, src: 1 })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::AddImm {
        dst: 5,
        a: 5,
        imm: 100,
    })
    .emit(I::Return { src: 5 });
    let h = m.function(h.finish().unwrap());
    let unspecified = m.constant(Constant::Unspecified);
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 2, 0);
    for (function, variable) in [(g, g_var), (h, h_var)] {
        body.emit(I::Closure {
            dst: 1,
            function,
            free: vec![],
        })
        .emit(I::SetVariable { src: 1, variable });
    }
    body.emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    cx.run(id).unwrap();
    let h = cx.variable(id, "h").unwrap();
    let result = cx
        .call(&h, &[vali_runtime::Handle::fixnum(7).unwrap()])
        .unwrap();
    assert_eq!(result.as_fixnum(), Some(110));
}
