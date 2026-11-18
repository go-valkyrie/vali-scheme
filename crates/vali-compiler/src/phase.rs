//! Running transformers written in Scheme: the expander's phase-1
//! instance, and the syntax procedures transformers call.
//!
//! The expander compiles each procedural transformer into a module of its
//! own and runs it in an isolate it owns, created when the first such
//! transformer appears. While a transformer runs, the expander's scope
//! context, binding table and compiled patterns and templates move into
//! that isolate's host data, where the procedures below find them, and
//! move back after.
//!
//! `syntax-case` and `syntax` compile to calls to `%syntax-match` and
//! `%syntax-build` with a pattern's or template's number. What a pattern
//! variable matched is a syntax object at depth zero, and a list of what
//! it matched one level down at each greater depth.

use std::any::Any;
use std::sync::Arc;

use vali_bytecode::Arity;
use vali_runtime::{Cx, DeferredHeadered, Error, HeapConfig, Strategy, Value};
use vali_syntax::{
    BindingTable, Origin, Phase, ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax,
    bound_identifier_eq, free_identifier_eq,
};

use crate::expand::Binding;
use crate::pattern::{self, Builder, Match, Pattern, Template};

/// What phase-1 code compiled to, which its procedures look up by number.
#[derive(Clone, Default)]
pub(crate) struct Compiled {
    /// The syntax objects `quote-syntax` forms quote.
    pub(crate) quoted: Vec<Syntax>,
    /// `syntax-case` patterns, with how many variables each binds.
    pub(crate) patterns: Vec<(Pattern, usize)>,
    /// `syntax` templates, with the depth of each variable they use.
    pub(crate) templates: Vec<(Template, Vec<usize>)>,
}

/// What the syntax procedures need from the expander while a transformer
/// runs.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) context: ScopeContext,
    pub(crate) table: BindingTable<String, Binding>,
    /// Each unit's compiled syntax, by the unit's identity.
    pub(crate) compiled: CompiledByUnit,
    /// The macro use being expanded, which built syntax gets as its origin.
    pub(crate) origin: Option<Arc<Origin>>,
    /// The sources read, which locate syntax objects' spans.
    pub(crate) sources: vali_syntax::Sources,
}

/// The syntax procedures transformers call, which only the phase-1
/// isolate has.
pub(crate) const SYNTAX_PROCEDURES: &[&str] = &[
    "syntax->datum",
    "datum->syntax",
    "identifier?",
    "bound-identifier=?",
    "free-identifier=?",
    "generate-temporaries",
    "syntax-violation",
    "syntax-source",
    "syntax-line",
    "syntax-column",
    "syntax-position",
    "syntax-span",
];

/// The procedures phase-1 code compiles calls to.
pub(crate) const QUOTE_SYNTAX: &str = "%quote-syntax";
pub(crate) const SYNTAX_MATCH: &str = "%syntax-match";
pub(crate) const SYNTAX_BUILD: &str = "%syntax-build";

/// A new phase-1 isolate, with the standard procedures and the syntax
/// procedures.
/// An isolate for expansion-time code. It counts references, deferred, so
/// a macro that allocates heavily frees as it goes.
pub(crate) fn instance() -> Result<Cx<DeferredHeadered>, Error> {
    let mut cx = Cx::new(&HeapConfig::default());
    cx.set_expanding(true);
    cx.install_primitives()?;
    install(&mut cx)?;
    Ok(cx)
}

/// Binds the syntax procedures in `cx`. They work while the expander's
/// data is there: during a transformer's call at phase 1, or at run time
/// after a program that uses syntax objects is loaded.
pub(crate) fn install<S: Strategy>(cx: &mut Cx<S>) -> Result<(), Error> {
    let exactly = Arity::exactly;
    let at_least = |required| Arity {
        required,
        optional: 0,
        rest: true,
    };
    let natives: [(&str, Arity, vali_runtime::NativeFn<S>); 15] = [
        ("syntax-source", exactly(1), syntax_source),
        ("syntax-line", exactly(1), syntax_line),
        ("syntax-column", exactly(1), syntax_column),
        ("syntax-position", exactly(1), syntax_position),
        ("syntax-span", exactly(1), syntax_span),
        ("syntax->datum", exactly(1), syntax_to_datum),
        ("datum->syntax", exactly(2), datum_to_syntax),
        ("identifier?", exactly(1), is_identifier),
        ("bound-identifier=?", exactly(2), bound_identifier),
        ("free-identifier=?", exactly(2), free_identifier),
        ("generate-temporaries", exactly(1), generate_temporaries),
        ("syntax-violation", at_least(3), syntax_violation),
        (QUOTE_SYNTAX, exactly(2), quote_syntax),
        (SYNTAX_MATCH, exactly(3), syntax_match),
        (SYNTAX_BUILD, at_least(2), syntax_build),
    ];
    for (name, arity, f) in natives {
        cx.define_primitive(name, arity, f)?;
    }
    crate::eval::install(cx)
}

fn nowhere() -> Span {
    Span {
        source: SourceId(0),
        range: Default::default(),
    }
}

/// Runs `f` with the expander's state, which is taken out of the host data
/// while `f` uses the isolate and put back after.
fn with_shared<T, S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    f: impl FnOnce(&mut Cx<S>, &mut Shared) -> Result<T, Error>,
) -> Result<T, Error> {
    let Some(data) = cx.set_expander_data(None) else {
        return Err(Error::scheme(format!(
            "{name}: only available while a macro is being expanded"
        )));
    };
    if !(&*data as &dyn Any).is::<Shared>() {
        cx.set_expander_data(Some(data));
        return Err(Error::scheme(format!(
            "{name}: only available while a macro is being expanded"
        )));
    }
    let mut shared = (data as Box<dyn Any>)
        .downcast::<Shared>()
        .expect("checked above");
    let result = f(cx, &mut shared);
    cx.set_expander_data(Some(shared));
    result
}

/// `v` as a syntax object, or an error naming `name`.
fn syntax<S: Strategy>(cx: &Cx<S>, v: Value, name: &str) -> Result<Syntax, Error> {
    // SAFETY: arguments are valid values.
    unsafe { cx.value_syntax(v, ScopeSetId::EMPTY, nowhere()) }
        .ok_or_else(|| Error::scheme(format!("{name}: expected syntax")))
}

/// `v` as an identifier, or an error naming `name`.
fn identifier<S: Strategy>(cx: &Cx<S>, v: Value, name: &str) -> Result<Syntax, Error> {
    // SAFETY: arguments are valid values.
    match unsafe { cx.syntax_value(v) } {
        Some(s) if s.as_identifier().is_some() => Ok(s.clone()),
        _ => Err(Error::scheme(format!("{name}: expected an identifier"))),
    }
}

/// Where the syntax object `v` is, if its source is known, for the
/// procedure `name`, which needs syntax (ADR 0014 #6).
fn location<S: Strategy>(
    cx: &mut Cx<S>,
    v: Value,
    name: &str,
) -> Result<Option<vali_syntax::Location>, Error> {
    // SAFETY: arguments are valid values.
    let Some(span) = unsafe { cx.syntax_value(v) }.map(Syntax::span) else {
        return Err(Error::scheme(format!("{name}: expected syntax")));
    };
    with_shared(cx, name, |_, shared| Ok(shared.sources.locate(span)))
}

/// One of a location's numbers, or `#f` with no location.
fn location_number<S: Strategy>(
    cx: &mut Cx<S>,
    v: Value,
    name: &str,
    field: fn(&vali_syntax::Location) -> u32,
) -> Result<Value, Error> {
    Ok(location(cx, v, name)?.map_or(Value::FALSE, |l| {
        Value::fixnum(i64::from(field(&l))).expect("fits")
    }))
}

/// `(syntax-source stx)`: the identity of the source `stx` was read from,
/// as a string, or `#f`.
fn syntax_source<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match location(cx, args[0], "syntax-source")? {
        Some(l) => {
            let source = cx.string(&l.source)?;
            cx.get(&source)
        }
        None => Ok(Value::FALSE),
    }
}

/// `(syntax-line stx)`: its line, from 1, or `#f`.
fn syntax_line<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    location_number(cx, args[0], "syntax-line", |l| l.line)
}

/// `(syntax-column stx)`: its column, from 0, or `#f`.
fn syntax_column<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    location_number(cx, args[0], "syntax-column", |l| l.column)
}

/// `(syntax-position stx)`: its position in its source, from 1, or `#f`.
fn syntax_position<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    location_number(cx, args[0], "syntax-position", |l| l.position)
}

/// `(syntax-span stx)`: how many characters it covers, or `#f`.
fn syntax_span<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    location_number(cx, args[0], "syntax-span", |l| l.span)
}

fn syntax_to_datum<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let datum = syntax(cx, args[0], "syntax->datum")?.to_datum();
    cx.datum_raw(&datum)
}

/// `(datum->syntax context datum)`: `datum` with the scopes, span and
/// origin of `context`. Syntax objects inside `datum` stay as they are.
fn datum_to_syntax<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
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

fn is_identifier<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: arguments are valid values.
    let syntax = unsafe { cx.syntax_value(args[0]) };
    Ok(Value::boolean(
        syntax.is_some_and(|s| s.as_identifier().is_some()),
    ))
}

fn bound_identifier<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "bound-identifier=?";
    let (a, b) = (
        identifier(cx, args[0], name)?,
        identifier(cx, args[1], name)?,
    );
    Ok(Value::boolean(bound_identifier_eq(&a, &b)))
}

fn free_identifier<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
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

/// `(%quote-syntax unit k)`: the syntax object the unit named `unit`
/// quotes as its `k`th.
fn quote_syntax<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "quote-syntax";
    let unit = unit_name(cx, args[0], name)?;
    let index = number(args[1], name)?;
    let quoted = with_shared(cx, name, |_, shared| {
        unit_table(shared, &unit, name)?
            .quoted
            .get(index)
            .cloned()
            .ok_or_else(|| Error::scheme("quote-syntax: no such syntax"))
    })?;
    Ok(cx.syntax_raw(quoted)?)
}

/// Each unit's compiled syntax, by the unit's identity. Code names its own
/// unit with each number, so numbers start from zero in every unit.
pub(crate) type CompiledByUnit = std::collections::HashMap<String, Compiled>;

/// The name of the unit whose compiled syntax a call refers to.
fn unit_name<S: Strategy>(cx: &Cx<S>, v: Value, name: &str) -> Result<String, Error> {
    if v.tag() != vali_abi::Tag::String {
        return Err(Error::scheme(format!("{name}: expected a unit's name")));
    }
    // SAFETY: a live string.
    Ok(unsafe { cx.string_str(v) }.to_owned())
}

/// The compiled syntax of the unit named `unit`.
fn unit_table<'a>(shared: &'a Shared, unit: &str, name: &str) -> Result<&'a Compiled, Error> {
    shared
        .compiled
        .get(unit)
        .ok_or_else(|| Error::scheme(format!("{name}: {unit} compiled no syntax")))
}

/// A pattern's or template's number.
fn number(v: Value, name: &str) -> Result<usize, Error> {
    v.as_fixnum()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error::scheme(format!("{name}: expected a number")))
}

/// What a pattern variable matched, as a value.
fn match_value<S: Strategy>(cx: &mut Cx<S>, m: Match) -> Result<Value, Error> {
    match m {
        Match::One(syntax) => Ok(cx.syntax_raw(syntax)?),
        Match::Many(matches) => {
            let values = matches
                .into_iter()
                .map(|m| match_value(cx, m))
                .collect::<Result<Vec<_>, _>>()?;
            let mut list = Value::EMPTY_LIST;
            for v in values.into_iter().rev() {
                // SAFETY: nothing is collected, so the values stay valid.
                list = unsafe { cx.cons_raw(v, list) }?;
            }
            Ok(list)
        }
    }
}

/// A value from [`match_value`] back as a match at `depth`.
fn value_match<S: Strategy>(cx: &Cx<S>, v: Value, depth: usize) -> Option<Match> {
    if depth == 0 {
        // SAFETY: arguments and their parts are valid values.
        return unsafe { cx.syntax_value(v) }.cloned().map(Match::One);
    }
    let mut matches = Vec::new();
    let mut rest = v;
    while rest.is_pair() {
        // SAFETY: as above.
        let (car, cdr) = unsafe { (cx.car(rest), cx.cdr(rest)) };
        matches.push(value_match(cx, car, depth - 1)?);
        rest = cdr;
    }
    (rest == Value::EMPTY_LIST).then_some(Match::Many(matches))
}

/// `(%syntax-match input unit k)`: what the `k`th pattern of the unit
/// named `unit` matched, as a vector, or `#f` if `input` doesn't match.
fn syntax_match<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "syntax-case";
    let input = syntax(cx, args[0], name)?;
    let unit = unit_name(cx, args[1], name)?;
    let k = number(args[2], name)?;
    let binds = with_shared(cx, name, |_, shared| {
        // The table and the scope context are borrowed apart.
        let compiled = shared
            .compiled
            .get(&unit)
            .ok_or_else(|| Error::scheme(format!("{name}: {unit} compiled no syntax")))?;
        let Some((pattern, vars)) = compiled.patterns.get(k) else {
            return Err(Error::scheme(format!("{name}: no such pattern")));
        };
        let mut binds = vec![None; *vars];
        let table = &shared.table;
        let same =
            |c: &ScopeContext, a: &Syntax, b: &Syntax| free_identifier_eq(c, table, Phase(0), a, b);
        let matched = pattern::matches(&mut shared.context, &same, pattern, &input, &mut binds)
            .map_err(|e| Error::scheme(format!("{name}: {}", e.message)))?;
        Ok(matched.then_some(binds))
    })?;
    let Some(binds) = binds else {
        return Ok(Value::FALSE);
    };
    let values = binds
        .into_iter()
        .map(|m| match_value(cx, m.expect("a matching pattern binds every variable")))
        .collect::<Result<Vec<_>, _>>()?;
    // SAFETY: the fill is valid.
    let vector = unsafe { cx.make_vector_raw(values.len(), Value::FALSE) }?;
    for (i, v) in values.into_iter().enumerate() {
        // SAFETY: the vector has that many elements.
        unsafe { cx.vector_set(vector, i, v) };
    }
    Ok(vector)
}

/// `(%syntax-build unit k v ...)`: the `k`th template of the unit named
/// `unit`, with the matches of the variables it uses.
fn syntax_build<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "syntax";
    let unit = unit_name(cx, args[0], name)?;
    let k = number(args[1], name)?;
    let built = with_shared(cx, name, |cx, shared| {
        let Some((template, depths)) = unit_table(shared, &unit, name)?.templates.get(k) else {
            return Err(Error::scheme(format!("{name}: no such template")));
        };
        let mut binds = args[2..]
            .iter()
            .zip(depths)
            .map(|(&v, &depth)| {
                value_match(cx, v, depth).ok_or_else(|| {
                    Error::scheme(format!("{name}: a pattern variable's match is corrupt"))
                })
            })
            .map(|m| m.map(Some))
            .collect::<Result<Vec<_>, _>>()?;
        let builder = Builder {
            depths: depths.clone(),
            origin: shared.origin.as_ref(),
            span: nowhere(),
        };
        builder
            .build(template, &mut binds)
            .map_err(|e| Error::scheme(format!("{name}: {}", e.message)))
    })?;
    Ok(cx.syntax_raw(built)?)
}

/// `(generate-temporaries list)`: as many fresh identifiers as `list` has
/// elements, distinct from every other identifier.
fn generate_temporaries<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = "generate-temporaries";
    let list = syntax(cx, args[0], name)?;
    let ids = with_shared(cx, name, |_, shared| {
        let items = list
            .list_items(&mut shared.context)
            .map_err(|e| Error::scheme(format!("{name}: {e}")))?
            .ok_or_else(|| Error::scheme(format!("{name}: expected a list")))?;
        let fresh = |e: vali_syntax::TooManyScopes| Error::scheme(format!("{name}: {e}"));
        let scope = shared.context.new_scope(ScopeKind::Macro).map_err(fresh)?;
        let scopes = shared.context.intern(&[scope]).map_err(fresh)?;
        Ok((0..items.len())
            .map(|i| Syntax::identifier(&format!("t{i}"), scopes, nowhere(), None))
            .collect::<Vec<_>>())
    })?;
    let mut list = Value::EMPTY_LIST;
    for id in ids.into_iter().rev() {
        let id = cx.syntax_raw(id)?;
        // SAFETY: nothing is collected, so the values stay valid.
        list = unsafe { cx.cons_raw(id, list) }?;
    }
    Ok(list)
}

/// `(syntax-violation who message form [subform])`: raises an error.
fn syntax_violation<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let describe = |v: Value| {
        // SAFETY: arguments are valid values.
        unsafe { cx.value_syntax(v, ScopeSetId::EMPTY, nowhere()) }
            .map(|s| s.to_datum().to_string())
    };
    // SAFETY: as above.
    let text = |v: Value| unsafe {
        if v.tag() == vali_abi::Tag::String {
            Some(cx.string_str(v).to_owned())
        } else if v.tag() == vali_abi::Tag::Symbol {
            Some(cx.symbol_name(v).to_owned())
        } else {
            None
        }
    };
    let message = text(args[1]).unwrap_or_else(|| "invalid syntax".to_owned());
    let mut out = match text(args[0]) {
        Some(who) => format!("{who}: {message}"),
        None => message,
    };
    if let Some(form) = describe(args[2]) {
        out.push_str(&format!(": {form}"));
    }
    if let Some(sub) = args.get(3).and_then(|&v| describe(v)) {
        out.push_str(&format!(", in {sub}"));
    }
    Err(Error::scheme(out))
}
