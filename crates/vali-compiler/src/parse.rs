//! From datums to the resolved tree.
//!
//! Parsing resolves every identifier: to a lexical variable, or to a module
//! variable linked to a global by name. It records which variables are
//! assigned and which are captured, and each `lambda`'s free variables.
//!
//! Until the expander exists, it also rewrites a handful of derived forms
//! into core forms: `and`, `or`, `cond`, `when`, `unless`, `let*` and named
//! `let`. This stopgap goes when the expander arrives.

use std::collections::{HashMap, HashSet};

use vali_syntax::{Datum, DatumKind, Span};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};

pub(crate) struct Parser {
    pub(crate) vars: Vec<VarInfo>,
    pub(crate) lambdas: Vec<Lambda>,
    /// Module variables that the program defines or assigns, so calls to
    /// them are never inlined as standard procedures.
    pub(crate) rebound: HashSet<String>,
    scopes: Vec<HashMap<String, VarId>>,
    /// The `lambda`s being parsed, outermost first.
    stack: Vec<LambdaId>,
}

fn error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError {
        message: message.into(),
        span: Some(span),
    }
}

fn symbol(d: &Datum) -> Option<&str> {
    match &d.kind {
        DatumKind::Symbol(name) => Some(name),
        _ => None,
    }
}

/// The items of a proper list.
fn items(d: &Datum) -> Option<&[Datum]> {
    match &d.kind {
        DatumKind::List { items, tail: None } => Some(items),
        _ => None,
    }
}

type Result<T> = std::result::Result<T, CompileError>;

impl Parser {
    pub(crate) fn new() -> Parser {
        Parser {
            vars: Vec::new(),
            lambdas: Vec::new(),
            rebound: HashSet::new(),
            scopes: Vec::new(),
            stack: Vec::new(),
        }
    }

    fn current(&self) -> LambdaId {
        *self.stack.last().expect("inside a lambda")
    }

    fn new_var(&mut self, name: &str) -> VarId {
        self.vars.push(VarInfo {
            name: name.to_owned(),
            binder: self.current(),
            assigned: false,
            captured: false,
        });
        self.vars.len() - 1
    }

    fn bind(&mut self, name: &str) -> VarId {
        let v = self.new_var(name);
        self.scopes
            .last_mut()
            .expect("a scope")
            .insert(name.to_owned(), v);
        v
    }

    fn lookup(&self, name: &str) -> Option<VarId> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    /// Resolves a reference, recording captures and free variables.
    fn reference(&mut self, name: &str) -> Expr {
        let Some(v) = self.lookup(name) else {
            return Expr::Global(name.to_owned());
        };
        let binder = self.vars[v].binder;
        if binder != self.current() {
            self.vars[v].captured = true;
            let from = self
                .stack
                .iter()
                .position(|&l| l == binder)
                .expect("the binder is an enclosing lambda");
            for &l in &self.stack[from + 1..] {
                if !self.lambdas[l].free.contains(&v) {
                    self.lambdas[l].free.push(v);
                }
            }
        }
        Expr::Local(v)
    }

    /// Whether `name` here means the special form of that name.
    fn special(&self, name: &str) -> bool {
        self.lookup(name).is_none()
    }

    /// Parses a whole program: top-level forms, where `define` makes module
    /// variables. Returns the body `lambda`'s number.
    pub(crate) fn program(&mut self, forms: &[Datum]) -> Result<LambdaId> {
        let body = self.open_lambda(None);
        let mut exprs = Vec::new();
        self.toplevel(forms, &mut exprs)?;
        if exprs.is_empty() {
            exprs.push(Expr::Unspecified);
        }
        self.close_lambda(body, Vec::new(), None, Expr::Begin(exprs));
        Ok(body)
    }

    fn toplevel(&mut self, forms: &[Datum], out: &mut Vec<Expr>) -> Result<()> {
        for form in forms {
            if let Some([head, rest @ ..]) = items(form)
                && let Some(name) = symbol(head)
                && self.special(name)
            {
                match name {
                    "define" => {
                        let (name, value) = self.definition(form, rest)?;
                        self.rebound.insert(name.clone());
                        out.push(Expr::SetGlobal(name, Box::new(value)));
                        continue;
                    }
                    "begin" => {
                        self.toplevel(rest, out)?;
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(self.expr(form)?);
        }
        Ok(())
    }

    /// `(define name expr)`, `(define name)` or `(define (name . formals)
    /// body ...)`: the name, and the value's expression.
    fn definition(&mut self, form: &Datum, rest: &[Datum]) -> Result<(String, Expr)> {
        match rest {
            [target, body @ ..] if symbol(target).is_some() => {
                let name = symbol(target).expect("checked").to_owned();
                let value = match body {
                    [] => Expr::Unspecified,
                    [value] => self.named(value, &name)?,
                    _ => return Err(error("define: expected one expression", form.span)),
                };
                Ok((name, value))
            }
            [target, body @ ..] => match &target.kind {
                DatumKind::List { items, tail } if !items.is_empty() => {
                    let name = symbol(&items[0])
                        .ok_or_else(|| error("define: expected a name", target.span))?
                        .to_owned();
                    let formals = Datum {
                        kind: DatumKind::List {
                            items: items[1..].to_vec(),
                            tail: tail.clone(),
                        },
                        span: target.span,
                    };
                    let lambda = self.lambda(Some(&name), &formals, body, form.span)?;
                    Ok((name, lambda))
                }
                _ => Err(error("define: expected a name", form.span)),
            },
            [] => Err(error("define: expected a name", form.span)),
        }
    }

    /// Parses `d`, naming it `name` if it is a `lambda`.
    fn named(&mut self, d: &Datum, name: &str) -> Result<Expr> {
        if let Some([head, formals, body @ ..]) = items(d)
            && symbol(head) == Some("lambda")
            && self.special("lambda")
        {
            return self.lambda(Some(name), formals, body, d.span);
        }
        self.expr(d)
    }

    fn open_lambda(&mut self, name: Option<&str>) -> LambdaId {
        self.lambdas.push(Lambda {
            name: name.map(str::to_owned),
            required: Vec::new(),
            rest: None,
            body: Expr::Unspecified,
            free: Vec::new(),
        });
        let id = self.lambdas.len() - 1;
        self.stack.push(id);
        self.scopes.push(HashMap::new());
        id
    }

    fn close_lambda(
        &mut self,
        id: LambdaId,
        required: Vec<VarId>,
        rest: Option<VarId>,
        body: Expr,
    ) {
        self.scopes.pop();
        self.stack.pop();
        let lambda = &mut self.lambdas[id];
        lambda.required = required;
        lambda.rest = rest;
        lambda.body = body;
    }

    fn lambda(
        &mut self,
        name: Option<&str>,
        formals: &Datum,
        body: &[Datum],
        span: Span,
    ) -> Result<Expr> {
        let id = self.open_lambda(name);
        let (names, rest_name) = match &formals.kind {
            DatumKind::Symbol(rest) => (Vec::new(), Some(rest.as_str())),
            DatumKind::List { items, tail } => {
                let names = items
                    .iter()
                    .map(|d| {
                        symbol(d).ok_or_else(|| error("lambda: expected a parameter name", d.span))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let rest = match tail {
                    None => None,
                    Some(t) => Some(
                        symbol(t)
                            .ok_or_else(|| error("lambda: expected a parameter name", t.span))?,
                    ),
                };
                (names, rest)
            }
            _ => return Err(error("lambda: expected parameters", formals.span)),
        };
        if names.len() > usize::from(u8::MAX) {
            return Err(error("lambda: too many parameters", span));
        }
        let required: Vec<VarId> = names.iter().map(|n| self.bind(n)).collect();
        let rest = rest_name.map(|n| self.bind(n));
        let body = self.body(body, span)?;
        self.close_lambda(id, required, rest, body);
        Ok(Expr::Lambda(id))
    }

    /// A body: internal definitions, which become a `letrec*`, then at
    /// least one expression.
    fn body(&mut self, forms: &[Datum], span: Span) -> Result<Expr> {
        let definitions = forms
            .iter()
            .take_while(|f| {
                items(f)
                    .and_then(|i| i.first())
                    .and_then(symbol)
                    .is_some_and(|s| s == "define" && self.special("define"))
            })
            .count();
        let (definitions, rest) = forms.split_at(definitions);
        if rest.is_empty() {
            return Err(error("a body needs at least one expression", span));
        }
        if definitions.is_empty() {
            return self.sequence(rest, span);
        }
        self.scopes.push(HashMap::new());
        // Bind every name first: each definition sees all of them.
        let mut names = Vec::new();
        for form in definitions {
            let rest = &items(form).expect("a list")[1..];
            let name = match rest.first().map(|d| &d.kind) {
                Some(DatumKind::Symbol(name)) => name.clone(),
                Some(DatumKind::List { items, .. }) if !items.is_empty() => symbol(&items[0])
                    .ok_or_else(|| error("define: expected a name", form.span))?
                    .to_owned(),
                _ => return Err(error("define: expected a name", form.span)),
            };
            names.push(self.bind(&name));
        }
        let mut bindings = Vec::new();
        for (form, v) in definitions.iter().zip(names) {
            let rest = &items(form).expect("a list")[1..];
            let (_, value) = self.definition(form, rest)?;
            bindings.push((v, value));
        }
        let body = self.sequence(rest, span)?;
        self.scopes.pop();
        Ok(Expr::Letrec(bindings, Box::new(body)))
    }

    fn sequence(&mut self, forms: &[Datum], span: Span) -> Result<Expr> {
        if forms.is_empty() {
            return Err(error("expected at least one expression", span));
        }
        let mut exprs = forms
            .iter()
            .map(|f| self.expr(f))
            .collect::<Result<Vec<_>>>()?;
        Ok(if exprs.len() == 1 {
            exprs.pop().expect("one")
        } else {
            Expr::Begin(exprs)
        })
    }

    pub(crate) fn expr(&mut self, d: &Datum) -> Result<Expr> {
        match &d.kind {
            DatumKind::Symbol(name) => Ok(self.reference(name)),
            DatumKind::Keyword(_) => Err(error("keywords are not supported yet", d.span)),
            DatumKind::Labelled(..) | DatumKind::LabelRef(_) => Err(error(
                "datum labels are only allowed inside quoted data",
                d.span,
            )),
            DatumKind::List { items, tail } => {
                if tail.is_some() {
                    return Err(error("a dotted list is not an expression", d.span));
                }
                let Some((head, args)) = items.split_first() else {
                    return Err(error("() is not an expression; quote it", d.span));
                };
                if let Some(name) = symbol(head)
                    && self.special(name)
                    && let Some(result) = self.special_form(name, args, d)
                {
                    return result;
                }
                let f = self.expr(head)?;
                if args.len() > usize::from(u8::MAX) - 4 {
                    return Err(error("too many arguments in one call", d.span));
                }
                let args = args
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Expr::Call(Box::new(f), args, d.span))
            }
            _ => Ok(Expr::Const(d.clone())),
        }
    }

    /// A special form, or `None` if `name` names none.
    fn special_form(&mut self, name: &str, args: &[Datum], d: &Datum) -> Option<Result<Expr>> {
        let span = d.span;
        Some(match name {
            "quote" => match args {
                [datum] => Ok(Expr::Const(datum.clone())),
                _ => Err(error("quote: expected one datum", span)),
            },
            "if" => (|| match args {
                [c, t] => Ok(Expr::If(
                    Box::new(self.expr(c)?),
                    Box::new(self.expr(t)?),
                    Box::new(Expr::Unspecified),
                )),
                [c, t, e] => Ok(Expr::If(
                    Box::new(self.expr(c)?),
                    Box::new(self.expr(t)?),
                    Box::new(self.expr(e)?),
                )),
                _ => Err(error("if: expected a test and one or two branches", span)),
            })(),
            "begin" => self.sequence(args, span),
            "lambda" => match args {
                [formals, body @ ..] => self.lambda(None, formals, body, span),
                [] => Err(error("lambda: expected parameters and a body", span)),
            },
            "set!" => (|| match args {
                [target, value] => {
                    let name = symbol(target)
                        .ok_or_else(|| error("set!: expected a variable", target.span))?;
                    let value = Box::new(self.expr(value)?);
                    match self.reference(name) {
                        Expr::Local(v) => {
                            self.vars[v].assigned = true;
                            Ok(Expr::SetLocal(v, value))
                        }
                        _ => {
                            self.rebound.insert(name.to_owned());
                            Ok(Expr::SetGlobal(name.to_owned(), value))
                        }
                    }
                }
                _ => Err(error("set!: expected a variable and an expression", span)),
            })(),
            "define" => Err(error(
                "define is only allowed at top level or at the start of a body",
                span,
            )),
            "let" => self.let_form(args, span),
            "let*" => self.let_star(args, span),
            "letrec" | "letrec*" => self.letrec(args, span),
            "and" => self.and(args),
            "or" => self.or(args),
            "cond" => self.cond(args),
            "when" | "unless" => (|| {
                let [test, body @ ..] = args else {
                    return Err(error(format!("{name}: expected a test"), span));
                };
                let test = self.expr(test)?;
                let body = self.sequence(body, span)?;
                Ok(if name == "when" {
                    Expr::If(Box::new(test), Box::new(body), Box::new(Expr::Unspecified))
                } else {
                    Expr::If(Box::new(test), Box::new(Expr::Unspecified), Box::new(body))
                })
            })(),
            _ => return None,
        })
    }

    /// The `(name init)` pairs of a binding list.
    fn bindings<'d>(&self, list: &'d Datum, form: &str) -> Result<Vec<(&'d str, &'d Datum)>> {
        let list =
            items(list).ok_or_else(|| error(format!("{form}: expected bindings"), list.span))?;
        list.iter()
            .map(|b| match items(b) {
                Some([name, init]) if symbol(name).is_some() => {
                    Ok((symbol(name).expect("checked"), init))
                }
                _ => Err(error(format!("{form}: expected (name expression)"), b.span)),
            })
            .collect()
    }

    fn let_form(&mut self, args: &[Datum], span: Span) -> Result<Expr> {
        match args {
            // Named let: the inits are resolved outside the loop's scope.
            [name, bindings, body @ ..] if symbol(name).is_some() => {
                let name = symbol(name).expect("checked");
                let bindings = self.bindings(bindings, "let")?;
                let inits = bindings
                    .iter()
                    .map(|(_, init)| self.expr(init))
                    .collect::<Result<Vec<_>>>()?;
                self.scopes.push(HashMap::new());
                let v = self.bind(name);
                let formals = Datum {
                    kind: DatumKind::List {
                        items: bindings
                            .iter()
                            .map(|(n, d)| Datum {
                                kind: DatumKind::Symbol((*n).to_owned()),
                                span: d.span,
                            })
                            .collect(),
                        tail: None,
                    },
                    span,
                };
                let lambda = self.lambda(Some(name), &formals, body, span)?;
                self.scopes.pop();
                Ok(Expr::Letrec(
                    vec![(v, lambda)],
                    Box::new(Expr::Call(Box::new(Expr::Local(v)), inits, span)),
                ))
            }
            [bindings, body @ ..] => {
                let bindings = self.bindings(bindings, "let")?;
                let inits = bindings
                    .iter()
                    .map(|(name, init)| self.named(init, name))
                    .collect::<Result<Vec<_>>>()?;
                self.scopes.push(HashMap::new());
                let vars: Vec<VarId> = bindings.iter().map(|(n, _)| self.bind(n)).collect();
                let body = self.body(body, span)?;
                self.scopes.pop();
                Ok(Expr::Let(
                    vars.into_iter().zip(inits).collect(),
                    Box::new(body),
                ))
            }
            [] => Err(error("let: expected bindings and a body", span)),
        }
    }

    fn let_star(&mut self, args: &[Datum], span: Span) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error("let*: expected bindings and a body", span));
        };
        let bindings = self.bindings(bindings, "let*")?;
        let mut vars = Vec::new();
        for (name, init) in &bindings {
            let init = self.named(init, name)?;
            self.scopes.push(HashMap::new());
            vars.push((self.bind(name), init));
        }
        let mut result = self.body(body, span)?;
        for (v, init) in vars.into_iter().rev() {
            self.scopes.pop();
            result = Expr::Let(vec![(v, init)], Box::new(result));
        }
        Ok(result)
    }

    fn letrec(&mut self, args: &[Datum], span: Span) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error("letrec: expected bindings and a body", span));
        };
        let bindings = self.bindings(bindings, "letrec")?;
        self.scopes.push(HashMap::new());
        let vars: Vec<VarId> = bindings.iter().map(|(n, _)| self.bind(n)).collect();
        let inits = bindings
            .iter()
            .map(|(name, init)| self.named(init, name))
            .collect::<Result<Vec<_>>>()?;
        let body = self.body(body, span)?;
        self.scopes.pop();
        Ok(Expr::Letrec(
            vars.into_iter().zip(inits).collect(),
            Box::new(body),
        ))
    }

    fn and(&mut self, args: &[Datum]) -> Result<Expr> {
        match args {
            [] => Ok(Expr::Const(Datum {
                kind: DatumKind::Boolean(true),
                span: Span {
                    source: vali_syntax::SourceId(0),
                    range: Default::default(),
                },
            })),
            [last] => self.expr(last),
            [first, rest @ ..] => Ok(Expr::If(
                Box::new(self.expr(first)?),
                Box::new(self.and(rest)?),
                Box::new(false_const(first.span)),
            )),
        }
    }

    fn or(&mut self, args: &[Datum]) -> Result<Expr> {
        match args {
            [] => Ok(false_const(Span {
                source: vali_syntax::SourceId(0),
                range: Default::default(),
            })),
            [last] => self.expr(last),
            [first, rest @ ..] => {
                let first = self.expr(first)?;
                let t = self.new_var("or");
                let rest = self.or(rest)?;
                Ok(Expr::Let(
                    vec![(t, first)],
                    Box::new(Expr::If(
                        Box::new(Expr::Local(t)),
                        Box::new(Expr::Local(t)),
                        Box::new(rest),
                    )),
                ))
            }
        }
    }

    fn cond(&mut self, clauses: &[Datum]) -> Result<Expr> {
        let Some((clause, rest)) = clauses.split_first() else {
            return Ok(Expr::Unspecified);
        };
        let parts = items(clause).ok_or_else(|| error("cond: expected a clause", clause.span))?;
        match parts {
            [head, body @ ..] if symbol(head) == Some("else") && self.special("else") => {
                if !rest.is_empty() {
                    return Err(error("cond: else must be the last clause", clause.span));
                }
                self.sequence(body, clause.span)
            }
            [test] => {
                let test = self.expr(test)?;
                let t = self.new_var("cond");
                let rest = self.cond(rest)?;
                Ok(Expr::Let(
                    vec![(t, test)],
                    Box::new(Expr::If(
                        Box::new(Expr::Local(t)),
                        Box::new(Expr::Local(t)),
                        Box::new(rest),
                    )),
                ))
            }
            [test, arrow, receiver] if symbol(arrow) == Some("=>") && self.special("=>") => {
                let test = self.expr(test)?;
                let receiver = self.expr(receiver)?;
                let t = self.new_var("cond");
                let rest = self.cond(rest)?;
                Ok(Expr::Let(
                    vec![(t, test)],
                    Box::new(Expr::If(
                        Box::new(Expr::Local(t)),
                        Box::new(Expr::Call(
                            Box::new(receiver),
                            vec![Expr::Local(t)],
                            clause.span,
                        )),
                        Box::new(rest),
                    )),
                ))
            }
            [test, body @ ..] => Ok(Expr::If(
                Box::new(self.expr(test)?),
                Box::new(self.sequence(body, clause.span)?),
                Box::new(self.cond(rest)?),
            )),
            [] => Err(error("cond: empty clause", clause.span)),
        }
    }
}

fn false_const(span: Span) -> Expr {
    Expr::Const(Datum {
        kind: DatumKind::Boolean(false),
        span,
    })
}
