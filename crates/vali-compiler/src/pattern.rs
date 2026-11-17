//! Patterns and templates (R7RS 4.3.2), matched and built in Rust
//! (ADR 0015 #14). `syntax-rules` uses them now; `syntax-case` and
//! `syntax` will use the same.
//!
//! A pattern variable followed by ellipses in its pattern has that many
//! levels of depth. In a template, its own levels are consumed by the
//! innermost ellipses around it; any further ellipses repeat it whole, as
//! Chez Scheme and Racket allow.

use std::sync::Arc;

use vali_syntax::{Exposed, Origin, ScopeContext, Span, Syntax, bound_identifier_eq};

use crate::CompileError;

type Result<T> = std::result::Result<T, CompileError>;

fn error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError {
        message: message.into(),
        span: Some(span),
    }
}

fn too_many(e: vali_syntax::TooManyScopes) -> CompileError {
    CompileError {
        message: e.to_string(),
        span: None,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Pattern {
    /// `_`: matches anything.
    Discard,
    /// A pattern variable, by number.
    Var(usize),
    /// A literal identifier: matches an identifier that is
    /// `free-identifier=?` to it.
    Literal(Syntax),
    /// Any other datum: matches an equal datum.
    Datum(String),
    /// `(p ... q ellipsis r ... . s)`.
    List {
        before: Vec<Pattern>,
        repeat: Option<Repeat>,
        tail: Option<Box<Pattern>>,
    },
    /// `#(p ... q ellipsis r ...)`.
    Vector {
        before: Vec<Pattern>,
        repeat: Option<Repeat>,
    },
}

/// The part of a list pattern from the ellipsis on.
#[derive(Clone, Debug)]
pub(crate) struct Repeat {
    each: Box<Pattern>,
    /// The variables `each` binds.
    vars: Vec<usize>,
    after: Vec<Pattern>,
}

/// What a pattern variable matched: one form, or at depth `n`, a list of
/// matches at depth `n - 1`.
#[derive(Clone, Debug)]
pub(crate) enum Match {
    One(Syntax),
    Many(Vec<Match>),
}

#[derive(Clone, Debug)]
pub(crate) enum Template {
    /// A pattern variable, by number.
    Var(usize),
    /// An identifier or datum, copied.
    Constant(Syntax),
    /// A list; each element with the ellipses after it.
    List {
        items: Vec<(Template, usize)>,
        tail: Option<Box<Template>>,
        /// The template list itself, for its scopes and span.
        syntax: Syntax,
    },
    Vector {
        items: Vec<(Template, usize)>,
        syntax: Syntax,
    },
}

/// The pattern variables of one pattern, as they are compiled.
pub(crate) struct Vars {
    /// Each variable's identifier and depth.
    vars: Vec<(Syntax, usize)>,
}

impl Vars {
    pub(crate) fn new() -> Vars {
        Vars { vars: Vec::new() }
    }

    pub(crate) fn len(&self) -> usize {
        self.vars.len()
    }

    /// Each variable's depth, by number.
    pub(crate) fn depths(&self) -> Vec<usize> {
        self.vars.iter().map(|(_, d)| *d).collect()
    }

    fn find(&self, id: &Syntax) -> Option<usize> {
        self.vars
            .iter()
            .position(|(v, _)| bound_identifier_eq(v, id))
    }

    /// A variable's number and depth, found by `bound-identifier=?`, as
    /// `syntax-rules` finds them.
    pub(crate) fn lookup(&self, id: &Syntax) -> Option<(usize, usize)> {
        self.find(id).map(|v| (v, self.vars[v].1))
    }

    /// Each variable's identifier and depth, by number.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &(Syntax, usize)> {
        self.vars.iter()
    }
}

/// How a `syntax-rules` form spells its ellipsis and literals.
pub(crate) struct Spelling<'a> {
    pub(crate) ellipsis: &'a str,
    pub(crate) literals: &'a [Syntax],
}

impl Spelling<'_> {
    fn is_ellipsis(&self, stx: &Syntax) -> bool {
        stx.as_identifier() == Some(self.ellipsis)
            && !self.literals.iter().any(|l| bound_identifier_eq(l, stx))
    }

    fn literal(&self, stx: &Syntax) -> Option<&Syntax> {
        self.literals.iter().find(|l| bound_identifier_eq(l, stx))
    }
}

/// Compiles `stx` as a pattern at `depth` ellipses.
pub(crate) fn pattern(
    cx: &mut ScopeContext,
    spelling: &Spelling<'_>,
    vars: &mut Vars,
    stx: &Syntax,
    depth: usize,
) -> Result<Pattern> {
    if stx.as_identifier().is_some() {
        if let Some(literal) = spelling.literal(stx) {
            return Ok(Pattern::Literal(literal.clone()));
        }
        if spelling.is_ellipsis(stx) {
            return Err(error("an ellipsis must follow a pattern", stx.span()));
        }
        if stx.as_identifier() == Some("_") {
            return Ok(Pattern::Discard);
        }
        if vars.find(stx).is_some() {
            return Err(error(
                format!(
                    "{} appears twice in one pattern",
                    stx.as_identifier().unwrap_or_default()
                ),
                stx.span(),
            ));
        }
        vars.vars.push((stx.clone(), depth));
        return Ok(Pattern::Var(vars.vars.len() - 1));
    }
    match stx.expose(cx).map_err(too_many)? {
        Exposed::List { items, tail } => {
            let (before, repeat) = sequence(cx, spelling, vars, &items, depth)?;
            let tail = tail
                .map(|t| pattern(cx, spelling, vars, &t, depth).map(Box::new))
                .transpose()?;
            Ok(Pattern::List {
                before,
                repeat,
                tail,
            })
        }
        Exposed::Vector(items) => {
            let (before, repeat) = sequence(cx, spelling, vars, &items, depth)?;
            Ok(Pattern::Vector { before, repeat })
        }
        Exposed::Atom(_) | Exposed::Identifier(_) => Ok(Pattern::Datum(stx.to_datum().to_string())),
    }
}

/// The elements of a list or vector pattern, with at most one ellipsis.
fn sequence(
    cx: &mut ScopeContext,
    spelling: &Spelling<'_>,
    vars: &mut Vars,
    items: &[Syntax],
    depth: usize,
) -> Result<(Vec<Pattern>, Option<Repeat>)> {
    let ellipses: Vec<usize> = (0..items.len())
        .filter(|&i| spelling.is_ellipsis(&items[i]))
        .collect();
    match ellipses[..] {
        [] => Ok((
            items
                .iter()
                .map(|p| pattern(cx, spelling, vars, p, depth))
                .collect::<Result<_>>()?,
            None,
        )),
        [0] => Err(error("an ellipsis must follow a pattern", items[0].span())),
        [at] => {
            let before = items[..at - 1]
                .iter()
                .map(|p| pattern(cx, spelling, vars, p, depth))
                .collect::<Result<_>>()?;
            let first = vars.len();
            let each = pattern(cx, spelling, vars, &items[at - 1], depth + 1)?;
            let each_vars = (first..vars.len()).collect();
            let after = items[at + 1..]
                .iter()
                .map(|p| pattern(cx, spelling, vars, p, depth))
                .collect::<Result<_>>()?;
            Ok((
                before,
                Some(Repeat {
                    each: Box::new(each),
                    vars: each_vars,
                    after,
                }),
            ))
        }
        [_, second, ..] => Err(error(
            "a list pattern can have only one ellipsis",
            items[second].span(),
        )),
    }
}

/// Whether two identifiers mean the same: `free-identifier=?`.
pub(crate) type Same<'a> = &'a dyn Fn(&ScopeContext, &Syntax, &Syntax) -> bool;

/// Matches `stx` against `pattern`, recording what each variable matched.
pub(crate) fn matches(
    cx: &mut ScopeContext,
    same: Same<'_>,
    pattern: &Pattern,
    stx: &Syntax,
    binds: &mut [Option<Match>],
) -> Result<bool> {
    match pattern {
        Pattern::Discard => Ok(true),
        Pattern::Var(v) => {
            binds[*v] = Some(Match::One(stx.clone()));
            Ok(true)
        }
        Pattern::Literal(literal) => Ok(stx.as_identifier().is_some() && same(cx, literal, stx)),
        Pattern::Datum(datum) => {
            Ok(stx.as_atom().is_some() && stx.to_datum().to_string() == *datum)
        }
        Pattern::List {
            before,
            repeat,
            tail,
        } => {
            // Check the shape before taking the list apart, so a clause
            // that cannot match costs nothing.
            let Some((length, dotted)) = stx.list_shape() else {
                return Ok(false);
            };
            let fixed = before.len() + repeat.as_ref().map_or(0, |r| r.after.len());
            if length < fixed {
                return Ok(false);
            }
            // Without an ellipsis or a tail pattern, the lengths must agree.
            let used = if repeat.is_some() {
                length
            } else {
                before.len()
            };
            if tail.is_none() && (used != length || dotted) {
                return Ok(false);
            }
            let Exposed::List { items, tail: rest } = stx.expose(cx).map_err(too_many)? else {
                return Ok(false);
            };
            if !elements(cx, same, before, repeat.as_ref(), &items[..used], binds)? {
                return Ok(false);
            }
            match tail {
                None => Ok(true),
                Some(tail) => {
                    // The rest of the list: what the elements didn't use,
                    // then the input's own tail.
                    let remaining = items[used..].to_vec();
                    let rest = if remaining.is_empty() {
                        rest.unwrap_or_else(|| {
                            Syntax::list(Vec::new(), None, stx.scopes(), stx.span(), None)
                        })
                    } else {
                        Syntax::list(remaining, rest, stx.scopes(), stx.span(), None)
                    };
                    matches(cx, same, tail, &rest, binds)
                }
            }
        }
        Pattern::Vector { before, repeat } => {
            let Exposed::Vector(items) = stx.expose(cx).map_err(too_many)? else {
                return Ok(false);
            };
            if repeat.is_none() && items.len() != before.len() {
                return Ok(false);
            }
            elements(cx, same, before, repeat.as_ref(), &items, binds)
        }
    }
}

/// Matches `items` against `before`, then the repeated pattern as many
/// times as fits, then the patterns after it.
fn elements(
    cx: &mut ScopeContext,
    same: Same<'_>,
    before: &[Pattern],
    repeat: Option<&Repeat>,
    items: &[Syntax],
    binds: &mut [Option<Match>],
) -> Result<bool> {
    let after = repeat.map_or(0, |r| r.after.len());
    if items.len() < before.len() + after {
        return Ok(false);
    }
    for (p, item) in before.iter().zip(items) {
        if !matches(cx, same, p, item, binds)? {
            return Ok(false);
        }
    }
    let Some(repeat) = repeat else {
        return Ok(true);
    };
    let middle = &items[before.len()..items.len() - after];
    let mut collected: Vec<Vec<Match>> = vec![Vec::new(); repeat.vars.len()];
    for item in middle {
        let mut each = vec![None; binds.len()];
        if !matches(cx, same, &repeat.each, item, &mut each)? {
            return Ok(false);
        }
        for (slot, &v) in collected.iter_mut().zip(&repeat.vars) {
            slot.push(each[v].take().expect("the repeated pattern binds it"));
        }
    }
    for (slot, &v) in collected.into_iter().zip(&repeat.vars) {
        binds[v] = Some(Match::Many(slot));
    }
    for (p, item) in repeat.after.iter().zip(&items[items.len() - after..]) {
        if !matches(cx, same, p, item, binds)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Finds a template's pattern variables: given an identifier, its number
/// and its depth in its pattern, if it is one.
pub(crate) type Find<'a> = &'a mut dyn FnMut(&ScopeContext, &Syntax) -> Option<(usize, usize)>;

/// Compiles `stx` as a template, inside `depth` ellipses. With `escaped`,
/// the ellipsis is an ordinary identifier: `(... template)`.
pub(crate) fn template(
    cx: &mut ScopeContext,
    spelling: &Spelling<'_>,
    find: Find<'_>,
    stx: &Syntax,
    depth: usize,
    escaped: bool,
) -> Result<Template> {
    if stx.as_identifier().is_some() {
        if let Some((v, needed)) = find(cx, stx) {
            if depth < needed {
                return Err(error(
                    format!(
                        "{} needs {needed} ellipses after it in the template",
                        stx.as_identifier().unwrap_or_default()
                    ),
                    stx.span(),
                ));
            }
            return Ok(Template::Var(v));
        }
        if !escaped && spelling.is_ellipsis(stx) {
            return Err(error("an ellipsis must follow a template", stx.span()));
        }
        return Ok(Template::Constant(stx.clone()));
    }
    match stx.expose(cx).map_err(too_many)? {
        Exposed::List { items, tail } => {
            // `(... template)`: the template, with ellipses as identifiers.
            if !escaped
                && tail.is_none()
                && let [first, inner] = &items[..]
                && spelling.is_ellipsis(first)
            {
                return template(cx, spelling, find, inner, depth, true);
            }
            let items = elements_template(cx, spelling, find, &items, depth, escaped)?;
            let tail = tail
                .map(|t| template(cx, spelling, find, &t, depth, escaped).map(Box::new))
                .transpose()?;
            Ok(Template::List {
                items,
                tail,
                syntax: stx.clone(),
            })
        }
        Exposed::Vector(items) => Ok(Template::Vector {
            items: elements_template(cx, spelling, find, &items, depth, escaped)?,
            syntax: stx.clone(),
        }),
        Exposed::Atom(_) | Exposed::Identifier(_) => Ok(Template::Constant(stx.clone())),
    }
}

fn elements_template(
    cx: &mut ScopeContext,
    spelling: &Spelling<'_>,
    find: Find<'_>,
    items: &[Syntax],
    depth: usize,
    escaped: bool,
) -> Result<Vec<(Template, usize)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < items.len() {
        if !escaped && spelling.is_ellipsis(&items[i]) {
            return Err(error("an ellipsis must follow a template", items[i].span()));
        }
        let mut count = 0;
        while !escaped && i + 1 + count < items.len() && spelling.is_ellipsis(&items[i + 1 + count])
        {
            count += 1;
        }
        out.push((
            template(cx, spelling, find, &items[i], depth + count, escaped)?,
            count,
        ));
        i += 1 + count;
    }
    Ok(out)
}

/// Builds a template's output.
pub(crate) struct Builder<'a> {
    /// Each variable's depth in its pattern.
    pub(crate) depths: Vec<usize>,
    /// The macro use being expanded, given to every node the template
    /// introduces.
    pub(crate) origin: Option<&'a Arc<Origin>>,
    /// Where the macro was used, for errors.
    pub(crate) span: Span,
}

impl Builder<'_> {
    /// Builds `t`. Repeating a part rebinds the variables it repeats over
    /// in `binds` while it runs, and restores them after.
    pub(crate) fn build(&self, t: &Template, binds: &mut [Option<Match>]) -> Result<Syntax> {
        let origin = self.origin.cloned();
        match t {
            Template::Var(v) => match &binds[*v] {
                Some(Match::One(s)) => Ok(s.clone()),
                _ => Err(error(
                    "a pattern variable is used at the wrong ellipsis depth",
                    self.span,
                )),
            },
            Template::Constant(s) => Ok(s.with_origin(origin)),
            Template::List {
                items,
                tail,
                syntax,
            } => {
                let items = self.items(items, binds)?;
                let tail = tail.as_ref().map(|t| self.build(t, binds)).transpose()?;
                Ok(Syntax::list(
                    items,
                    tail,
                    syntax.scopes(),
                    syntax.span(),
                    origin,
                ))
            }
            Template::Vector { items, syntax } => Ok(Syntax::vector(
                self.items(items, binds)?,
                syntax.scopes(),
                syntax.span(),
                origin,
            )),
        }
    }

    fn items(
        &self,
        items: &[(Template, usize)],
        binds: &mut [Option<Match>],
    ) -> Result<Vec<Syntax>> {
        let mut out = Vec::new();
        for (t, count) in items {
            if *count == 0 {
                out.push(self.build(t, binds)?);
            } else {
                self.repeat(t, *count, binds, &mut out)?;
            }
        }
        Ok(out)
    }

    /// `t` followed by `count` ellipses: one copy for each match of the
    /// variables the outermost of those ellipses consumes.
    fn repeat(
        &self,
        t: &Template,
        count: usize,
        binds: &mut [Option<Match>],
        out: &mut Vec<Syntax>,
    ) -> Result<()> {
        let mut drivers = Vec::new();
        self.drivers(t, count, 0, &mut drivers);
        let mut length = None;
        for &v in &drivers {
            let Some(Match::Many(matches)) = &binds[v] else {
                continue;
            };
            match length {
                None => length = Some(matches.len()),
                Some(n) if n != matches.len() => {
                    return Err(error(
                        "pattern variables under one ellipsis matched different numbers of forms",
                        self.span,
                    ));
                }
                Some(_) => {}
            }
        }
        let Some(length) = length else {
            return Err(error(
                "an ellipsis in the template follows no pattern variable that repeats",
                self.span,
            ));
        };
        // Take each driver's matches out, bind one at a time, and put
        // them back: copying all the bindings for every repetition would
        // make a long repetition quadratic.
        let mut lists = Vec::new();
        for &v in &drivers {
            if let Some(Match::Many(matches)) = &mut binds[v] {
                lists.push((v, std::mem::take(matches)));
            }
        }
        for j in 0..length {
            for (v, matches) in &lists {
                binds[*v] = Some(matches[j].clone());
            }
            if count == 1 {
                out.push(self.build(t, binds)?);
            } else {
                self.repeat(t, count - 1, binds, out)?;
            }
        }
        for (v, matches) in lists {
            binds[v] = Some(Match::Many(matches));
        }
        Ok(())
    }

    /// The variables in `t` that the outermost of the `count` ellipses
    /// after it consumes. The innermost `p` ellipses around a variable
    /// with depth `p` in its pattern consume it, so one with `inner`
    /// ellipses between it and these is consumed when `inner + count <= p`.
    /// Variables outside that reach are repeated whole.
    fn drivers(&self, t: &Template, count: usize, inner: usize, out: &mut Vec<usize>) {
        match t {
            Template::Var(v) => {
                if inner + count <= self.depths[*v] && !out.contains(v) {
                    out.push(*v);
                }
            }
            Template::Constant(_) => {}
            Template::List { items, tail, .. } => {
                for (item, more) in items {
                    self.drivers(item, count, inner + more, out);
                }
                if let Some(t) = tail {
                    self.drivers(t, count, inner, out);
                }
            }
            Template::Vector { items, .. } => {
                for (item, more) in items {
                    self.drivers(item, count, inner + more, out);
                }
            }
        }
    }
}
