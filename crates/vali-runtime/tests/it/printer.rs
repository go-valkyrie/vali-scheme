//! Printing values with `write` and `display`.

use vali_runtime::{Cx, Handle, HeapConfig, NoCollection};

fn cx() -> Cx<NoCollection> {
    Cx::new(&HeapConfig::default())
}

fn list(cx: &mut Cx<NoCollection>, items: &[Handle]) -> Handle {
    let mut list = Handle::empty_list();
    for item in items.iter().rev() {
        list = cx.cons(item, &list).unwrap();
    }
    list
}

#[test]
fn atoms() {
    let mut cx = cx();
    let cases: Vec<(Handle, &str, &str)> = vec![
        (Handle::fixnum(-42).unwrap(), "-42", "-42"),
        (Handle::boolean(true), "#t", "#t"),
        (Handle::character('a'), "#\\a", "a"),
        (Handle::character(' '), "#\\space", " "),
        (Handle::character('\u{1}'), "#\\x1", "\u{1}"),
        (Handle::empty_list(), "()", "()"),
        (cx.flonum(2.5).unwrap(), "2.5", "2.5"),
        (cx.flonum(1e21).unwrap(), "1e21", "1e21"),
        (
            cx.string("a\"b\\c\n").unwrap(),
            "\"a\\\"b\\\\c\\n\"",
            "a\"b\\c\n",
        ),
        (cx.symbol("hello").unwrap(), "hello", "hello"),
        (
            cx.symbol("hello world").unwrap(),
            "|hello world|",
            "hello world",
        ),
    ];
    for (value, written, displayed) in cases {
        assert_eq!(cx.write_string(&value).unwrap(), written);
        assert_eq!(cx.display_string(&value).unwrap(), displayed);
    }
}

#[test]
fn lists_and_vectors() {
    let mut cx = cx();
    let s = cx.string("s").unwrap();
    let inner = list(&mut cx, &[Handle::character('c'), s]);
    let items = [Handle::fixnum(1).unwrap(), inner, Handle::boolean(false)];
    let xs = list(&mut cx, &items);
    assert_eq!(cx.write_string(&xs).unwrap(), "(1 (#\\c \"s\") #f)");
    assert_eq!(cx.display_string(&xs).unwrap(), "(1 (c s) #f)");

    let dotted = cx
        .cons(&Handle::fixnum(1).unwrap(), &Handle::fixnum(2).unwrap())
        .unwrap();
    assert_eq!(cx.write_string(&dotted).unwrap(), "(1 . 2)");

    let v = cx.vector(3, &Handle::fixnum(0).unwrap()).unwrap();
    v.vector_set(&mut cx, 1, &dotted).unwrap();
    assert_eq!(cx.write_string(&v).unwrap(), "#(0 (1 . 2) 0)");
}

#[test]
fn shared_structure_prints_in_full_and_cycles_get_labels() {
    let mut cx = cx();
    let a = cx.symbol("a").unwrap();
    let shared = list(&mut cx, &[a]);
    let twice = list(&mut cx, &[shared.clone(), shared]);
    assert_eq!(cx.write_string(&twice).unwrap(), "((a) (a))");

    // (1 2 1 2 ...): the last pair's cdr points back to the first.
    let one = Handle::fixnum(1).unwrap();
    let two = Handle::fixnum(2).unwrap();
    let cycle = list(&mut cx, &[one, two]);
    let last = cycle.cdr(&mut cx).unwrap();
    last.set_cdr(&mut cx, &cycle).unwrap();
    assert_eq!(cx.write_string(&cycle).unwrap(), "#0=(1 2 . #0#)");

    // A vector that contains itself.
    let v = cx.vector(2, &Handle::fixnum(0).unwrap()).unwrap();
    v.vector_set(&mut cx, 1, &v).unwrap();
    assert_eq!(cx.write_string(&v).unwrap(), "#0=#(0 #0#)");
}

#[test]
fn long_lists_do_not_overflow_the_rust_stack() {
    let mut cx = cx();
    let mut xs = Handle::empty_list();
    for i in 0..200_000 {
        xs = cx.cons(&Handle::fixnum(i % 10).unwrap(), &xs).unwrap();
    }
    let text = cx.write_string(&xs).unwrap();
    assert_eq!(text.len(), 2 * 200_000 + 1);
}
