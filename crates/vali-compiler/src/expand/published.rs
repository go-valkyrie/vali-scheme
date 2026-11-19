//! Published definitions (ADR 0012 #9): the private definitions of a
//! library that code it generates into other libraries may reference.
//!
//! A library's macros expand in the libraries that use them, and what they
//! expand to may name a definition the library does not export: a helper
//! its `guard` calls, say. The library's artifact records those definitions
//! as *published*, and the linker lets other libraries' code link a name
//! that is exported or published and no other.
//!
//! The compiler infers the set when it finishes a library: every private
//! definition that an identifier in the library's syntax export resolves
//! to, in the templates of its `syntax-rules` macros and in the compiled
//! syntax (quoted syntax and templates) its procedure transformers use.
//! An identifier resolves where the library wrote it, which is where an
//! importer's expansion resolves it too, since remapping keeps the
//! scopes' structure. A `publish` declaration adds what a walk cannot see.

use super::*;

impl Expander {
    /// Publishes the definitions of library `unit` that its `publish`
    /// declarations name, resolved where its body is.
    pub(super) fn publish(
        &mut self,
        unit: usize,
        form: &LibraryForm,
        top: ScopeSetId,
    ) -> Result<()> {
        for spec in &form.publishes {
            let id = Syntax::identifier(&spec.internal, top, spec.span, None);
            match self.resolve(&id)? {
                Some(Binding::Variable(owner, name)) if owner == unit => {
                    self.units[unit].published.insert(name);
                }
                _ => {
                    return Err(error(
                        format!(
                            "publish: {} is not a variable this library defines",
                            spec.internal
                        ),
                        spec.span,
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The variables of unit `index` that `state`, its syntax export, can
/// reference: the names of its own definitions that an identifier in one
/// of its own macros' templates, or in its compiled syntax under `key`,
/// resolves to.
pub(super) fn infer(state: &mut ContextState, index: usize, key: &str) -> Result<HashSet<String>> {
    let ContextState {
        context,
        table,
        macros,
        compiled,
        ..
    } = state;
    let mut walk = Walk {
        context,
        table,
        index,
        found: HashSet::new(),
        seen: HashSet::new(),
    };
    for (id, m) in macros.iter() {
        if id.unit as usize != index {
            continue;
        }
        if let Transformer::Rules(clauses) = &m.transformer {
            for clause in clauses {
                walk.template(&clause.template)?;
            }
        }
    }
    if let Some(compiled) = compiled.get(key) {
        for s in &compiled.quoted {
            walk.syntax(s)?;
        }
        for (t, _) in &compiled.templates {
            walk.template(t)?;
        }
    }
    Ok(walk.found)
}

struct Walk<'a> {
    context: &'a mut ScopeContext,
    table: &'a BindingTable<String, Binding>,
    index: usize,
    found: HashSet<String>,
    /// The identifiers resolved, by name and scopes.
    seen: HashSet<(String, ScopeSetId)>,
}

/// The phases an identifier in a template is resolved at: a template in
/// phase-1 code builds phase-0 code, one nested a level deeper builds
/// phase-1 code, and so on.
const PHASES: [Phase; 3] = [Phase(0), Phase(1), Phase(2)];

impl Walk<'_> {
    fn template(&mut self, template: &Template) -> Result<()> {
        let mut work = vec![template];
        while let Some(t) = work.pop() {
            match t {
                Template::Var(_) => {}
                Template::Constant(s) => self.syntax(s)?,
                Template::List { items, tail, .. } => {
                    work.extend(items.iter().map(|(t, _)| t));
                    work.extend(tail.as_deref());
                }
                Template::Vector { items, .. } => work.extend(items.iter().map(|(t, _)| t)),
            }
        }
        Ok(())
    }

    fn syntax(&mut self, syntax: &Syntax) -> Result<()> {
        let mut work = vec![syntax.clone()];
        while let Some(s) = work.pop() {
            match s.expose(self.context).map_err(too_many)? {
                Exposed::Identifier(name) => {
                    if self.seen.insert((name.to_owned(), s.scopes())) {
                        for phase in PHASES {
                            match s.resolve(self.context, self.table, phase) {
                                Resolution::Bound(b) => self.note(b),
                                Resolution::Ambiguous(all) => {
                                    all.into_iter().for_each(|b| self.note(b));
                                }
                                Resolution::Unbound => {}
                            }
                        }
                    }
                }
                Exposed::List { items, tail } => {
                    work.extend(items);
                    work.extend(tail);
                }
                Exposed::Vector(items) => work.extend(items),
                Exposed::Atom(_) => {}
            }
        }
        Ok(())
    }

    fn note(&mut self, binding: &Binding) {
        if let Binding::Variable(owner, name) = binding
            && *owner == self.index
        {
            self.found.insert(name.clone());
        }
    }
}
