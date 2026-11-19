//! Run-format constants as long or as deep as memory allows: copying,
//! comparing, hashing and dropping them must not recurse on their nesting,
//! which would overflow a test thread's 2 MiB stack.

use std::hash::{DefaultHasher, Hash, Hasher};

use vali_bytecode::Constant;

/// Far beyond what a recursive pass could take on a 2 MiB stack.
const DEEP: i64 = 500_000;

fn hash_of(constant: &Constant) -> u64 {
    let mut hasher = DefaultHasher::new();
    constant.hash(&mut hasher);
    hasher.finish()
}

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
fn deep_constants_clone_compare_hash_and_drop() {
    for make in [long_list, nested_lists, nested_vectors] {
        let constant = make(DEEP);
        let copy = constant.clone();
        assert!(constant == copy);
        assert_eq!(hash_of(&constant), hash_of(&copy));
        drop(copy);
        assert!(constant == make(DEEP));
        assert!(constant != make(DEEP - 1));
        assert_ne!(hash_of(&constant), hash_of(&make(DEEP - 1)));
        drop(constant);
    }
}

#[test]
fn hashing_follows_equality_for_every_kind() {
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
    let copy = all.clone();
    assert!(copy == all);
    assert_eq!(hash_of(&copy), hash_of(&all));
    assert!(Constant::Integer(1) != Constant::Flonum(1.0));
    assert!(Constant::Flonum(f64::NAN) != Constant::Flonum(f64::NAN));
}

#[test]
fn deep_constants_format_with_debug() {
    // Small ones print as a derive would.
    let small = Constant::Pair(
        Box::new(Constant::Integer(1)),
        Box::new(Constant::EmptyList),
    );
    assert_eq!(format!("{small:?}"), "Pair(Integer(1), EmptyList)");
    // Deep ones stop at a bounded depth.
    for make in [long_list, nested_lists, nested_vectors] {
        let constant = make(DEEP);
        for text in [format!("{constant:?}"), format!("{constant:#?}")] {
            assert!(text.contains("..."));
            assert!(text.len() < 10_000_000, "{}", text.len());
        }
    }
}
