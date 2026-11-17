//! Reading text into the heap, and writing it back.

use vali_runtime::{Cx, Error, HeapConfig, NoCollection};

fn round_trip(source: &str) -> Vec<String> {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let values = cx.read(source).unwrap();
    values.iter().map(|v| cx.write_string(v).unwrap()).collect()
}

#[test]
fn reading_then_writing_round_trips() {
    for text in [
        "42",
        "-7",
        "123456789012345678901234567890",
        "2.5",
        "#t",
        "#\\space",
        "\"a\\\"b\\n\"",
        "hello",
        "|odd symbol|",
        "(1 (2 3) . 4)",
        "#(a #(b) \"c\")",
        "#u8(1 2 255)",
        "()",
    ] {
        assert_eq!(round_trip(text), [text], "{text}");
    }
}

#[test]
fn abbreviations_and_several_datums() {
    assert_eq!(
        round_trip("'a `(b ,c) d"),
        ["(quote a)", "(quasiquote (b (unquote c)))", "d"]
    );
}

#[test]
fn labels_build_shared_and_circular_structure() {
    assert_eq!(round_trip("#0=(a b . #0#)"), ["#0=(a b . #0#)"]);
    assert_eq!(round_trip("#0=#(1 #0#)"), ["#0=#(1 #0#)"]);
    // Shared, not circular: written in full.
    assert_eq!(round_trip("(#0=(x) #0#)"), ["((x) (x))"]);
}

#[test]
fn symbols_are_interned() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let values = cx.read("abc abc").unwrap();
    assert_eq!(cx.get(&values[0]), cx.get(&values[1]));
}

#[test]
fn errors() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let Error::Scheme(e) = cx.read("(1 2").unwrap_err() else {
        panic!("expected a read error");
    };
    assert!(e.message.starts_with("read: "), "{}", e.message);
}

#[test]
fn every_kind_of_number() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    let values = cx.read("1/2 -6/4 1+2i 1.5-0.5i +i #e1.25").unwrap();
    let written: Vec<String> = values.iter().map(|v| cx.write_string(v).unwrap()).collect();
    assert_eq!(written, ["1/2", "-3/2", "1+2i", "1.5-0.5i", "0+1i", "5/4"]);
}
