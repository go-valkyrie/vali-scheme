//! Isolates as a host holds them (ADR 0008).

use vali_runtime::{HeapConfig, Isolate, NoCollection};

#[test]
fn a_host_enters_an_isolate_and_calls_into_it() {
    let mut isolate = Isolate::<NoCollection>::new(&HeapConfig::default());
    let (plus, args) = isolate.with(|cx| {
        cx.install_primitives().unwrap();
        let plus = cx.primitive("+").unwrap();
        (plus, cx.read("1 2 3").unwrap())
    });
    let sum = isolate.call(&plus, &args).unwrap();
    assert_eq!(isolate.with(|cx| cx.write_string(&sum)).unwrap(), "6");
}

#[test]
fn an_isolate_moves_between_threads_between_uses() {
    let mut isolate = Isolate::<NoCollection>::new(&HeapConfig::default());
    let pi = isolate.with(|cx| cx.flonum(3.25)).unwrap();
    let (mut isolate, pi) = std::thread::spawn(move || (isolate, pi)).join().unwrap();
    assert_eq!(isolate.with(|cx| pi.to_f64(cx)).unwrap(), 3.25);
}
