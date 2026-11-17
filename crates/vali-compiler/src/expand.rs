//! The expander (ADR 0015): from syntax objects to the resolved tree.
//!
//! Every identifier is resolved by sets of scopes. The core forms are bound
//! in the *core* scope, and a program's forms carry that scope and the
//! program's own, so a program's definitions shadow core forms and core
//! forms stay reachable from code that macros introduce.
//!
//! Bodies are expanded in two passes, as definition contexts must be: the
//! first finds every definition, splicing `begin`s, so that the second
//! expands each value and expression with every name in the body bound.
//!
//! A free identifier, one with no binding, is a module variable linked to
//! the global of that name; until libraries exist, that is how the standard
//! procedures are reached.

use std::collections::{HashSet, VecDeque};

use vali_syntax::{
    AlreadyBound, BindingTable, DatumKind, Exposed, Phase, Phases, Resolution, Scope, ScopeContext,
    ScopeKind, ScopeSetId, Span, Syntax, TooManyScopes,
};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};

type Result<T> = std::result::Result<T, CompileError>;

/// The core forms (ADR 0015 #15) that exist so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Core {
    Quote,
    If,
    Begin,
    Lambda,
    Let,
    Letrec,
    Set,
    Define,
}

const CORE: &[(&str, Core)] = &[
    ("quote", Core::Quote),
    ("if", Core::If),
    ("begin", Core::Begin),
    ("lambda", Core::Lambda),
    ("let", Core::Let),
    ("letrec", Core::Letrec),
    ("letrec*", Core::Letrec),
    ("set!", Core::Set),
    ("define", Core::Define),
];

/// What an identifier can be bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Core(Core),
    Local(VarId),
    /// A module variable the program defines.
    Global(String),
}

/// One form of a body, after the first pass.
enum Item {
    /// `(define id value)` at top level: a module variable.
    Global(String, Option<Syntax>),
    /// `(define id value)` in a body: a local variable.
    Local(VarId, Option<Syntax>),
    Expr(Syntax),
}

fn error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError {
        message: message.into(),
        span: Some(span),
    }
}

fn too_many(e: TooManyScopes) -> CompileError {
    CompileError {
        message: e.to_string(),
        span: None,
    }
}

pub(crate) struct Expander {
    pub(crate) vars: Vec<VarInfo>,
    pub(crate) lambdas: Vec<Lambda>,
    /// Module variables the program defines, so calls to them are never
    /// inlined as standard procedures.
    pub(crate) rebound: HashSet<String>,
    context: ScopeContext,
    table: BindingTable<String, Binding>,
    /// The scopes of the core forms.
    core: ScopeSetId,
    /// The scopes of the program's own forms.
    top: ScopeSetId,
    /// The `lambda`s being expanded, outermost first.
    stack: Vec<LambdaId>,
    /// Suffixes for module variables that macros introduce.
    fresh: usize,
}

impl Expander {
    pub(crate) fn new() -> Result<Expander> {
        let mut context = ScopeContext::new();
        let core_scope = context.new_scope(ScopeKind::Library).map_err(too_many)?;
        let program_scope = context.new_scope(ScopeKind::Library).map_err(too_many)?;
        let core = context.intern(&[core_scope]).map_err(too_many)?;
        let top = context
            .intern(&[core_scope, program_scope])
            .map_err(too_many)?;
        let mut table = BindingTable::new();
        for &(name, form) in CORE {
            table
                .bind(name.to_owned(), core, Phases::All, Binding::Core(form))
                .map_err(|_| CompileError {
                    message: format!("{name} is bound twice"),
                    span: None,
                })?;
        }
        Ok(Expander {
            vars: Vec::new(),
            lambdas: Vec::new(),
            rebound: HashSet::new(),
            context,
            table,
            core,
            top,
            stack: Vec::new(),
            fresh: 0,
        })
    }

    /// Expands a whole program. Returns the body `lambda`'s number.
    pub(crate) fn program(&mut self, datums: &[vali_syntax::Datum]) -> Result<LambdaId> {
        let forms = datums
            .iter()
            .map(|d| Syntax::from_datum(d, self.top))
            .collect();
        let body = self.open_lambda(None);
        let span = datums.first().map_or(
            Span {
                source: vali_syntax::SourceId(0),
                range: Default::default(),
            },
            |d| d.span,
        );
        let expr = self.body(forms, true, span)?;
        self.close_lambda(body, Vec::new(), None, expr);
        Ok(body)
    }

    // Scopes and bindings.

    fn scope(&mut self, kind: ScopeKind) -> Result<Scope> {
        self.context.new_scope(kind).map_err(too_many)
    }

    fn add(&mut self, stx: &Syntax, scope: Scope) -> Result<Syntax> {
        stx.add(&mut self.context, scope).map_err(too_many)
    }

    fn expose<'s>(&mut self, stx: &'s Syntax) -> Result<Exposed<'s>> {
        stx.expose(&mut self.context).map_err(too_many)
    }

    /// The elements of a proper list, or an error naming `what` it should
    /// have been.
    fn items(&mut self, stx: &Syntax, what: &str) -> Result<Vec<Syntax>> {
        stx.list_items(&mut self.context)
            .map_err(too_many)?
            .ok_or_else(|| error(format!("expected {what}"), stx.span()))
    }

    /// What `id` refers to: `None` if it is free.
    fn resolve(&self, id: &Syntax) -> Result<Option<Binding>> {
        match id.resolve(&self.context, &self.table, Phase(0)) {
            Resolution::Bound(b) => Ok(Some(b.clone())),
            Resolution::Unbound => Ok(None),
            Resolution::Ambiguous(_) => Err(error(
                format!(
                    "{} is ambiguous: more than one binding could be meant",
                    id.as_identifier().unwrap_or("this identifier")
                ),
                id.span(),
            )),
        }
    }

    /// The core form `stx` is, if it is a list whose head names one.
    fn core_form(&mut self, stx: &Syntax) -> Result<Option<(Core, Vec<Syntax>)>> {
        if !stx.is_list() {
            return Ok(None);
        }
        let Exposed::List { items, tail: None } = self.expose(stx)? else {
            return Ok(None);
        };
        match items.first() {
            Some(head) if head.as_identifier().is_some() => match self.resolve(head)? {
                Some(Binding::Core(form)) => Ok(Some((form, items))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// Binds `id`, refusing a second binding with the same scopes.
    fn bind(&mut self, id: &Syntax, binding: Binding, what: &str) -> Result<()> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error(format!("{what}: expected a name"), id.span()))?;
        self.table
            .bind(name.to_owned(), id.scopes(), Phases::All, binding)
            .map_err(|_| error(format!("{what}: {name} is bound twice"), id.span()))
    }

    /// A new local variable bound to `id` in the current `lambda`.
    fn bind_local(&mut self, id: &Syntax, what: &str) -> Result<VarId> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error(format!("{what}: expected a name"), id.span()))?
            .to_owned();
        self.vars.push(VarInfo {
            name,
            binder: self.current(),
            assigned: false,
            captured: false,
        });
        let v = self.vars.len() - 1;
        self.bind(id, Binding::Local(v), what)?;
        Ok(v)
    }

    // Lambdas.

    fn current(&self) -> LambdaId {
        *self.stack.last().expect("inside a lambda")
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
        id
    }

    fn close_lambda(
        &mut self,
        id: LambdaId,
        required: Vec<VarId>,
        rest: Option<VarId>,
        body: Expr,
    ) {
        self.stack.pop();
        let lambda = &mut self.lambdas[id];
        lambda.required = required;
        lambda.rest = rest;
        lambda.body = body;
    }

    /// A reference to `v`, recording captures and free variables.
    fn local(&mut self, v: VarId) -> Expr {
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

    // Bodies.

    /// A body: definitions, then expressions. At top level, definitions
    /// make module variables and may come between expressions; elsewhere
    /// they make local variables, all before the first expression.
    fn body(&mut self, forms: Vec<Syntax>, top: bool, span: Span) -> Result<Expr> {
        let mut queue: VecDeque<Syntax> = if top {
            forms.into()
        } else {
            // Definitions in a body shadow the parameters around it.
            let scope = self.scope(ScopeKind::Binder)?;
            forms
                .iter()
                .map(|f| self.add(f, scope))
                .collect::<Result<_>>()?
        };
        let mut items = Vec::new();
        while let Some(form) = queue.pop_front() {
            match self.core_form(&form)? {
                Some((Core::Begin, parts)) => {
                    for part in parts.into_iter().skip(1).rev() {
                        queue.push_front(part);
                    }
                }
                Some((Core::Define, parts)) => {
                    if !top && items.iter().any(|i| matches!(i, Item::Expr(_))) {
                        return Err(error(
                            "define: definitions must come before a body's expressions",
                            form.span(),
                        ));
                    }
                    let (id, value) = self.definition(&form, &parts)?;
                    items.push(if top {
                        Item::Global(self.define_global(&id)?, value)
                    } else {
                        Item::Local(self.bind_local(&id, "define")?, value)
                    });
                }
                _ => items.push(Item::Expr(form)),
            }
        }
        if top {
            let mut exprs = Vec::new();
            for item in items {
                exprs.push(match item {
                    Item::Global(name, value) => {
                        let value = match value {
                            Some(v) => self.named(&v, &name)?,
                            None => Expr::Unspecified,
                        };
                        Expr::SetGlobal(name, Box::new(value))
                    }
                    Item::Expr(e) => self.expr(&e)?,
                    Item::Local(..) => unreachable!("top-level definitions are global"),
                });
            }
            if exprs.is_empty() {
                exprs.push(Expr::Unspecified);
            }
            return Ok(Expr::Begin(exprs));
        }
        let mut bindings = Vec::new();
        let mut exprs = Vec::new();
        for item in items {
            match item {
                Item::Local(v, value) => {
                    let name = self.vars[v].name.clone();
                    let value = match value {
                        Some(value) => self.named(&value, &name)?,
                        None => Expr::Unspecified,
                    };
                    bindings.push((v, value));
                }
                Item::Expr(e) => exprs.push(e),
                Item::Global(..) => unreachable!("body definitions are local"),
            }
        }
        if exprs.is_empty() {
            return Err(error("a body needs at least one expression", span));
        }
        let body = self.sequence(&exprs, span)?;
        Ok(if bindings.is_empty() {
            body
        } else {
            Expr::Letrec(bindings, Box::new(body))
        })
    }

    /// A top-level definition's module variable. One the program wrote
    /// keeps its name; one a macro introduced gets a fresh name, so it
    /// cannot collide with the program's.
    fn define_global(&mut self, id: &Syntax) -> Result<String> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error("define: expected a name", id.span()))?;
        let global = if id.scopes() == self.top {
            name.to_owned()
        } else {
            self.fresh += 1;
            format!("{name}~{}", self.fresh)
        };
        match self.table.bind(
            name.to_owned(),
            id.scopes(),
            Phases::All,
            Binding::Global(global.clone()),
        ) {
            Ok(()) => {}
            // A redefinition at top level assigns the same variable.
            Err(AlreadyBound {
                existing: Binding::Global(existing),
            }) => return Ok(existing.clone()),
            Err(_) => return Err(error(format!("define: {name} is bound twice"), id.span())),
        }
        self.rebound.insert(global.clone());
        Ok(global)
    }

    /// `(define id)`, `(define id value)` or `(define (id . formals) body
    /// ...)`: the name and the value, if any.
    fn definition(&mut self, form: &Syntax, parts: &[Syntax]) -> Result<(Syntax, Option<Syntax>)> {
        match parts {
            [_, id] if id.as_identifier().is_some() => Ok((id.clone(), None)),
            [_, id, value] if id.as_identifier().is_some() => Ok((id.clone(), Some(value.clone()))),
            [keyword, target, body @ ..] if target.is_list() => {
                let Exposed::List { items, tail } = self.expose(target)? else {
                    unreachable!("a list")
                };
                let Some((id, params)) = items.split_first() else {
                    return Err(error("define: expected a name", target.span()));
                };
                // `(lambda formals body ...)`, with `lambda` meaning the
                // core form whatever the program binds.
                let lambda = Syntax::identifier("lambda", self.core, keyword.span(), None);
                let formals = Syntax::list(
                    params.to_vec(),
                    tail,
                    target.scopes(),
                    target.span(),
                    target.origin().cloned(),
                );
                let mut elements = vec![lambda, formals];
                elements.extend(body.iter().cloned());
                let value = Syntax::list(
                    elements,
                    None,
                    form.scopes(),
                    form.span(),
                    form.origin().cloned(),
                );
                Ok((id.clone(), Some(value)))
            }
            _ => Err(error(
                "define: expected a name and at most one expression",
                form.span(),
            )),
        }
    }

    fn sequence(&mut self, forms: &[Syntax], span: Span) -> Result<Expr> {
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

    // Expressions.

    /// Expands `stx`, naming it `name` if it is a `lambda`.
    fn named(&mut self, stx: &Syntax, name: &str) -> Result<Expr> {
        if let Some((Core::Lambda, parts)) = self.core_form(stx)? {
            return self.lambda(Some(name), &parts, stx.span());
        }
        self.expr(stx)
    }

    pub(crate) fn expr(&mut self, stx: &Syntax) -> Result<Expr> {
        if let Some(name) = stx.as_identifier() {
            return match self.resolve(stx)? {
                Some(Binding::Local(v)) => Ok(self.local(v)),
                Some(Binding::Global(global)) => Ok(Expr::Global(global)),
                None => Ok(Expr::Global(name.to_owned())),
                Some(Binding::Core(_)) => Err(error(
                    format!("{name}: a syntactic keyword is not an expression"),
                    stx.span(),
                )),
            };
        }
        if let Some(atom) = stx.as_atom() {
            return match atom {
                DatumKind::Keyword(_) => Err(error("keywords are not supported yet", stx.span())),
                DatumKind::Labelled(..) | DatumKind::LabelRef(_) => Err(error(
                    "datum labels are only allowed inside quoted data",
                    stx.span(),
                )),
                _ => Ok(Expr::Const(stx.to_datum())),
            };
        }
        let span = stx.span();
        let (items, tail) = match self.expose(stx)? {
            Exposed::List { items, tail } => (items, tail),
            // Vectors are self-evaluating (R7RS 4.1.2).
            _ => return Ok(Expr::Const(stx.to_datum())),
        };
        if tail.is_some() {
            return Err(error("a dotted list is not an expression", span));
        }
        let Some(head) = items.first() else {
            return Err(error("() is not an expression; quote it", span));
        };
        if head.as_identifier().is_some()
            && let Some(Binding::Core(form)) = self.resolve(head)?
        {
            let keyword = head.as_identifier().expect("an identifier").to_owned();
            return self.core(form, &keyword, &items, span);
        }
        if items.len() - 1 > usize::from(u8::MAX) - 4 {
            return Err(error("too many arguments in one call", span));
        }
        let f = self.expr(head)?;
        let args = items[1..]
            .iter()
            .map(|a| self.expr(a))
            .collect::<Result<Vec<_>>>()?;
        Ok(Expr::Call(Box::new(f), args, span))
    }

    fn core(&mut self, form: Core, keyword: &str, items: &[Syntax], span: Span) -> Result<Expr> {
        let args = &items[1..];
        match form {
            Core::Quote => match args {
                [datum] => Ok(Expr::Const(datum.to_datum())),
                _ => Err(error(format!("{keyword}: expected one datum"), span)),
            },
            Core::If => {
                let (c, t, e) = match args {
                    [c, t] => (c, t, None),
                    [c, t, e] => (c, t, Some(e)),
                    _ => {
                        return Err(error(
                            format!("{keyword}: expected a test and one or two branches"),
                            span,
                        ));
                    }
                };
                let c = self.expr(c)?;
                let t = self.expr(t)?;
                let e = match e {
                    Some(e) => self.expr(e)?,
                    None => Expr::Unspecified,
                };
                Ok(Expr::If(Box::new(c), Box::new(t), Box::new(e)))
            }
            Core::Begin => self.sequence(args, span),
            Core::Lambda => self.lambda(None, items, span),
            Core::Let => self.let_(keyword, args, span),
            Core::Letrec => self.letrec(keyword, args, span),
            Core::Set => {
                let [target, value] = args else {
                    return Err(error(
                        format!("{keyword}: expected a variable and an expression"),
                        span,
                    ));
                };
                let Some(name) = target.as_identifier() else {
                    return Err(error(
                        format!("{keyword}: expected a variable"),
                        target.span(),
                    ));
                };
                let name = name.to_owned();
                match self.resolve(target)? {
                    Some(Binding::Local(v)) => {
                        let value = self.expr(value)?;
                        self.vars[v].assigned = true;
                        let Expr::Local(v) = self.local(v) else {
                            unreachable!("a local")
                        };
                        Ok(Expr::SetLocal(v, Box::new(value)))
                    }
                    Some(Binding::Global(global)) => {
                        let value = self.expr(value)?;
                        Ok(Expr::SetGlobal(global, Box::new(value)))
                    }
                    Some(Binding::Core(_)) => Err(error(
                        format!("{keyword}: {name} is a syntactic keyword"),
                        target.span(),
                    )),
                    None => Err(error(
                        format!("{keyword}: {name} is not defined"),
                        target.span(),
                    )),
                }
            }
            Core::Define => Err(error(
                "define is only allowed at top level or at the start of a body",
                span,
            )),
        }
    }

    /// `(lambda formals body ...)`, from its items.
    fn lambda(&mut self, name: Option<&str>, items: &[Syntax], span: Span) -> Result<Expr> {
        let [keyword, formals, body @ ..] = items else {
            let keyword = items
                .first()
                .and_then(Syntax::as_identifier)
                .unwrap_or("lambda");
            return Err(error(
                format!("{keyword}: expected parameters and a body"),
                span,
            ));
        };
        let keyword = keyword.as_identifier().unwrap_or("lambda").to_owned();
        let id = self.open_lambda(name);
        let scope = self.scope(ScopeKind::Binder)?;
        let formals = self.add(formals, scope)?;
        let (params, rest) = if formals.as_identifier().is_some() {
            (Vec::new(), Some(formals))
        } else if formals.is_list() {
            let Exposed::List { items, tail } = self.expose(&formals)? else {
                unreachable!("a list")
            };
            (items, tail)
        } else {
            return Err(error(
                format!("{keyword}: expected parameters"),
                formals.span(),
            ));
        };
        if params.len() > usize::from(u8::MAX) {
            return Err(error(format!("{keyword}: too many parameters"), span));
        }
        let required = params
            .iter()
            .map(|p| self.bind_local(p, &keyword))
            .collect::<Result<Vec<_>>>()?;
        let rest = rest.map(|r| self.bind_local(&r, &keyword)).transpose()?;
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        let body = self.body(body, false, span)?;
        self.close_lambda(id, required, rest, body);
        Ok(Expr::Lambda(id))
    }

    /// The `(id init)` pairs of a binding list.
    fn bindings(&mut self, list: &Syntax, keyword: &str) -> Result<Vec<(Syntax, Syntax)>> {
        let list = self
            .items(list, "bindings")
            .map_err(|e| error(format!("{keyword}: {}", e.message), list.span()))?;
        list.iter()
            .map(
                |b| match b.list_items(&mut self.context).map_err(too_many)? {
                    Some(pair) if pair.len() == 2 && pair[0].as_identifier().is_some() => {
                        Ok((pair[0].clone(), pair[1].clone()))
                    }
                    _ => Err(error(
                        format!("{keyword}: expected (name expression)"),
                        b.span(),
                    )),
                },
            )
            .collect()
    }

    /// `(let ((id init) ...) body ...)` and named `let`.
    fn let_(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        match args {
            [name, bindings, body @ ..] if name.as_identifier().is_some() => {
                // Named let: a loop procedure, called with the inits, which
                // are expanded outside its scope.
                let bindings = self.bindings(bindings, keyword)?;
                let inits = bindings
                    .iter()
                    .map(|(_, init)| self.expr(init))
                    .collect::<Result<Vec<_>>>()?;
                let scope = self.scope(ScopeKind::Binder)?;
                let id = self.add(name, scope)?;
                let v = self.bind_local(&id, keyword)?;
                let params = bindings.into_iter().map(|(p, _)| p).collect();
                let formals = Syntax::list(params, None, name.scopes(), span, None);
                let mut lambda = vec![Syntax::identifier("lambda", self.core, span, None), formals];
                lambda.extend(body.iter().cloned());
                let lambda = Syntax::list(lambda, None, name.scopes(), span, None);
                let lambda = self.add(&lambda, scope)?;
                let Some((Core::Lambda, items)) = self.core_form(&lambda)? else {
                    unreachable!("built as a lambda")
                };
                let name = self.vars[v].name.clone();
                let lambda = self.lambda(Some(&name), &items, span)?;
                Ok(Expr::Letrec(
                    vec![(v, lambda)],
                    Box::new(Expr::Call(Box::new(Expr::Local(v)), inits, span)),
                ))
            }
            [bindings, body @ ..] => {
                let bindings = self.bindings(bindings, keyword)?;
                let mut inits = Vec::new();
                for (id, init) in &bindings {
                    let name = id.as_identifier().expect("checked").to_owned();
                    inits.push(self.named(init, &name)?);
                }
                let scope = self.scope(ScopeKind::Binder)?;
                let mut vars = Vec::new();
                for (id, _) in &bindings {
                    let id = self.add(id, scope)?;
                    vars.push(self.bind_local(&id, keyword)?);
                }
                let body = body
                    .iter()
                    .map(|f| self.add(f, scope))
                    .collect::<Result<Vec<_>>>()?;
                let body = self.body(body, false, span)?;
                Ok(Expr::Let(
                    vars.into_iter().zip(inits).collect(),
                    Box::new(body),
                ))
            }
            [] => Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            )),
        }
    }

    /// `(letrec ((id init) ...) body ...)`, and `letrec*`: each init sees
    /// every variable, and they run in order.
    fn letrec(&mut self, keyword: &str, args: &[Syntax], span: Span) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            ));
        };
        let bindings = self.bindings(bindings, keyword)?;
        let scope = self.scope(ScopeKind::Binder)?;
        let mut vars = Vec::new();
        for (id, _) in &bindings {
            let id = self.add(id, scope)?;
            vars.push(self.bind_local(&id, keyword)?);
        }
        let mut inits = Vec::new();
        for (&v, (_, init)) in vars.iter().zip(&bindings) {
            let init = self.add(init, scope)?;
            let name = self.vars[v].name.clone();
            inits.push(self.named(&init, &name)?);
        }
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        let body = self.body(body, false, span)?;
        Ok(Expr::Letrec(
            vars.into_iter().zip(inits).collect(),
            Box::new(body),
        ))
    }
}
