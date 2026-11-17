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
//!
//! Macros are `syntax-rules` transformers, matched and built in Rust
//! (`pattern`). Each use flips a fresh macro scope on its input and output;
//! a use in a definition context also adds a use-site scope to its input,
//! which definitions then drop, so a macro can define a name its use
//! supplies. The derived forms are macros in `prelude.scm`, bound in the
//! core scope.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use vali_syntax::{
    AlreadyBound, BindingTable, DatumKind, Exposed, Origin, Phase, Phases, Resolution, Scope,
    ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax, TooManyScopes, free_identifier_eq,
};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};
use crate::pattern::{self, Builder, Match, Pattern, Spelling, Template, Vars};

/// The derived forms, as macros.
const PRELUDE: &str = include_str!("prelude.scm");

/// How many macro steps may produce one form, counting both uses nested
/// in expressions and a body's forms that expand into further uses, so a
/// macro that expands into itself forever is reported. Legitimate code
/// nests deeply too: a `cond` with a thousand clauses is a thousand uses
/// deep.
const MAX_MACRO_DEPTH: usize = 10_000;

type Result<T> = std::result::Result<T, CompileError>;

/// When less stack than this is left, recursion moves to a new segment of
/// `STACK_GROWTH` bytes.
pub(crate) const RED_ZONE: usize = 128 * 1024;
pub(crate) const STACK_GROWTH: usize = 2 * 1024 * 1024;

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
    DefineSyntax,
    LetSyntax,
    LetrecSyntax,
    SyntaxRules,
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
    ("define-syntax", Core::DefineSyntax),
    ("let-syntax", Core::LetSyntax),
    ("letrec-syntax", Core::LetrecSyntax),
    ("syntax-rules", Core::SyntaxRules),
];

/// What an identifier can be bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Core(Core),
    Local(VarId),
    /// A module variable the program defines.
    Global(String),
    /// A macro, by number.
    Macro(usize),
}

/// A `syntax-rules` macro.
struct Macro {
    name: String,
    clauses: Vec<Clause>,
}

struct Clause {
    pattern: Pattern,
    template: Template,
    /// Each pattern variable's depth.
    depths: Vec<usize>,
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
    macros: Vec<Macro>,
    /// Macro uses in expressions being expanded, one inside another.
    depth: usize,
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
            macros: Vec::new(),
            depth: 0,
        })
    }

    /// Expands a whole program. Returns the body `lambda`'s number.
    pub(crate) fn program(&mut self, datums: &[vali_syntax::Datum]) -> Result<LambdaId> {
        let forms = datums
            .iter()
            .map(|d| Syntax::from_datum(d, self.top))
            .collect();
        let body = self.open_lambda(None);
        self.prelude()?;
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

    /// Expands the prelude, whose forms carry only the core scope, so the
    /// macros it defines are bound where the core forms are.
    fn prelude(&mut self) -> Result<()> {
        let read = vali_syntax::read(PRELUDE, SourceId(u32::MAX));
        if let Some(e) = read.errors.first() {
            return Err(CompileError {
                message: format!("the prelude does not read: {}", e.kind),
                span: None,
            });
        }
        let forms = read
            .datums
            .iter()
            .map(|d| Syntax::from_datum(d, self.core))
            .collect();
        self.body(
            forms,
            true,
            Span {
                source: SourceId(u32::MAX),
                range: Default::default(),
            },
        )?;
        Ok(())
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

    /// The keyword `stx` uses, a core form or a macro, if it is a list
    /// whose head names one.
    fn form(&mut self, stx: &Syntax) -> Result<Option<(Binding, Vec<Syntax>)>> {
        if !stx.is_list() {
            return Ok(None);
        }
        let Exposed::List { items, tail } = self.expose(stx)? else {
            return Ok(None);
        };
        match items.first() {
            Some(head) if head.as_identifier().is_some() => match self.resolve(head)? {
                Some(Binding::Core(form)) if tail.is_none() => {
                    Ok(Some((Binding::Core(form), items)))
                }
                Some(Binding::Macro(m)) => Ok(Some((Binding::Macro(m), items))),
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// The core form `stx` is, if it is a list whose head names one.
    fn core_form(&mut self, stx: &Syntax) -> Result<Option<(Core, Vec<Syntax>)>> {
        Ok(match self.form(stx)? {
            Some((Binding::Core(form), items)) => Some((form, items)),
            _ => None,
        })
    }

    /// `id` without the use-site scopes of the body defining it.
    fn strip(&mut self, id: &Syntax, use_sites: &[Scope]) -> Result<Syntax> {
        let mut id = id.clone();
        for &scope in use_sites {
            id = id.remove(&mut self.context, scope).map_err(too_many)?;
        }
        Ok(id)
    }

    // Macros.

    /// Compiles a transformer, `(syntax-rules ...)`, for the macro `name`.
    fn transformer(&mut self, spec: &Syntax, name: &str) -> Result<usize> {
        let Some((Core::SyntaxRules, items)) = self.core_form(spec)? else {
            return Err(error(
                format!("{name}: expected a syntax-rules transformer"),
                spec.span(),
            ));
        };
        // `(syntax-rules (literal ...) clause ...)`, or with a custom
        // ellipsis, `(syntax-rules ellipsis (literal ...) clause ...)`.
        let (ellipsis, rest) = match &items[1..] {
            [e, rest @ ..] if e.as_identifier().is_some() => {
                (e.as_identifier().expect("checked").to_owned(), rest)
            }
            rest => ("...".to_owned(), rest),
        };
        let Some((literals, clauses)) = rest.split_first() else {
            return Err(error(
                "syntax-rules: expected a list of literals",
                spec.span(),
            ));
        };
        let literals = self.items(literals, "a list of literals")?;
        if let Some(l) = literals.iter().find(|l| l.as_identifier().is_none()) {
            return Err(error(
                "syntax-rules: a literal must be an identifier",
                l.span(),
            ));
        }
        let spelling = Spelling {
            ellipsis: &ellipsis,
            literals: &literals,
        };
        let mut compiled = Vec::new();
        for clause in clauses {
            let parts = self.items(clause, "(pattern template)")?;
            let [pattern, template] = &parts[..] else {
                return Err(error(
                    "syntax-rules: expected (pattern template)",
                    clause.span(),
                ));
            };
            // The pattern's first element stands for the keyword and is
            // ignored: it is not a pattern variable.
            let Exposed::List {
                items: elements,
                tail,
            } = self.expose(pattern)?
            else {
                return Err(error(
                    "syntax-rules: a pattern must be a list",
                    pattern.span(),
                ));
            };
            if elements.is_empty() {
                return Err(error(
                    "syntax-rules: a pattern must start with the keyword",
                    pattern.span(),
                ));
            }
            let rest = Syntax::list(
                elements[1..].to_vec(),
                tail,
                pattern.scopes(),
                pattern.span(),
                None,
            );
            let mut vars = Vars::new();
            let mut compiled_pattern =
                pattern::pattern(&mut self.context, &spelling, &mut vars, &rest, 0)?;
            if let Pattern::List { before, .. } = &mut compiled_pattern {
                before.insert(0, Pattern::Discard);
            }
            let template =
                pattern::template(&mut self.context, &spelling, &vars, template, 0, false)?;
            compiled.push(Clause {
                pattern: compiled_pattern,
                template,
                depths: vars.depths(),
            });
        }
        self.macros.push(Macro {
            name: name.to_owned(),
            clauses: compiled,
        });
        Ok(self.macros.len() - 1)
    }

    /// One macro step: flips a fresh macro scope on the use, adds
    /// `use_site` if the use is in a definition context, transforms it,
    /// and flips the macro scope again on the output, so what the macro
    /// introduced has the scope and what it passed through doesn't.
    /// `chain` is how many macro steps produced `form` in a body.
    fn apply_macro(
        &mut self,
        m: usize,
        form: &Syntax,
        use_site: Option<Scope>,
        chain: usize,
    ) -> Result<Syntax> {
        if self.depth + chain >= MAX_MACRO_DEPTH {
            return Err(error(
                format!(
                    "{}: too many macro steps; does the macro expand into itself forever?",
                    self.macros[m].name
                ),
                form.span(),
            ));
        }
        let scope = self.scope(ScopeKind::Macro)?;
        let mut input = form.flip(&mut self.context, scope).map_err(too_many)?;
        if let Some(use_site) = use_site {
            input = self.add(&input, use_site)?;
        }
        let origin = Arc::new(Origin {
            macro_name: self.macros[m].name.as_str().into(),
            use_site: form.span(),
            parent: form.origin().cloned(),
        });
        let table = &self.table;
        let same = |cx: &ScopeContext, a: &Syntax, b: &Syntax| {
            free_identifier_eq(cx, table, Phase(0), a, b)
        };
        let mac = &self.macros[m];
        for clause in &mac.clauses {
            let mut binds: Vec<Option<Match>> = vec![None; clause.depths.len()];
            if pattern::matches(
                &mut self.context,
                &same,
                &clause.pattern,
                &input,
                &mut binds,
            )? {
                let builder = Builder {
                    depths: clause.depths.clone(),
                    origin: Some(&origin),
                    span: form.span(),
                };
                let output = builder.build(&clause.template, &mut binds)?;
                return output.flip(&mut self.context, scope).map_err(too_many);
            }
        }
        Err(error(
            format!("{}: no pattern matches this use", mac.name),
            form.span(),
        ))
    }

    /// Expands a macro use as an expression.
    fn macro_expr(&mut self, m: usize, form: &Syntax, name: Option<&str>) -> Result<Expr> {
        let output = self.apply_macro(m, form, None, 0)?;
        self.depth += 1;
        let result = match name {
            Some(name) => self.named(&output, name),
            None => self.expr(&output),
        };
        self.depth -= 1;
        result
    }

    /// `(let-syntax ((id transformer) ...) body ...)`, and
    /// `letrec-syntax`, whose transformers see their own bindings.
    fn let_syntax(
        &mut self,
        keyword: &str,
        args: &[Syntax],
        recursive: bool,
        span: Span,
    ) -> Result<Expr> {
        let [bindings, body @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected bindings and a body"),
                span,
            ));
        };
        let bindings = self.bindings(bindings, keyword)?;
        let scope = self.scope(ScopeKind::Binder)?;
        for (id, spec) in &bindings {
            let spec = if recursive {
                self.add(spec, scope)?
            } else {
                spec.clone()
            };
            let name = id.as_identifier().expect("checked").to_owned();
            let m = self.transformer(&spec, &name)?;
            let id = self.add(id, scope)?;
            self.bind(&id, Binding::Macro(m), keyword)?;
        }
        let body = body
            .iter()
            .map(|f| self.add(f, scope))
            .collect::<Result<Vec<_>>>()?;
        self.body(body, false, span)
    }

    // Scopes and bindings.

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
        // Each form with the number of macro steps that produced it.
        let mut queue: VecDeque<(Syntax, usize)> = if top {
            forms.into_iter().map(|f| (f, 0)).collect()
        } else {
            // Definitions in a body shadow the parameters around it.
            let scope = self.scope(ScopeKind::Binder)?;
            forms
                .iter()
                .map(|f| Ok((self.add(f, scope)?, 0)))
                .collect::<Result<_>>()?
        };
        let mut items = Vec::new();
        let mut use_sites = Vec::new();
        while let Some((form, chain)) = queue.pop_front() {
            let definitions_end = !top && items.iter().any(|i| matches!(i, Item::Expr(_)));
            match self.form(&form)? {
                Some((Binding::Core(Core::Begin), parts)) => {
                    for part in parts.into_iter().skip(1).rev() {
                        queue.push_front((part, chain));
                    }
                }
                Some((Binding::Macro(m), _)) => {
                    let use_site = self.scope(ScopeKind::UseSite)?;
                    use_sites.push(use_site);
                    let output = self.apply_macro(m, &form, Some(use_site), chain)?;
                    queue.push_front((output, chain + 1));
                }
                Some((Binding::Core(Core::DefineSyntax), parts)) => {
                    if definitions_end {
                        return Err(error(
                            "define-syntax: definitions must come before a body's expressions",
                            form.span(),
                        ));
                    }
                    let [_, id, spec] = &parts[..] else {
                        return Err(error(
                            "define-syntax: expected a name and a transformer",
                            form.span(),
                        ));
                    };
                    let id = self.strip(id, &use_sites)?;
                    let name = id
                        .as_identifier()
                        .ok_or_else(|| error("define-syntax: expected a name", id.span()))?
                        .to_owned();
                    let m = self.transformer(spec, &name)?;
                    self.bind(&id, Binding::Macro(m), "define-syntax")?;
                }
                Some((Binding::Core(Core::Define), parts)) => {
                    if definitions_end {
                        return Err(error(
                            "define: definitions must come before a body's expressions",
                            form.span(),
                        ));
                    }
                    let (id, value) = self.definition(&form, &parts)?;
                    let id = self.strip(&id, &use_sites)?;
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

    /// Expands `stx`, naming it `name` if it is a `lambda`, or a macro
    /// use that expands into one.
    fn named(&mut self, stx: &Syntax, name: &str) -> Result<Expr> {
        match self.form(stx)? {
            Some((Binding::Core(Core::Lambda), parts)) => {
                self.lambda(Some(name), &parts, stx.span())
            }
            Some((Binding::Macro(m), _)) => self.macro_expr(m, stx, Some(name)),
            _ => self.expr(stx),
        }
    }

    /// Expands an expression. Deeply nested code recurses deeply, so the
    /// stack grows on demand.
    pub(crate) fn expr(&mut self, stx: &Syntax) -> Result<Expr> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.expr_here(stx))
    }

    fn expr_here(&mut self, stx: &Syntax) -> Result<Expr> {
        if let Some(name) = stx.as_identifier() {
            return match self.resolve(stx)? {
                Some(Binding::Local(v)) => Ok(self.local(v)),
                Some(Binding::Global(global)) => Ok(Expr::Global(global)),
                None => Ok(Expr::Global(name.to_owned())),
                Some(Binding::Core(_) | Binding::Macro(_)) => Err(error(
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
        if head.as_identifier().is_some() {
            match self.resolve(head)? {
                Some(Binding::Core(form)) => {
                    let keyword = head.as_identifier().expect("an identifier").to_owned();
                    return self.core(form, &keyword, &items, span);
                }
                Some(Binding::Macro(m)) => return self.macro_expr(m, stx, None),
                _ => {}
            }
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
                    Some(Binding::Core(_) | Binding::Macro(_)) => Err(error(
                        format!("{keyword}: {name} is a syntactic keyword"),
                        target.span(),
                    )),
                    None => Err(error(
                        format!("{keyword}: {name} is not defined"),
                        target.span(),
                    )),
                }
            }
            Core::Define | Core::DefineSyntax => Err(error(
                format!("{keyword} is only allowed at top level or at the start of a body"),
                span,
            )),
            Core::LetSyntax => self.let_syntax(keyword, args, false, span),
            Core::LetrecSyntax => self.let_syntax(keyword, args, true, span),
            Core::SyntaxRules => Err(error(
                format!("{keyword} is only allowed as a macro's transformer"),
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
