//! The IR's validator and printer.

use vali_dist::{
    Arity, Constant, ConstantId, Effects, FunId, Function, Global, GlobalId, GlobalLink, Inlined,
    JoinId, Module, Operand, Position, Primitive, Term, TermId, Value, Var, decode, encode,
    validate,
};

fn v(n: u32) -> Operand {
    Operand::Var(Var(n))
}

fn c(n: u32) -> Operand {
    Operand::Const(ConstantId(n))
}

fn at(line: u32, column: u32) -> Option<Position> {
    Some(Position {
        source: 0,
        line,
        column,
        inlined: 0,
    })
}

/// Builds a function's arena bottom-up.
#[derive(Default)]
struct Terms(Vec<Term>);

impl Terms {
    fn push(&mut self, term: Term) -> TermId {
        self.0.push(term);
        TermId(self.0.len() as u32 - 1)
    }

    fn bind(&mut self, var: u32, value: Value, body: TermId) -> TermId {
        self.push(Term::Let {
            var: Var(var),
            value,
            body,
            position: None,
        })
    }
}

fn prim(op: Primitive, args: &[Operand]) -> Value {
    Value::Primitive {
        op,
        args: args.to_vec(),
    }
}

/// `(define (sum list) (let loop ((l list) (acc 0)) (if (null? l) acc
/// (loop (cdr l) (+ acc (car l))))))`, with a body that exports it.
fn sum() -> Module {
    let mut t = Terms::default();
    let done = t.push(Term::Return(v(2)));
    let next = t.push(Term::Jump {
        join: JoinId(0),
        args: vec![v(6), v(5)],
    });
    let rest = t.bind(6, prim(Primitive::Cdr, &[v(1)]), next);
    let total = t.bind(5, prim(Primitive::Add, &[v(2), v(4)]), rest);
    let first = t.push(Term::Let {
        var: Var(4),
        value: prim(Primitive::Car, &[v(1)]),
        body: total,
        position: at(3, 22),
    });
    let test = t.push(Term::If {
        test: v(3),
        then: done,
        otherwise: first,
    });
    let handler = t.bind(3, prim(Primitive::IsNull, &[v(1)]), test);
    let start = t.push(Term::Jump {
        join: JoinId(0),
        args: vec![v(0), c(0)],
    });
    let body = t.push(Term::Join {
        join: JoinId(0),
        params: vec![Var(1), Var(2)],
        recursive: true,
        handler,
        body: start,
    });
    let sum = Function {
        name: Some("sum".into()),
        arity: Arity::exactly(1),
        params: vec![Var(0)],
        free: vec![],
        vars: 7,
        joins: 1,
        terms: t.0,
        body,
        inlined: vec![],
    };

    let mut t = Terms::default();
    let ret = t.push(Term::Return(v(1)));
    let set = t.bind(1, Value::SetGlobal(GlobalId(0), v(0)), ret);
    let body = t.bind(
        0,
        Value::Closure {
            function: FunId(1),
            free: vec![],
        },
        set,
    );
    let main = Function {
        name: None,
        arity: Arity::exactly(0),
        params: vec![],
        free: vec![],
        vars: 2,
        joins: 0,
        terms: t.0,
        body,
        inlined: vec![],
    };
    Module {
        name: "(example sum)".into(),
        constants: vec![Constant::Integer(0)],
        globals: vec![Global {
            name: "sum".into(),
            link: GlobalLink::Defined,
        }],
        exports: vec![("sum".into(), GlobalId(0))],
        functions: vec![main, sum],
        body: FunId(0),
        sources: vec!["example/sum.sld".into()],
    }
}

#[test]
fn a_loop_validates_and_prints() {
    let module = sum();
    validate(&module).unwrap();
    let expected = r#"module (example sum)
source 0 "example/sum.sld"
c0 = 0
g0 = sum defined
export sum = g0
body f0

fun f0 () []:
  let v0 = closure f1 []
  let v1 = set-global g0 v0
  return v1

fun f1 sum (v0) []:
  rec join j0(v1 v2):
    let v3 = null? v1
    if v3:
      return v2
    else:
      let v4 = car v1 @0:3:22
      let v5 = + v2 v4
      let v6 = cdr v1
      jump j0(v6 v5)
  jump j0(v0 c0)
"#;
    assert_eq!(module.to_string(), expected);
}

/// The error validating `module` gives after `change`.
fn broken(change: impl FnOnce(&mut Module)) -> String {
    let mut module = sum();
    change(&mut module);
    validate(&module).unwrap_err().to_string()
}

fn sum_terms(module: &mut Module) -> &mut Vec<Term> {
    &mut module.functions[1].terms
}

#[test]
fn malformed_modules_are_refused() {
    // The loop's parameters are not in scope after it.
    let error = broken(|m| {
        sum_terms(m)[7] = Term::Jump {
            join: JoinId(0),
            args: vec![v(1), c(0)],
        };
    });
    assert_eq!(error, "function f1: v1 is used where it is not in scope");

    // A join point that is not recursive cannot be jumped to from its
    // handler.
    let error = broken(|m| {
        let Term::Join { recursive, .. } = &mut sum_terms(m)[8] else {
            unreachable!()
        };
        *recursive = false;
    });
    assert_eq!(error, "function f1: join point j0 is not in scope");

    let error = broken(|m| {
        sum_terms(m)[7] = Term::Jump {
            join: JoinId(0),
            args: vec![v(0)],
        };
    });
    assert_eq!(
        error,
        "function f1: a jump to j0 with 1 arguments for 2 parameters"
    );

    // Both arms of the `if` are the same term.
    let error = broken(|m| {
        let Term::If { otherwise, .. } = &mut sum_terms(m)[5] else {
            unreachable!()
        };
        *otherwise = TermId(0);
    });
    assert_eq!(error, "function f1: term t0 is part of the body twice");

    let error = broken(|m| sum_terms(m).push(Term::Return(c(0))));
    assert_eq!(error, "function f1: term t9 is not part of the body");

    let error = broken(|m| {
        let Term::Let { var, .. } = &mut sum_terms(m)[2] else {
            unreachable!()
        };
        *var = Var(0);
    });
    assert_eq!(error, "function f1: v0 is bound twice");

    let error = broken(|m| {
        let Term::Let { value, .. } = &mut sum_terms(m)[3] else {
            unreachable!()
        };
        *value = prim(Primitive::Add, &[v(2)]);
    });
    assert_eq!(error, "function f1: + with 1 operands for 2");

    let error = broken(|m| {
        m.functions[0].terms[2] = Term::Let {
            var: Var(0),
            value: Value::Closure {
                function: FunId(1),
                free: vec![c(0)],
            },
            body: TermId(1),
            position: None,
        };
    });
    assert_eq!(
        error,
        "function f0: closure of f1 with 1 free variables for 0"
    );

    let error = broken(|m| m.globals[0].link = GlobalLink::Imported("(scheme base)".into()));
    assert_eq!(error, "function f0: g0 is assigned but not defined here");

    let error = broken(|m| m.sources.clear());
    assert_eq!(error, "function f1: source 0 does not exist");

    let error = broken(|m| m.exports.push(("sum".into(), GlobalId(0))));
    assert_eq!(error, "sum is exported twice");

    let error = broken(|m| m.body = FunId(1));
    assert_eq!(error, "the body f1 takes arguments or free variables");
}

#[test]
fn constants_must_be_canonical() {
    let int = |n| Box::new(Constant::Integer(n));
    let cases = [
        (
            Constant::Ratio(int(2), int(4)),
            "a ratio must be in lowest terms",
        ),
        (
            Constant::Ratio(int(1), int(1)),
            "a ratio's denominator must be above 1",
        ),
        (
            Constant::Bignum {
                negative: false,
                limbs: vec![5],
            },
            "a bignum must need more than 64 bits, with no zero top limb",
        ),
        (
            Constant::Complex(int(1), Box::new(Constant::Flonum(1.0))),
            "a complex number's parts must be both exact or both flonums",
        ),
    ];
    for (constant, why) in cases {
        let error = broken(|m| m.constants.push(Constant::Pair(Box::new(constant), int(0))));
        assert_eq!(error, format!("constant c1: {why}"));
    }
    let mut module = sum();
    module.constants.push(Constant::Bignum {
        negative: true,
        limbs: vec![1 << 63],
    });
    assert!(validate(&module).is_err(), "-2^63 fits in 64 bits");
    module.constants[1] = Constant::Bignum {
        negative: false,
        limbs: vec![1 << 63],
    };
    validate(&module).unwrap();
}

/// `#0=(a #1=(b) #1# . #0#)`: a list that contains a shared sublist and
/// is its own tail.
fn labelled() -> Constant {
    let sym = |s: &str| Box::new(Constant::Symbol(s.into()));
    Constant::Labelled(
        0,
        Box::new(Constant::Pair(
            sym("a"),
            Box::new(Constant::Pair(
                Box::new(Constant::Labelled(
                    1,
                    Box::new(Constant::Pair(sym("b"), Box::new(Constant::EmptyList))),
                )),
                Box::new(Constant::Pair(
                    Box::new(Constant::LabelRef(1)),
                    Box::new(Constant::LabelRef(0)),
                )),
            )),
        )),
    )
}

#[test]
fn labels_must_be_defined_in_order() {
    let pair = |a, b| Constant::Pair(Box::new(a), Box::new(b));
    let label = |n, c| Constant::Labelled(n, Box::new(c));
    let nil = || Constant::EmptyList;
    validate(&{
        let mut module = sum();
        module.constants.push(labelled());
        module
    })
    .unwrap();
    let cases = [
        (
            pair(Constant::LabelRef(0), nil()),
            "label 0 is referred to before it is defined",
        ),
        (
            pair(label(0, nil()), pair(Constant::LabelRef(1), nil())),
            "label 1 is referred to before it is defined",
        ),
        (
            pair(pair(Constant::LabelRef(0), nil()), label(0, nil())),
            "label 0 is referred to before it is defined",
        ),
        (
            pair(label(1, nil()), nil()),
            "label 1 is out of order: expected label 0",
        ),
        (
            pair(label(0, nil()), label(0, nil())),
            "label 0 is out of order: expected label 1",
        ),
        (
            label(0, Constant::LabelRef(0)),
            "label 0 stands for nothing but itself",
        ),
        (
            label(0, label(1, Constant::LabelRef(0))),
            "label 0 stands for nothing but itself",
        ),
    ];
    for (constant, why) in cases {
        let error = broken(|m| m.constants.push(constant.clone()));
        assert_eq!(error, format!("constant c1: {why}"), "{constant}");
    }
}

#[test]
fn constants_print_as_data() {
    let list = Constant::Pair(
        Box::new(Constant::Symbol("a".into())),
        Box::new(Constant::Pair(
            Box::new(Constant::Vector(vec![
                Constant::Integer(1),
                Constant::String("x\n".into()),
            ])),
            Box::new(Constant::Character(' ')),
        )),
    );
    assert_eq!(list.to_string(), r#"(a #(1 "x\n") . #\x20)"#);
    assert_eq!(labelled().to_string(), "#0=(a #1=(b) #1# . #0#)");
    assert_eq!(Constant::Flonum(f64::NAN).to_string(), "+nan.0");
    assert_eq!(
        Constant::Ratio(
            Box::new(Constant::Integer(-1)),
            Box::new(Constant::Integer(3))
        )
        .to_string(),
        "-1/3"
    );
}

#[test]
fn deep_nesting_does_not_recurse() {
    const DEPTH: u32 = 200_000;
    let mut t = Terms::default();
    let mut body = t.push(Term::Return(v(DEPTH - 1)));
    for n in (1..DEPTH).rev() {
        body = t.bind(n, prim(Primitive::Cons, &[c(0), v(n - 1)]), body);
    }
    body = t.bind(0, Value::Operand(c(0)), body);
    let mut list = Constant::EmptyList;
    for _ in 0..DEPTH {
        list = Constant::Pair(Box::new(Constant::Integer(1)), Box::new(list));
    }
    let module = Module {
        name: "deep".into(),
        constants: vec![list],
        globals: vec![],
        exports: vec![],
        functions: vec![Function {
            name: None,
            arity: Arity::exactly(0),
            params: vec![],
            free: vec![],
            vars: DEPTH,
            joins: 0,
            terms: t.0,
            body,
            inlined: vec![],
        }],
        body: FunId(0),
        sources: vec![],
    };
    validate(&module).unwrap();
    assert!(module.to_string().len() > DEPTH as usize * 20);
}

#[test]
fn effects() {
    assert!(Primitive::Eq.effects().is_pure());
    assert!(Primitive::Car.effects().contains(Effects::RAISE));
    assert!(!Primitive::Car.effects().contains(Effects::WRITE));
    assert_eq!(
        Primitive::VectorSet.effects().to_string(),
        "write may-raise"
    );
    let call = Value::Call {
        callee: v(0),
        args: vec![],
    };
    assert!(call.effects().contains(Effects::CAPTURE));
    for &op in Primitive::ALL {
        assert_eq!(Primitive::named(op.name()), Some(op));
        assert!(!op.effects().contains(Effects::CAPTURE));
    }
}

mod encoding {
    use super::*;
    use vali_dist::{Artifact, decode, encode};

    #[test]
    fn a_module_survives_encoding() {
        let module = sum();
        let bytes = encode(&module).unwrap();
        let decoded = decode(&bytes).unwrap();
        // Its variables were numbered in binding order already, so only
        // the arena's order differs: the printed form is the same.
        assert_eq!(decoded.to_string(), module.to_string());
        assert_eq!(encode(&decoded).unwrap(), bytes);
    }

    #[test]
    fn constants_survive_encoding() {
        let mut module = sum();
        let mut list = Constant::EmptyList;
        for n in 0..1000 {
            list = Constant::Pair(Box::new(Constant::Integer(n - 500)), Box::new(list));
        }
        module.constants.extend([
            list,
            Constant::Vector(vec![
                Constant::Flonum(-0.5),
                Constant::Character('λ'),
                Constant::String("é\n".into()),
                Constant::Keyword("k".into()),
                Constant::Bytevector(vec![0, 255]),
                Constant::Vector(Vec::new()),
            ]),
            Constant::Bignum {
                negative: true,
                limbs: vec![1, 2, 3],
            },
            Constant::Ratio(
                Box::new(Constant::Integer(-1)),
                Box::new(Constant::Integer(3)),
            ),
            Constant::Complex(
                Box::new(Constant::Flonum(1.0)),
                Box::new(Constant::Flonum(f64::INFINITY)),
            ),
            Constant::Integer(i64::MIN),
            Constant::Integer(i64::MAX),
        ]);
        let decoded = decode(&encode(&module).unwrap()).unwrap();
        assert_eq!(decoded.constants, module.constants);
    }

    #[test]
    fn labelled_constants_survive_encoding() {
        let mut module = sum();
        module.constants.extend([
            labelled(),
            Constant::Vector(vec![
                Constant::Labelled(0, Box::new(Constant::String("s".into()))),
                Constant::LabelRef(0),
            ]),
        ]);
        let bytes = encode(&module).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.constants, module.constants);
        assert_eq!(encode(&decoded).unwrap(), bytes);
    }

    #[test]
    fn deep_nesting_does_not_recurse_in_encoding() {
        const DEPTH: u32 = 200_000;
        let mut t = Terms::default();
        let mut body = t.push(Term::Return(v(DEPTH - 1)));
        for n in (1..DEPTH).rev() {
            body = t.bind(n, prim(Primitive::Cons, &[c(0), v(n - 1)]), body);
        }
        body = t.bind(0, Value::Operand(c(0)), body);
        let mut list = Constant::EmptyList;
        for _ in 0..DEPTH {
            list = Constant::Pair(Box::new(Constant::Integer(1)), Box::new(list));
        }
        let module = Module {
            name: "deep".into(),
            constants: vec![list],
            globals: vec![],
            exports: vec![],
            functions: vec![Function {
                name: None,
                arity: Arity::exactly(0),
                params: vec![],
                free: vec![],
                vars: DEPTH,
                joins: 0,
                terms: t.0,
                body,
                inlined: vec![],
            }],
            body: FunId(0),
            sources: vec![],
        };
        let bytes = encode(&module).unwrap();
        assert_eq!(encode(&decode(&bytes).unwrap()).unwrap(), bytes);
    }

    /// A small random number generator, so the tests need no crate.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn corrupt_input_is_refused_not_panicked_on() {
        let bytes = encode(&sum()).unwrap();
        for end in 0..bytes.len() {
            assert!(
                decode(&bytes[..end]).is_err(),
                "a prefix of {end} bytes decoded"
            );
        }
        for i in 0..bytes.len() {
            for bit in 0..8 {
                let mut flipped = bytes.clone();
                flipped[i] ^= 1 << bit;
                let _ = decode(&flipped);
            }
        }
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..20_000 {
            let len = (rng.next() % 64) as usize;
            let random: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let _ = decode(&random);
            let mut spliced = bytes.clone();
            let at = (rng.next() as usize) % spliced.len();
            spliced.splice(at..at, random);
            let _ = decode(&spliced);
        }
    }

    #[test]
    fn primitives_are_encoded_by_name_not_position() {
        let bytes = encode(&sum()).unwrap();
        let find = |needle: &[u8]| bytes.windows(needle.len()).position(|w| w == needle);
        // `car` is in the bytes by its name, so reordering `Primitive::ALL`
        // changes nothing about what these bytes say.
        let at = find(b"\x03car").expect("the name is written, with its length");
        let mut other = bytes.clone();
        other[at + 1..at + 4].copy_from_slice(b"cxr");
        let error = decode(&other).unwrap_err();
        assert!(error.message.contains("unknown primitive cxr"), "{error}");
        for &p in Primitive::ALL {
            assert_eq!(Primitive::named(p.name()), Some(p));
        }
    }

    #[test]
    fn artifacts_keep_what_they_know() {
        let mut artifact = Artifact::default();
        artifact.insert("invoke", true, vec![1, 2, 3]);
        artifact.insert("debug", false, vec![4]);
        let bytes = artifact.encode();
        let read = Artifact::decode(&bytes, &["invoke", "debug"]).unwrap();
        assert_eq!(read, artifact);
        // An optional section it does not know is dropped.
        let read = Artifact::decode(&bytes, &["invoke"]).unwrap();
        assert_eq!(read.section("invoke"), Some(&[1, 2, 3][..]));
        assert_eq!(read.section("debug"), None);
        // A required one is refused, by name.
        let error = Artifact::decode(&bytes, &["debug"]).unwrap_err();
        assert!(
            error.message.contains("needs the section invoke"),
            "{error}"
        );
        let mut newer = artifact.clone();
        newer.features.push("wide-slots".into());
        let error = Artifact::decode(&newer.encode(), &["invoke", "debug"]).unwrap_err();
        assert!(
            error.message.contains("format feature wide-slots"),
            "{error}"
        );
        assert!(Artifact::decode(b"not an artifact", &[]).is_err());
        for end in 0..bytes.len() {
            assert!(Artifact::decode(&bytes[..end], &["invoke", "debug"]).is_err());
        }
    }
}

/// `sum` with the `car` in its loop inside a copy of a function `first`,
/// inlined where the function's own code called it.
fn with_copy() -> Module {
    let mut module = sum();
    module.functions[1].inlined = vec![Inlined {
        name: Some("first".into()),
        call: at(2, 4),
    }];
    let Term::Let { position, .. } = &mut sum_terms(&mut module)[4] else {
        panic!("term 4 is the car");
    };
    position.as_mut().expect("the car has a position").inlined = 1;
    module
}

#[test]
fn inlined_copies_print_round_trip_and_are_checked() {
    let module = with_copy();
    validate(&module).unwrap();
    let printed = module.to_string();
    assert!(printed.contains("car v1 @0:3:22/i0\n"), "{printed}");
    assert!(printed.contains("  inlined i0 first @0:2:4\n"), "{printed}");
    let bytes = encode(&module).unwrap();
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.to_string(), printed);
    assert_eq!(encode(&decoded).unwrap(), bytes);
    // A position in a copy that does not exist.
    let mut missing = with_copy();
    missing.functions[1].inlined.clear();
    assert!(
        validate(&missing)
            .unwrap_err()
            .to_string()
            .contains("inlined copy i0 does not exist")
    );
    // A copy called from itself, which a backtrace would follow forever.
    let mut cycle = with_copy();
    cycle.functions[1].inlined[0].call = Some(Position {
        inlined: 1,
        ..at(2, 4).unwrap()
    });
    assert!(
        validate(&cycle)
            .unwrap_err()
            .to_string()
            .contains("called from a later one")
    );
}
