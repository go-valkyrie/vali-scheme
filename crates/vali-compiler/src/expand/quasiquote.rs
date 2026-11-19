//! `quasiquote` (R7RS 4.2.8), expanded in one pass over the template.
//!
//! A list template is already flat (its items and a dotted tail), so the
//! walk loops over a list's elements instead of recursing on its tail, and
//! costs time linear in the template's size. It recurses only into nested
//! lists and vectors, as deep as the template nests, on a stack that grows
//! on demand. The result builds every pair and vector it returns fresh,
//! with `list` over a run of elements (at most `RUN` to a call) and
//! `append` over the runs and the spliced lists, so no macro step is taken
//! for an element and a call's argument limit is never reached.

use super::*;

/// How many elements one `list` call takes.
const RUN: usize = 1000;
/// How many atoms one quoted list takes. A constant list is built
/// recursively when a module loads, so one of tens of thousands of
/// elements would overflow the stack there.
const QUOTED_RUN: usize = 4096;
/// How many arguments one `append` call takes: well inside a frame.
const APPEND_ARGS: usize = 1000;

fn symbol(name: &str, span: Span) -> Expr {
    Expr::Const(Datum {
        kind: DatumKind::Symbol(name.to_owned()),
        span,
    })
}

fn call(name: &str, args: Vec<Expr>, span: Span) -> Expr {
    Expr::Call(Box::new(Expr::Free(name.to_owned())), args, span)
}

/// Appends `parts`, the last of which may be any object, in as many
/// `append` calls as it takes to stay under the argument limit.
fn append(mut parts: Vec<Expr>, span: Span) -> Expr {
    while parts.len() > APPEND_ARGS {
        let mut grouped = Vec::with_capacity(parts.len() / APPEND_ARGS + 1);
        let mut rest = parts.into_iter().peekable();
        while rest.peek().is_some() {
            let group: Vec<Expr> = rest.by_ref().take(APPEND_ARGS).collect();
            grouped.push(call("append", group, span));
        }
        parts = grouped;
    }
    if parts.len() == 1 {
        parts.pop().expect("one")
    } else {
        call("append", parts, span)
    }
}

/// Ends a run of constant atoms: a quoted list, which `append` copies
/// when something follows it.
fn flush_atoms(atoms: &mut Vec<Datum>, parts: &mut Vec<Expr>, span: Span) {
    let mut atoms = std::mem::take(atoms).into_iter().peekable();
    while atoms.peek().is_some() {
        parts.push(Expr::Const(Datum {
            kind: DatumKind::List {
                items: atoms.by_ref().take(QUOTED_RUN).collect(),
                tail: None,
            },
            span,
        }));
    }
}

/// Ends a run of other elements: `list` calls of at most `RUN`.
fn flush_exprs(run: &mut Vec<Expr>, parts: &mut Vec<Expr>, span: Span) {
    let mut elements = std::mem::take(run).into_iter().peekable();
    while elements.peek().is_some() {
        let chunk: Vec<Expr> = elements.by_ref().take(RUN).collect();
        parts.push(call("list", chunk, span));
    }
}

impl Expander {
    /// Whether `stx` is a symbol or a self-evaluating atom, which
    /// quasiquote quotes whatever the nesting.
    fn qq_atom(&self, stx: &Syntax) -> bool {
        stx.as_identifier().is_some()
            || stx
                .as_atom()
                .is_some_and(|a| !matches!(a, DatumKind::Labelled(..) | DatumKind::LabelRef(_)))
    }

    /// `(quasiquote template)`.
    pub(super) fn quasiquote(
        &mut self,
        keyword: &str,
        args: &[Syntax],
        span: Span,
    ) -> Result<Expr> {
        let [template] = args else {
            return Err(error(format!("{keyword}: expected one template"), span));
        };
        self.qq(template, 0)
    }

    /// Which of `unquote`, `unquote-splicing` and `quasiquote` `stx` is,
    /// by binding.
    fn qq_keyword(&mut self, stx: &Syntax) -> Result<Option<Core>> {
        if stx.as_identifier().is_none() {
            return Ok(None);
        }
        Ok(match self.resolve(stx)? {
            Some(Binding::Core(c @ (Core::Quasiquote | Core::Unquote | Core::UnquoteSplicing))) => {
                Some(c)
            }
            _ => None,
        })
    }

    /// The expression that builds `stx` at nesting `depth`, the number of
    /// quasiquotes enclosing it beyond the outermost.
    fn qq(&mut self, stx: &Syntax, depth: usize) -> Result<Expr> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.qq_here(stx, depth))
    }

    fn qq_here(&mut self, stx: &Syntax, depth: usize) -> Result<Expr> {
        let span = stx.span();
        match self.expose(stx)? {
            Exposed::Vector(items) => {
                let list = self.qq_list(&items, None, depth, span)?;
                Ok(call("list->vector", vec![list], span))
            }
            Exposed::List { items, tail } => self.qq_list(&items, tail.as_ref(), depth, span),
            _ => Ok(Expr::Const(stx.to_datum())),
        }
    }

    /// The expression for `(items ... . tail)`.
    fn qq_list(
        &mut self,
        items: &[Syntax],
        tail: Option<&Syntax>,
        depth: usize,
        span: Span,
    ) -> Result<Expr> {
        if items.is_empty() && tail.is_none() {
            return Ok(Expr::Const(Datum {
                kind: DatumKind::List {
                    items: Vec::new(),
                    tail: None,
                },
                span,
            }));
        }
        // The lists to append, and the run of elements not yet in one:
        // either constant atoms, quoted as one list that `append` copies,
        // or the expressions for other elements, a `list` call's
        // arguments. A run of atoms is one constant however long it is,
        // as a module holds only so many.
        let mut parts: Vec<Expr> = Vec::new();
        let mut atoms: Vec<Datum> = Vec::new();
        let mut run: Vec<Expr> = Vec::new();
        // Whether the last part is one `append` would share instead of
        // copying: a spliced list or a quoted run of atoms.
        let mut shared = false;
        let mut tail_expr = None;
        let mut i = 0;
        while i < items.len() {
            let item = &items[i];
            let keyword = self.qq_keyword(item)?;
            // `(unquote x)` or `(quasiquote x)` is the rest of this list,
            // whether it is the whole list or follows a dot: `(a . ,x)`.
            if let Some(kw @ (Core::Unquote | Core::Quasiquote)) = keyword
                && i + 2 == items.len()
                && tail.is_none()
            {
                let operand = &items[i + 1];
                tail_expr = Some(match (kw, depth) {
                    (Core::Unquote, 0) => self.expr(operand)?,
                    (Core::Unquote, _) => {
                        let inner = self.qq(operand, depth - 1)?;
                        call("list", vec![symbol("unquote", span), inner], span)
                    }
                    _ => {
                        let inner = self.qq(operand, depth + 1)?;
                        call("list", vec![symbol("quasiquote", span), inner], span)
                    }
                });
                break;
            }
            // `(unquote-splicing x)` as an element.
            let splice = self.qq_splice(item)?;
            if let (Some(operand), true) = (&splice, depth == 0) {
                flush_atoms(&mut atoms, &mut parts, span);
                flush_exprs(&mut run, &mut parts, span);
                parts.push(self.expr(operand)?);
                shared = true;
            } else if let Some(operand) = &splice {
                // Inside a nested quasiquote it is data, and its operand
                // is one level closer to being evaluated.
                flush_atoms(&mut atoms, &mut parts, span);
                let inner = self.qq(operand, depth - 1)?;
                run.push(call(
                    "list",
                    vec![symbol("unquote-splicing", span), inner],
                    span,
                ));
                shared = false;
            } else if self.qq_atom(item) {
                flush_exprs(&mut run, &mut parts, span);
                atoms.push(item.to_datum());
                shared = true;
            } else {
                flush_atoms(&mut atoms, &mut parts, span);
                run.push(self.qq(item, depth)?);
                shared = false;
            }
            i += 1;
        }
        flush_atoms(&mut atoms, &mut parts, span);
        flush_exprs(&mut run, &mut parts, span);
        match (tail, tail_expr) {
            (_, Some(t)) => parts.push(t),
            (Some(t), None) => parts.push(self.qq(t, depth)?),
            (None, None) if shared => parts.push(Expr::Const(Datum {
                kind: DatumKind::List {
                    items: Vec::new(),
                    tail: None,
                },
                span,
            })),
            (None, None) => {}
        }
        Ok(append(parts, span))
    }

    /// `x` if `item` is exactly `(unquote-splicing x)`.
    fn qq_splice(&mut self, item: &Syntax) -> Result<Option<Syntax>> {
        if !item.is_list() {
            return Ok(None);
        }
        let Exposed::List { items, tail: None } = self.expose(item)? else {
            return Ok(None);
        };
        match items.as_slice() {
            [head, operand] if self.qq_keyword(head)? == Some(Core::UnquoteSplicing) => {
                Ok(Some(operand.clone()))
            }
            _ => Ok(None),
        }
    }
}
