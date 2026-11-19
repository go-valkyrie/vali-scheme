//! Constants as long or as deep as memory allows: copying, comparing and
//! dropping them must not recurse on their nesting, which would overflow a
//! test thread's 2 MiB stack.

use vali_dist::Constant;

/// Far beyond what a recursive pass could take on a 2 MiB stack.
const DEEP: i64 = 500_000;

/// `(1 2 3 ... n)`, nested through the `cdr`s.
fn long_list(n: i64) -> Constant {
    let mut list = Constant::EmptyList;
    for i in (1..=n).rev() {
        list = Constant::Pair(Box::new(Constant::Integer(i)), Box::new(list));
    }
    list
}

/// `((((...))))`, nested through the `car`s.
fn nested_lists(n: i64) -> Constant {
    let mut list = Constant::EmptyList;
    for _ in 0..n {
        list = Constant::Pair(Box::new(list), Box::new(Constant::EmptyList));
    }
    list
}

/// `#(#(#(...)))`.
fn nested_vectors(n: i64) -> Constant {
    let mut vector = Constant::Vector(Vec::new());
    for _ in 0..n {
        vector = Constant::Vector(vec![vector]);
    }
    vector
}

#[test]
fn deep_constants_clone_compare_and_drop() {
    for make in [long_list, nested_lists, nested_vectors] {
        let constant = make(DEEP);
        let copy = constant.clone();
        assert!(constant == copy);
        drop(copy);
        assert!(constant == make(DEEP));
        assert!(constant != make(DEEP - 1));
        drop(constant);
    }
}

#[test]
fn comparison_tells_constants_apart() {
    // The same list but for its last element.
    let mut other = Constant::Pair(
        Box::new(Constant::Integer(-1)),
        Box::new(Constant::EmptyList),
    );
    for i in (1..DEEP).rev() {
        other = Constant::Pair(Box::new(Constant::Integer(i)), Box::new(other));
    }
    assert!(long_list(DEEP) != other);
    assert!(Constant::Flonum(f64::NAN) != Constant::Flonum(f64::NAN));
    assert!(Constant::Integer(1) != Constant::Flonum(1.0));
    assert!(Constant::Vector(vec![Constant::Integer(1)]) != Constant::Vector(Vec::new()));
}

#[test]
fn cloning_copies_every_kind() {
    let all = Constant::Vector(vec![
        Constant::Integer(-3),
        Constant::Bignum {
            negative: true,
            limbs: vec![1, 2],
        },
        Constant::Ratio(
            Box::new(Constant::Integer(1)),
            Box::new(Constant::Integer(3)),
        ),
        Constant::Flonum(2.5),
        Constant::Complex(
            Box::new(Constant::Flonum(1.0)),
            Box::new(Constant::Flonum(2.0)),
        ),
        Constant::Boolean(true),
        Constant::Character('x'),
        Constant::EmptyList,
        Constant::Unspecified,
        Constant::String("s".into()),
        Constant::Symbol("sym".into()),
        Constant::Keyword("key".into()),
        Constant::Pair(
            Box::new(Constant::Integer(1)),
            Box::new(Constant::Integer(2)),
        ),
        Constant::Bytevector(vec![1, 2, 3]),
        Constant::Vector(vec![Constant::EmptyList]),
    ]);
    assert!(all.clone() == all);
}
