//! Running transformers written in Scheme: the expander's phase-1
//! instance, and the syntax procedures transformers call.
//!
//! The expander compiles each procedural transformer into a module of its
//! own and runs it in an isolate it owns, created when the first such
//! transformer appears. While a transformer runs, the expander's scope
//! context, binding table and quoted syntax move into that isolate's host
//! data, where the procedures below find them, and move back after.

use std::any::Any;

use vali_bytecode::Arity;
use vali_runtime::{Cx, Error, HeapConfig, NoCollection, Value};
use vali_syntax::{
    BindingTable, Phase, ScopeContext, ScopeSetId, SourceId, Span, Syntax, bound_identifier_eq,
    free_identifier_eq,
};

use crate::expand::Binding;

/// What the syntax procedures need from the expander while a transformer
/// runs.
pub(crate) struct Shared {
    pub(crate) context: ScopeContext,
    pub(crate) table: BindingTable<String, Binding>,
    /// The syntax objects `quote-syntax` forms quote, by number.
    pub(crate) quoted: Vec<Syntax>,
}

/// The name `quote-syntax` compiles a call to, with the quoted syntax
/// object's number.
pub(crate) const QUOTE_SYNTAX: &str = "%quote-syntax";

/// A new phase-1 isolate, with the standard procedures and the syntax
/// procedures.
pub(crate) fn instance() -> Result<Cx<NoCollection>, Error> {
    let mut cx = Cx::new(&HeapConfig::default());
    cx.install_primitives()?;
    let natives: [(&str, u8, vali_runtime::NativeFn<NoCollection>); 6] = [
        ("syntax->datum", 1, syntax_to_datum),
        ("datum->syntax", 2, datum_to_syntax),
        ("identifier?", 1, is_identifier),
        ("bound-identifier=?", 2, bound_identifier),
        ("free-identifier=?", 2, free_identifier),
        (QUOTE_SYNTAX, 1, quote_syntax),
    ];
    for (name, arity, f) in natives {
        cx.define_native(name, Arity::exactly(arity), f)?;
    }
    Ok(cx)
}

fn nowhere() -> Span {
    Span {
        source: SourceId(0),
        range: Default::default(),
    }
}

/// Runs `f` with the expander's state, which is taken out of the host data
/// while `f` uses the isolate and put back after.
fn with_shared<T>(
    cx: &mut Cx<NoCollection>,
    name: &str,
    f: impl FnOnce(&mut Cx<NoCollection>, &mut Shared) -> Result<T, Error>,
) -> Result<T, Error> {
    let Some(data) = cx.set_host_data(None) else {
        return Err(Error::scheme(format!(
            "{name}: only available while a macro is being expanded"
        )));
    };
    if !(&*data as &dyn Any).is::<Shared>() {
        cx.set_host_data(Some(data));
        return Err(Error::scheme(format!(
            "{name}: only available while a macro is being expanded"
        )));
    }
    let mut shared = (data as Box<dyn Any>)
        .downcast::<Shared>()
        .expect("checked above");
    let result = f(cx, &mut shared);
    cx.set_host_data(Some(shared));
    result
}

/// `v` as a syntax object, or an error naming `name`.
fn syntax(cx: &Cx<NoCollection>, v: Value, name: &str) -> Result<Syntax, Error> {
    // SAFETY: arguments are valid values.
    unsafe { cx.value_syntax(v, ScopeSetId::EMPTY, nowhere()) }
        .ok_or_else(|| Error::scheme(format!("{name}: expected syntax")))
}

/// `v` as an identifier, or an error naming `name`.
fn identifier(cx: &Cx<NoCollection>, v: Value, name: &str) -> Result<Syntax, Error> {
    // SAFETY: arguments are valid values.
    match unsafe { cx.syntax_value(v) } {
        Some(s) if s.as_identifier().is_some() => Ok(s.clone()),
        _ => Err(Error::scheme(format!("{name}: expected an identifier"))),
    }
}

fn syntax_to_datum(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    let datum = syntax(cx, args[0], "syntax->datum")?.to_datum();
    cx.datum_raw(&datum)
}

/// `(datum->syntax context datum)`: `datum` with the scopes, span and
/// origin of `context`. Syntax objects inside `datum` stay as they are.
fn datum_to_syntax(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: arguments are valid values.
    let Some(context) = unsafe { cx.syntax_value(args[0]) }.cloned() else {
        return Err(Error::scheme(
            "datum->syntax: expected syntax as the context",
        ));
    };
    // SAFETY: as above.
    let made = unsafe { cx.value_syntax(args[1], context.scopes(), context.span()) }
        .ok_or_else(|| Error::scheme("datum->syntax: expected a datum"))?
        .with_origin(context.origin().cloned());
    Ok(cx.syntax_raw(made)?)
}

fn is_identifier(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: arguments are valid values.
    let syntax = unsafe { cx.syntax_value(args[0]) };
    Ok(Value::boolean(
        syntax.is_some_and(|s| s.as_identifier().is_some()),
    ))
}

fn bound_identifier(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    let name = "bound-identifier=?";
    let (a, b) = (
        identifier(cx, args[0], name)?,
        identifier(cx, args[1], name)?,
    );
    Ok(Value::boolean(bound_identifier_eq(&a, &b)))
}

fn free_identifier(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    let name = "free-identifier=?";
    let (a, b) = (
        identifier(cx, args[0], name)?,
        identifier(cx, args[1], name)?,
    );
    with_shared(cx, name, |_, shared| {
        Ok(Value::boolean(free_identifier_eq(
            &shared.context,
            &shared.table,
            Phase(0),
            &a,
            &b,
        )))
    })
}

fn quote_syntax(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    let index = args[0]
        .as_fixnum()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error::scheme("quote-syntax: expected a number"))?;
    let quoted = with_shared(cx, "quote-syntax", |_, shared| {
        shared
            .quoted
            .get(index)
            .cloned()
            .ok_or_else(|| Error::scheme("quote-syntax: no such syntax"))
    })?;
    Ok(cx.syntax_raw(quoted)?)
}
