//! The scope machinery (ADR 0015), tested on the classic hygiene examples.
//!
//! A toy expander below handles `lambda`, `let`, `if`, `quote` and
//! pattern-template macros, using only the scope sets, the binding table and
//! the reference syntax type. It prints each variable as its unique binding,
//! so `(let ((x 1)) x)` expands to `(let ((x.1 1)) x.1)`.

use std::sync::Arc;

use vali_syntax::reference::{Shape, Syntax, apply_macro, bound_identifier_eq, free_identifier_eq};
use vali_syntax::{
    BindingTable, Origin, Phase, Phases, Resolution, ScopeContext, ScopeKind, ScopeSetId, SourceId,
    TooManyScopes,
};

#[derive(Clone, Debug, PartialEq)]
enum Binding {
    Core(&'static str),
    Variable(String),
    Macro(usize),
}

/// A macro `(_ param ...)` that expands to `template`, whose identifiers
/// carry `scopes`, the macro definition's. Identifiers named in `capture`
/// are given the use's scopes, as `datum->syntax` with the macro keyword
/// would.
struct Macro {
    params: Vec<&'static str>,
    template: Syntax,
    capture: Vec<&'static str>,
}

struct Expander {
    context: ScopeContext,
    table: BindingTable<String, Binding>,
    macros: Vec<Macro>,
    /// The scopes of the module body, where the core forms are bound.
    top: ScopeSetId,
    next: usize,
}

fn read(text: &str) -> vali_syntax::Datum {
    let mut read = vali_syntax::read(text, SourceId(0));
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    assert_eq!(read.datums.len(), 1);
    read.datums.remove(0)
}

impl Expander {
    fn new() -> Self {
        let mut context = ScopeContext::new();
        let module = context.new_scope(ScopeKind::Library).unwrap();
        let top = context.add(ScopeSetId::EMPTY, module).unwrap();
        let mut table = BindingTable::new();
        for core in ["lambda", "let", "if", "quote"] {
            table
                .bind(
                    &context,
                    core.to_owned(),
                    top,
                    Phases::All,
                    Binding::Core(core),
                )
                .unwrap();
        }
        Expander {
            context,
            table,
            macros: Vec::new(),
            top,
            next: 0,
        }
    }

    /// Defines a macro at module level.
    fn define_macro(
        &mut self,
        name: &str,
        params: &[&'static str],
        template: &str,
        capture: &[&'static str],
    ) {
        let template = Syntax::from_datum(&read(template), self.top);
        self.macros.push(Macro {
            params: params.to_vec(),
            template,
            capture: capture.to_vec(),
        });
        let index = self.macros.len() - 1;
        self.table
            .bind(
                &self.context,
                name.to_owned(),
                self.top,
                Phases::All,
                Binding::Macro(index),
            )
            .unwrap();
    }

    fn expand_text(&mut self, text: &str) -> Result<String, String> {
        let syntax = Syntax::from_datum(&read(text), self.top);
        self.expand(syntax)
    }

    fn resolve(&self, syntax: &Syntax) -> Result<&Binding, String> {
        let name = syntax.identifier().unwrap_or("?");
        match syntax.resolve(&self.context, &self.table, Phase::RUN) {
            Resolution::Bound(binding) => Ok(binding),
            Resolution::Unbound => Err(format!("{name}: unbound")),
            Resolution::Ambiguous(_) => Err(format!("{name}: ambiguous")),
        }
    }

    fn bind_variable(&mut self, id: &Syntax) -> String {
        self.next += 1;
        let unique = format!("{}.{}", id.identifier().expect("a binder"), self.next);
        self.table
            .bind(
                &self.context,
                id.identifier().unwrap().to_owned(),
                id.scopes,
                Phases::All,
                Binding::Variable(unique.clone()),
            )
            .unwrap();
        unique
    }

    fn expand(&mut self, syntax: Syntax) -> Result<String, String> {
        if syntax.identifier().is_some() {
            return match self.resolve(&syntax)? {
                Binding::Variable(unique) => Ok(unique.clone()),
                other => Err(format!("{other:?} used as a variable")),
            };
        }
        let Some(items) = syntax.list() else {
            return Ok(syntax.to_datum().to_string());
        };
        let head = items.first().filter(|h| h.identifier().is_some());
        let binding = head.map(|h| self.resolve(h)).transpose()?.cloned();
        match binding {
            Some(Binding::Core("quote")) => Ok(syntax.to_datum().to_string()),
            Some(Binding::Core("if")) => {
                let parts = self.expand_all(&items[1..])?;
                Ok(format!("(if {})", parts.join(" ")))
            }
            Some(Binding::Core("lambda")) => {
                let [_, params, body] = items else {
                    return Err("bad lambda".into());
                };
                let (mut params, mut body) = (params.clone(), body.clone());
                let scope = self.scope(ScopeKind::Binder)?;
                params.add(&mut self.context, scope).map_err(err)?;
                body.add(&mut self.context, scope).map_err(err)?;
                let names: Vec<String> = params
                    .list()
                    .ok_or("bad lambda")?
                    .iter()
                    .map(|p| self.bind_variable(p))
                    .collect();
                Ok(format!(
                    "(lambda ({}) {})",
                    names.join(" "),
                    self.expand(body)?
                ))
            }
            Some(Binding::Core("let")) => {
                let [_, clauses, body] = items else {
                    return Err("bad let".into());
                };
                let scope = self.scope(ScopeKind::Binder)?;
                let mut body = body.clone();
                body.add(&mut self.context, scope).map_err(err)?;
                let mut out = Vec::new();
                for clause in clauses.list().ok_or("bad let")? {
                    let [id, init] = clause.list().ok_or("bad let")? else {
                        return Err("bad let".into());
                    };
                    let init = self.expand(init.clone())?;
                    let mut id = id.clone();
                    id.add(&mut self.context, scope).map_err(err)?;
                    out.push(format!("({} {init})", self.bind_variable(&id)));
                }
                Ok(format!("(let ({}) {})", out.join(" "), self.expand(body)?))
            }
            Some(Binding::Macro(index)) => {
                let output = self.apply(index, syntax)?;
                self.expand(output)
            }
            _ => Ok(format!("({})", self.expand_all(items)?.join(" "))),
        }
    }

    fn expand_all(&mut self, items: &[Syntax]) -> Result<Vec<String>, String> {
        items.iter().map(|i| self.expand(i.clone())).collect()
    }

    fn scope(&mut self, kind: ScopeKind) -> Result<vali_syntax::Scope, String> {
        self.context.new_scope(kind).map_err(err)
    }

    fn apply(&mut self, index: usize, input: Syntax) -> Result<Syntax, String> {
        let origin = Arc::new(Origin {
            macro_name: input.list().unwrap()[0].identifier().unwrap().into(),
            use_site: input.span,
            parent: input.origin.clone(),
        });
        let mac = &self.macros[index];
        let (params, template, capture) = (&mac.params, mac.template.clone(), &mac.capture);
        apply_macro(&mut self.context, input, Some(origin), |_, input| {
            let items = input.list().expect("a macro use is a list");
            assert_eq!(items.len(), params.len() + 1, "wrong number of arguments");
            substitute(template, &|name, node| {
                if let Some(at) = params.iter().position(|p| *p == name) {
                    Some(items[at + 1].clone())
                } else if capture.contains(&name) {
                    Some(Syntax::datum_to_syntax(&items[0], &node.to_datum()))
                } else {
                    None
                }
            })
        })
        .map_err(err)
    }
}

fn err(e: TooManyScopes) -> String {
    e.to_string()
}

/// `template` with each identifier `f` returns a replacement for replaced.
fn substitute(template: Syntax, f: &impl Fn(&str, &Syntax) -> Option<Syntax>) -> Syntax {
    if let Some(name) = template.identifier()
        && let Some(replacement) = f(name, &template)
    {
        return replacement;
    }
    let shape = match template.shape {
        Shape::List { items, tail } => Shape::List {
            items: items.into_iter().map(|i| substitute(i, f)).collect(),
            tail: tail.map(|t| Box::new(substitute(*t, f))),
        },
        Shape::Vector(items) => {
            Shape::Vector(items.into_iter().map(|i| substitute(i, f)).collect())
        }
        other => other,
    };
    Syntax { shape, ..template }
}

fn with_or() -> Expander {
    let mut ex = Expander::new();
    ex.define_macro("my-or", &["a", "b"], "(let ((tmp a)) (if tmp tmp b))", &[]);
    ex
}

#[test]
fn a_lambda_shadows_a_let() {
    let mut ex = Expander::new();
    assert_eq!(
        ex.expand_text("(let ((x 1)) (lambda (x) x))").unwrap(),
        "(let ((x.1 1)) (lambda (x.2) x.2))"
    );
}

#[test]
fn an_introduced_binding_does_not_capture_a_passed_reference() {
    let mut ex = with_or();
    assert_eq!(
        ex.expand_text("(let ((tmp 5)) (my-or #f tmp))").unwrap(),
        "(let ((tmp.1 5)) (let ((tmp.2 #f)) (if tmp.2 tmp.2 tmp.1)))"
    );
}

#[test]
fn an_introduced_reference_is_not_captured_by_a_user_binding() {
    let mut ex = with_or();
    assert_eq!(
        ex.expand_text("(let ((if 1)) (my-or #f if))").unwrap(),
        "(let ((if.1 1)) (let ((tmp.2 #f)) (if tmp.2 tmp.2 if.1)))"
    );
}

#[test]
fn nested_macro_steps_get_separate_scopes() {
    let mut ex = with_or();
    ex.define_macro("my-or3", &["a", "b", "c"], "(my-or a (my-or b c))", &[]);
    assert_eq!(
        ex.expand_text("(let ((tmp 1)) (my-or3 #f #f tmp))")
            .unwrap(),
        "(let ((tmp.1 1)) (let ((tmp.2 #f)) (if tmp.2 tmp.2 \
         (let ((tmp.3 #f)) (if tmp.3 tmp.3 tmp.1)))))"
    );
}

#[test]
fn datum_to_syntax_captures_on_purpose() {
    let mut ex = Expander::new();
    ex.define_macro(
        "aif",
        &["test", "then"],
        "(let ((it test)) (if it then #f))",
        &["it"],
    );
    assert_eq!(
        ex.expand_text("(let ((x 1)) (aif x it))").unwrap(),
        "(let ((x.1 1)) (let ((it.2 x.1)) (if it.2 it.2 #f)))"
    );
}

#[test]
fn quote_keeps_names() {
    let mut ex = with_or();
    assert_eq!(
        ex.expand_text("(my-or #f (quote tmp))").unwrap(),
        "(let ((tmp.1 #f)) (if tmp.1 tmp.1 (quote tmp)))"
    );
}

#[test]
fn unbound_identifiers_are_reported() {
    let mut ex = Expander::new();
    assert_eq!(ex.expand_text("(lambda (x) y)").unwrap_err(), "y: unbound");
}

#[test]
fn origins_mark_introduced_syntax() {
    let mut ex = with_or();
    let input = Syntax::from_datum(&read("(my-or #f tmp)"), ex.top);
    let output = ex.apply(0, input).unwrap();
    // (let ((tmp #f)) (if tmp tmp <passed tmp>))
    let items = output.list().unwrap();
    assert_eq!(
        items[0].origin.as_ref().unwrap().macro_name.as_ref(),
        "my-or"
    );
    let body = items[2].list().unwrap();
    assert!(body[1].origin.is_some(), "the introduced tmp");
    assert!(body[3].origin.is_none(), "the passed tmp");
}

#[test]
fn bound_and_free_identifier_equality() {
    let mut context = ScopeContext::new();
    let a = context.new_scope(ScopeKind::Library).unwrap();
    let m = context.new_scope(ScopeKind::Macro).unwrap();
    let outer = context.add(ScopeSetId::EMPTY, a).unwrap();
    let inner = context.add(outer, m).unwrap();
    let mut table = BindingTable::new();
    table
        .bind(
            &context,
            "x".to_owned(),
            outer,
            Phases::All,
            Binding::Core("x"),
        )
        .unwrap();
    let id = |scopes| Syntax::from_datum(&read("x"), scopes);
    let (x1, x2) = (id(outer), id(inner));
    assert!(bound_identifier_eq(&x1, &id(outer)));
    assert!(!bound_identifier_eq(&x1, &x2));
    assert!(free_identifier_eq(&context, &table, Phase::RUN, &x1, &x2));
    let y = Syntax::from_datum(&read("y"), outer);
    assert!(!free_identifier_eq(&context, &table, Phase::RUN, &x1, &y));
    assert!(free_identifier_eq(
        &context,
        &table,
        Phase::RUN,
        &y,
        &y.clone()
    ));
}

#[test]
fn incomparable_candidates_are_ambiguous() {
    let mut context = ScopeContext::new();
    let [a, b, c] = [0; 3].map(|_| context.new_scope(ScopeKind::Other).unwrap());
    let ab = context.intern(&[a, b]).unwrap();
    let ac = context.intern(&[a, c]).unwrap();
    let abc = context.intern(&[c, b, a]).unwrap();
    let only_a = context.intern(&[a]).unwrap();
    let mut table = BindingTable::new();
    for (scopes, n) in [(only_a, 0), (ab, 1), (ac, 2)] {
        table.bind(&context, "x", scopes, Phases::All, n).unwrap();
    }
    assert_eq!(
        table.resolve(&context, "x", abc, Phase::RUN),
        Resolution::Ambiguous(vec![&1, &2])
    );
    assert_eq!(
        table.resolve(&context, "x", ab, Phase::RUN),
        Resolution::Bound(&1)
    );
    assert_eq!(
        table.resolve(&context, "x", ScopeSetId::EMPTY, Phase::RUN),
        Resolution::Unbound
    );
}

#[test]
fn phases_filter_candidates() {
    let mut context = ScopeContext::new();
    let [a, b] = [0; 2].map(|_| context.new_scope(ScopeKind::Library).unwrap());
    let outer = context.intern(&[a]).unwrap();
    let inner = context.intern(&[a, b]).unwrap();
    let mut table = BindingTable::new();
    table
        .bind(&context, "x", outer, Phases::All, "outer")
        .unwrap();
    // `(import (for (lib) expand))`: visible only at phase 1.
    table
        .bind(
            &context,
            "x",
            inner,
            Phases::Only(vec![Phase::EXPAND]),
            "import",
        )
        .unwrap();
    assert_eq!(
        table.resolve(&context, "x", inner, Phase::RUN),
        Resolution::Bound(&"outer")
    );
    assert_eq!(
        table.resolve(&context, "x", inner, Phase::EXPAND),
        Resolution::Bound(&"import")
    );
    // The same set at another phase is a separate binding; at a shared
    // phase it is refused.
    table
        .bind(&context, "x", inner, Phases::Only(vec![Phase(2)]), "meta 2")
        .unwrap();
    let refused = table.bind(
        &context,
        "x",
        inner,
        Phases::Only(vec![Phase(2), Phase(3)]),
        "again",
    );
    assert_eq!(refused.unwrap_err().existing, &"meta 2");
}
