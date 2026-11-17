//! Syntax objects in the heap, datum conversions, and host data.

use vali_runtime::{Cx, HeapConfig, NoCollection, Value};
use vali_syntax::{ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax};

fn cx() -> Cx<NoCollection> {
    Cx::new(&HeapConfig::default())
}

fn datum(text: &str) -> vali_syntax::Datum {
    let mut read = vali_syntax::read(text, SourceId(0));
    assert!(read.errors.is_empty());
    read.datums.remove(0)
}

fn span() -> Span {
    Span {
        source: SourceId(0),
        range: Default::default(),
    }
}

#[test]
fn syntax_objects_live_in_the_heap() {
    let mut cx = cx();
    let mut scopes = ScopeContext::new();
    let scope = scopes.new_scope(ScopeKind::Other).unwrap();
    let set = scopes.intern(&[scope]).unwrap();
    let syntax = Syntax::from_datum(&datum("(a b)"), set);
    let handle = cx.syntax(syntax).unwrap();
    // SAFETY: the handle keeps the value valid.
    let back = unsafe { cx.syntax_value(cx.get(&handle).unwrap()) }.unwrap();
    assert_eq!(back.scopes(), set);
    assert_eq!(back.to_datum().to_string(), "(a b)");
    // Printed without scopes.
    assert_eq!(cx.write_string(&handle).unwrap(), "#<syntax (a b)>");
    // Other values are not syntax objects.
    let pair = cx.read("(1)").unwrap().remove(0);
    // SAFETY: as above.
    assert!(unsafe { cx.syntax_value(cx.get(&pair).unwrap()) }.is_none());
    assert!(unsafe { cx.syntax_value(Value::FALSE) }.is_none());
}

#[test]
fn datums_convert_both_ways() {
    let mut cx = cx();
    for text in [
        "(1 \"two\" #\\3 (4.5 . sym) #(#t #f) #u8(1 2) 123456789012345678901234567890)",
        "()",
        "x",
    ] {
        let d = datum(text);
        let v = cx.datum_raw(&d).unwrap();
        // SAFETY: nothing was allocated since.
        let back = unsafe { cx.value_datum(v, span()) }.unwrap();
        assert_eq!(back.to_string(), d.to_string(), "{text}");
    }
    // Procedures and cycles are not datums.
    cx.install_primitives().unwrap();
    let car = cx.global("car").unwrap();
    // SAFETY: the handle keeps it valid.
    assert!(unsafe { cx.value_datum(cx.get(&car).unwrap(), span()) }.is_none());
    let cycle = cx.read("#0=(1 . #0#)").unwrap().remove(0);
    // SAFETY: the handle keeps it valid.
    assert!(unsafe { cx.value_datum(cx.get(&cycle).unwrap(), span()) }.is_none());
    // A list sharing structure without a cycle is fine.
    let shared = cx.read("(#0=(1) #0#)").unwrap().remove(0);
    // SAFETY: as above.
    let shared = unsafe { cx.value_datum(cx.get(&shared).unwrap(), span()) }.unwrap();
    assert_eq!(shared.to_string(), "((1) (1))");
}

#[test]
fn partly_wrapped_values_become_syntax() {
    let mut cx = cx();
    let mut scopes = ScopeContext::new();
    let scope = scopes.new_scope(ScopeKind::Other).unwrap();
    let set = scopes.intern(&[scope]).unwrap();
    // A list holding a syntax object and plain data, as a transformer
    // returning (list #'if x 1) would make.
    let id = cx
        .syntax(Syntax::identifier("if", set, span(), None))
        .unwrap();
    let rest = cx.read("(x 1)").unwrap().remove(0);
    let list = cx.cons(&id, &rest).unwrap();
    // SAFETY: the handle keeps it valid.
    let syntax =
        unsafe { cx.value_syntax(cx.get(&list).unwrap(), ScopeSetId::EMPTY, span()) }.unwrap();
    assert_eq!(syntax.to_datum().to_string(), "(if x 1)");
    let items = syntax.list_items(&mut scopes).unwrap().unwrap();
    assert_eq!(items[0].scopes(), set);
    assert_eq!(items[1].scopes(), ScopeSetId::EMPTY);
}

#[test]
fn host_data_is_kept_by_type() {
    let mut cx = cx();
    assert!(cx.host_data::<u32>().is_none());
    assert!(cx.set_host_data(Some(Box::new(7_u32))).is_none());
    *cx.host_data::<u32>().unwrap() += 1;
    assert!(cx.host_data::<String>().is_none());
    let old = cx.set_host_data(None).unwrap();
    assert_eq!(*old.downcast::<u32>().unwrap(), 8);
}
