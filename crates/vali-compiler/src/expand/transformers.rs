//! Macros: `syntax-rules` transformers and those written in Scheme, the
//! isolates the latter run in, and `syntax-case` and templates.

use super::*;

#[derive(Clone)]
pub(super) enum Transformer {
    /// `syntax-rules`, run in Rust.
    Rules(Vec<Clause>),
    /// A procedure written in Scheme.
    Procedure(Procedure),
}

/// A transformer written in Scheme: compiled code, which each unit's
/// expansion runs afresh in its own isolate for the phase the transformer
/// runs at, so no expansion sees what another left behind (ADR 0001 #2).
#[derive(Clone)]
pub(super) struct Procedure {
    /// The module whose body evaluates to the procedure, as distribution
    /// code: what a library's artifact holds for it (ADR 0012 #5).
    pub(super) ir: Arc<vali_dist::Module>,
    /// It converted to run format, once something makes it; shared by
    /// every copy of the macro.
    pub(super) code: Arc<std::sync::OnceLock<Module>>,
    /// The isolate it runs in: 0 for phase 1, 1 for phase 2, and so on.
    pub(super) phase: usize,
    /// The procedure, once made in the current unit's isolate, with the
    /// unit's generation.
    pub(super) made: Option<(u64, Handle)>,
}

/// An isolate expansion-time code runs in, for one unit and one phase.
pub(super) struct PhaseIsolate {
    pub(super) cx: Cx<DeferredHeadered>,
    /// The units instantiated in it, each a fresh instance.
    pub(super) units: HashSet<usize>,
}

#[derive(Clone)]
pub(super) struct Clause {
    pub(super) pattern: Pattern,
    pub(super) template: Template,
    /// Each pattern variable's depth.
    pub(super) depths: Vec<usize>,
}

impl Expander {
    // Macros.

    /// Compiles a transformer, `(syntax-rules ...)`, for the macro `name`.
    pub(super) fn transformer(&mut self, spec: &Syntax, name: &str) -> Result<MacroId> {
        let Some((Core::SyntaxRules, items)) = self.core_form(spec)? else {
            let procedure = self.procedure(spec, name)?;
            let m = self.add_macro(Macro {
                name: name.to_owned(),
                transformer: Transformer::Procedure(procedure),
            });
            // Made now, so a transformer that fails is an error here.
            self.made_transformer(m, spec.span())?;
            return Ok(m);
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
            let mut find = |_: &ScopeContext, id: &Syntax| vars.lookup(id);
            let template =
                pattern::template(&mut self.context, &spelling, &mut find, template, 0, false)?;
            compiled.push(Clause {
                pattern: compiled_pattern,
                template,
                depths: vars.depths(),
            });
        }
        Ok(self.add_macro(Macro {
            name: name.to_owned(),
            transformer: Transformer::Rules(compiled),
        }))
    }

    /// Expands and compiles a transformer written in Scheme, which runs at
    /// the phase above the code it is defined in.
    pub(super) fn procedure(&mut self, spec: &Syntax, name: &str) -> Result<Procedure> {
        let phase = self.floors.len();
        let body = self.open_lambda(Some(name));
        self.floors.push(self.stack.len() - 1);
        let expr = self.named(spec, name);
        self.floors.pop();
        self.close_lambda(body, Vec::new(), None, expr?);
        Ok(Procedure {
            ir: Arc::new(self.lowered(body, name)?),
            code: Arc::default(),
            phase,
            made: None,
        })
    }

    /// The procedure of the macro `m`, a transformer written in Scheme,
    /// made in the current unit's isolate for its phase unless it was
    /// already: the libraries it uses are instantiated there afresh, then
    /// its module runs.
    pub(super) fn made_transformer(&mut self, m: MacroId, span: Span) -> Result<Handle> {
        let Transformer::Procedure(procedure) = &self.macros[&m].transformer else {
            unreachable!("only procedures are made");
        };
        if let Some((generation, made)) = &procedure.made
            && *generation == self.generation
        {
            return Ok(made.clone());
        }
        let module = match procedure.code.get() {
            Some(module) => module.clone(),
            None => {
                let module = converted(&procedure.ir)?;
                let _ = procedure.code.set(module.clone());
                module
            }
        };
        let phase = procedure.phase;
        let name = self.macros[&m].name.clone();
        let failed = |e: vali_runtime::Error| error(format!("{name}: {e}"), span);
        for variable in &module.variables {
            if let Link::Imported(identity) = &variable.link
                && let Some(unit) = self.units.iter().position(|u| *u.identity == **identity)
            {
                self.instantiate(unit, phase).map_err(failed)?;
            }
        }
        let cx = self.phase_isolate(phase)?;
        let id = cx.load(module).map_err(failed)?;
        let made = cx.run(id).map_err(failed)?;
        if cx.get(&made).map_err(failed)?.tag() != vali_abi::Tag::Closure {
            return Err(error(
                format!("{name}: the transformer is not a procedure"),
                span,
            ));
        }
        let Transformer::Procedure(procedure) = &mut self
            .macros
            .get_mut(&m)
            .expect("a macro of this unit's context")
            .transformer
        else {
            unreachable!("as above");
        };
        procedure.made = Some((self.generation, made.clone()));
        Ok(made)
    }

    /// Loads and runs `unit` in the current unit's isolate for `phase`,
    /// after what it imports, unless it is there already: a fresh instance
    /// for this expansion and this phase (ADR 0001 #2).
    pub(super) fn instantiate(
        &mut self,
        unit: usize,
        phase: usize,
    ) -> std::result::Result<(), vali_runtime::Error> {
        let made = |e: CompileError| vali_runtime::Error::scheme(e.message);
        self.phase_isolate(phase).map_err(made)?;
        if self.phases[phase].units.contains(&unit) {
            return Ok(());
        }
        for dependency in self.units[unit].imports.clone() {
            self.instantiate(dependency, phase)?;
        }
        let module = self.units[unit]
            .module
            .clone()
            .expect("an imported library is compiled before its importers");
        let identity = self.units[unit].identity.clone();
        let cx = self.phase_isolate(phase).map_err(made)?;
        let id = cx.load(module)?;
        cx.run(id)?;
        cx.register_library(&identity, id);
        self.phases[phase].units.insert(unit);
        Ok(())
    }

    /// The current unit's isolate for `phase`, made on first use with the
    /// syntax procedures and the program's native libraries.
    pub(super) fn phase_isolate(&mut self, phase: usize) -> Result<&mut Cx<DeferredHeadered>> {
        while self.phases.len() <= phase {
            let mut cx = phase::instance().map_err(|e| CompileError {
                message: format!("the expander's isolate: {e}"),
                span: None,
                location: None,
            })?;
            for (identity, library) in &self.natives {
                cx.register_native_library(identity, std::sync::Arc::clone(library));
            }
            self.phases.push(PhaseIsolate {
                cx,
                units: HashSet::new(),
            });
        }
        Ok(&mut self.phases[phase].cx)
    }

    /// Calls a transformer written in Scheme on `input`, lending it the
    /// expander's scopes for the syntax procedures.
    pub(super) fn run_transformer(
        &mut self,
        m: MacroId,
        phase: usize,
        procedure: &Handle,
        input: Syntax,
        form: &Syntax,
        origin: Arc<Origin>,
    ) -> Result<Syntax> {
        let name = self.macros[&m].name.clone();
        let failed = |e: vali_runtime::Error| error(format!("{name}: {e}"), form.span());
        let shared = Shared {
            core_scope: self.core_scope,
            context: std::mem::replace(&mut self.context, ScopeContext::new()),
            table: std::mem::take(&mut self.table),
            compiled: std::mem::take(&mut self.compiled),
            origin: Some(origin),
            sources: self.sources.clone(),
        };
        let cx = &mut self.phases[phase].cx;
        // Allocate first, so a failure leaves the state where it was.
        let input = match cx.syntax(input) {
            Ok(input) => input,
            Err(e) => {
                self.context = shared.context;
                self.table = shared.table;
                self.compiled = shared.compiled;
                return Err(failed(e));
            }
        };
        cx.set_expander_data(Some(Box::new(shared)));
        let result = cx.call(procedure, &[input]);
        let shared = cx
            .set_expander_data(None)
            .and_then(|data| (data as Box<dyn std::any::Any>).downcast::<Shared>().ok())
            .expect("the expander's state comes back");
        self.context = shared.context;
        self.table = shared.table;
        self.compiled = shared.compiled;
        let output = result.map_err(failed)?;
        let cx = &mut self.phases[phase].cx;
        let value = cx.get(&output).map_err(failed)?;
        // SAFETY: the handle keeps the value valid.
        unsafe { cx.value_syntax(value, ScopeSetId::EMPTY, form.span()) }.ok_or_else(|| {
            error(
                format!("{name}: the transformer returned something that is not syntax"),
                form.span(),
            )
        })
    }

    /// An error if `v`, a local variable, belongs to code outside the
    /// transformer being expanded: it doesn't exist when the transformer
    /// runs.
    pub(super) fn in_context(&self, v: VarId, id: &Syntax) -> Result<()> {
        if let Some(&floor) = self.floors.last()
            && !self.stack[floor..].contains(&self.vars[v].binder)
        {
            return Err(error(
                format!(
                    "{}: used out of context: a macro's transformer cannot use a \
                     variable of the code around it",
                    self.vars[v].name
                ),
                id.span(),
            ));
        }
        Ok(())
    }

    /// A reference to the top-level variable `name` of unit `owner`.
    pub(super) fn variable(&self, owner: usize, name: String) -> Expr {
        if owner == self.units.len() - 1 {
            Expr::Global(name)
        } else {
            Expr::Imported(self.units[owner].identity.clone(), name)
        }
    }

    /// An error if a transformer uses a variable of the unit being
    /// compiled, which runs only after the transformer has.
    pub(super) fn global_in_context(&self, owner: usize, global: &str, id: &Syntax) -> Result<()> {
        if self.floors.is_empty() || owner != self.units.len() - 1 {
            return Ok(());
        }
        Err(error(
            format!(
                "{global}: defined by the program, so it does not exist yet when \
                 a macro's transformer runs"
            ),
            id.span(),
        ))
    }

    /// One macro step: flips a fresh macro scope on the use, adds
    /// `use_site` if the use is in a definition context, transforms it,
    /// and flips the macro scope again on the output, so what the macro
    /// introduced has the scope and what it passed through doesn't.
    /// `chain` is how many macro steps produced `form` in a body.
    pub(super) fn apply_macro(
        &mut self,
        m: MacroId,
        form: &Syntax,
        use_site: Option<Scope>,
        chain: usize,
    ) -> Result<Syntax> {
        if self.depth + chain >= MAX_MACRO_DEPTH {
            return Err(error(
                format!(
                    "{}: too many macro steps; does the macro expand into itself forever?",
                    self.macros[&m].name
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
            macro_name: self.macros[&m].name.as_str().into(),
            use_site: form.span(),
            parent: form.origin().cloned(),
        });
        if let Transformer::Procedure(procedure) = &self.macros[&m].transformer {
            let phase = procedure.phase;
            let procedure = self.made_transformer(m, form.span())?;
            let output = self.run_transformer(m, phase, &procedure, input, form, origin)?;
            return output.flip(&mut self.context, scope).map_err(too_many);
        }
        let table = &self.table;
        let same = |cx: &ScopeContext, a: &Syntax, b: &Syntax| {
            free_identifier_eq(cx, table, Phase(0), a, b)
        };
        let mac = &self.macros[&m];
        let Transformer::Rules(clauses) = &mac.transformer else {
            unreachable!("procedures were handled above")
        };
        for clause in clauses {
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
    pub(super) fn macro_expr(
        &mut self,
        m: MacroId,
        form: &Syntax,
        name: Option<&str>,
    ) -> Result<Expr> {
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
    pub(super) fn let_syntax(
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

    // syntax-case.

    /// The current unit's table of compiled syntax, numbered from zero,
    /// and its name, which code passes with each number, as a constant.
    pub(super) fn unit_table(&mut self, span: Span) -> (Expr, &mut Compiled) {
        let key = &self.units.last().expect("a unit is being expanded").key;
        (
            string(key, span),
            self.compiled.entry(key.clone()).or_default(),
        )
    }

    /// The scopes, bindings and compiled syntax so far, for syntax objects
    /// at run time (ADR 0015 #10).
    pub(crate) fn syntax_data(&self) -> Shared {
        Shared {
            core_scope: self.core_scope,
            context: self.context.clone(),
            table: self.table.clone(),
            compiled: self.compiled.clone(),
            origin: None,
            sources: self.sources.clone(),
        }
    }

    /// A local variable of the current `lambda` that no identifier names.
    pub(super) fn hidden_local(&mut self, name: &str) -> VarId {
        self.vars.push(VarInfo {
            name: name.to_owned(),
            binder: self.current(),
            assigned: false,
            captured: false,
        });
        self.vars.len() - 1
    }

    /// `(syntax-case input (literal ...) clause ...)`: the input in a
    /// hidden local, then each clause in turn.
    pub(super) fn syntax_case(
        &mut self,
        keyword: &str,
        args: &[Syntax],
        span: Span,
    ) -> Result<Expr> {
        let [input, literals, clauses @ ..] = args else {
            return Err(error(
                format!("{keyword}: expected an input, a list of literals and clauses"),
                span,
            ));
        };
        let literals = self.items(literals, "a list of literals")?;
        if let Some(l) = literals.iter().find(|l| l.as_identifier().is_none()) {
            return Err(error(
                format!("{keyword}: a literal must be an identifier"),
                l.span(),
            ));
        }
        let input = self.expr(input)?;
        let tmp = self.hidden_local(keyword);
        let clauses = self.clauses(keyword, tmp, &literals, clauses, span)?;
        Ok(Expr::Let(vec![(tmp, input)], Box::new(clauses)))
    }

    /// The first of `clauses`, falling through to the rest. A clause binds
    /// its pattern variables, in a scope its fender and output have, to
    /// hidden locals holding what each matched. With a fender, the rest is
    /// a procedure, called if either the pattern or the fender fails, so it
    /// is compiled once.
    pub(super) fn clauses(
        &mut self,
        keyword: &str,
        tmp: VarId,
        literals: &[Syntax],
        clauses: &[Syntax],
        span: Span,
    ) -> Result<Expr> {
        let Some((clause, rest)) = clauses.split_first() else {
            // No clause matched.
            let input = self.local(tmp);
            return Ok(Expr::Call(
                Box::new(Expr::Free("syntax-violation".to_owned())),
                vec![
                    Expr::Const(Datum {
                        kind: DatumKind::Boolean(false),
                        span,
                    }),
                    Expr::Const(Datum {
                        kind: DatumKind::String("invalid syntax".to_owned()),
                        span,
                    }),
                    input,
                ],
                span,
            ));
        };
        let parts = self.items(clause, "(pattern [fender] output)")?;
        let (pattern, fender, output) = match &parts[..] {
            [pattern, output] => (pattern, None, output),
            [pattern, fender, output] => (pattern, Some(fender), output),
            _ => {
                return Err(error(
                    format!("{keyword}: expected (pattern [fender] output)"),
                    clause.span(),
                ));
            }
        };
        let spelling = Spelling {
            ellipsis: "...",
            literals,
        };
        let mut vars = Vars::new();
        let compiled = pattern::pattern(&mut self.context, &spelling, &mut vars, pattern, 0)?;
        let (unit, table) = self.unit_table(span);
        table.patterns.push((compiled, vars.len()));
        let k = table.patterns.len() - 1;

        // What follows if this clause fails: the rest, inline, or with a
        // fender, a procedure that runs it.
        let (rest, fail) = if fender.is_some() {
            let thunk = self.open_lambda(None);
            let body = self.clauses(keyword, tmp, literals, rest, span)?;
            self.close_lambda(thunk, Vec::new(), None, body);
            let fail = self.hidden_local("fail");
            (None, Some((fail, Expr::Lambda(thunk))))
        } else {
            (
                Some(self.clauses(keyword, tmp, literals, rest, span)?),
                None,
            )
        };
        let otherwise = |this: &mut Self| match &fail {
            Some((f, _)) => Expr::Call(Box::new(this.local(*f)), Vec::new(), span),
            None => unreachable!("only with a fender"),
        };

        let scope = self.scope(ScopeKind::Binder)?;
        let m = self.hidden_local("match");
        let mut binds = Vec::new();
        let pattern_vars: Vec<(Syntax, usize)> = vars.iter().cloned().collect();
        for (i, (id, depth)) in pattern_vars.into_iter().enumerate() {
            let id = self.add(&id, scope)?;
            let name = id
                .as_identifier()
                .expect("pattern variables are identifiers");
            let v = self.hidden_local(name);
            self.bind(&id, Binding::PatternVar(v, depth), keyword)?;
            let element = Expr::Call(
                Box::new(Expr::Free("vector-ref".to_owned())),
                vec![Expr::Local(m), number(i, span)],
                span,
            );
            binds.push((v, element));
        }
        let output = self.add(output, scope)?;
        let output = self.expr(&output)?;
        let body = match fender {
            Some(fender) => {
                let fender = self.add(fender, scope)?;
                let fender = self.expr(&fender)?;
                Expr::If(
                    Box::new(fender),
                    Box::new(output),
                    Box::new(otherwise(self)),
                )
            }
            None => output,
        };
        let input = self.local(tmp);
        let matched = Expr::Call(
            Box::new(Expr::Free(SYNTAX_MATCH.to_owned())),
            vec![input, unit, number(k, span)],
            span,
        );
        let unmatched = match rest {
            Some(rest) => rest,
            None => otherwise(self),
        };
        let attempt = Expr::Let(
            vec![(m, matched)],
            Box::new(Expr::If(
                Box::new(Expr::Local(m)),
                Box::new(Expr::Let(binds, Box::new(body))),
                Box::new(unmatched),
            )),
        );
        Ok(match fail {
            Some((fail, thunk)) => Expr::Let(vec![(fail, thunk)], Box::new(attempt)),
            None => attempt,
        })
    }

    /// `(syntax template)`: a call building the template, with the matches
    /// of the pattern variables it uses, found by resolving its
    /// identifiers, so a template inside a nested `lambda` still sees its
    /// clause's variables.
    pub(super) fn syntax_template(
        &mut self,
        keyword: &str,
        args: &[Syntax],
        span: Span,
    ) -> Result<Expr> {
        let [form] = args else {
            return Err(error(format!("{keyword}: expected one template"), span));
        };
        let spelling = Spelling {
            ellipsis: "...",
            literals: &[],
        };
        let mut used: Vec<(VarId, usize)> = Vec::new();
        let table = &self.table;
        let mut find = |cx: &ScopeContext, id: &Syntax| match id.resolve(cx, table, Phase(0)) {
            Resolution::Bound(&Binding::PatternVar(v, depth)) => {
                let i = used.iter().position(|&(u, _)| u == v).unwrap_or_else(|| {
                    used.push((v, depth));
                    used.len() - 1
                });
                Some((i, depth))
            }
            _ => None,
        };
        let template = pattern::template(&mut self.context, &spelling, &mut find, form, 0, false)?;
        if used.len() > usize::from(u8::MAX) - 5 {
            return Err(error(
                format!("{keyword}: a template may use at most 250 pattern variables"),
                span,
            ));
        }
        let depths = used.iter().map(|&(_, d)| d).collect();
        let (unit, table) = self.unit_table(span);
        table.templates.push((template, depths));
        let mut args = vec![unit, number(table.templates.len() - 1, span)];
        for (v, _) in used {
            self.in_context(v, form)?;
            args.push(self.local(v));
        }
        Ok(Expr::Call(
            Box::new(Expr::Free(SYNTAX_BUILD.to_owned())),
            args,
            span,
        ))
    }
}
