//! Libraries, programs and REPL inputs as units: compiling one, its
//! module, interface and imports, and its artifact (ADR 0001 #5).

use super::*;

/// A library or the program, as it is compiled.
pub(super) struct Unit {
    pub(super) identity: String,
    /// What it exports, by external name.
    pub(super) interface: HashMap<String, Binding>,
    /// The private variables it defines that code it generates into other
    /// libraries may reference, which are linkable though not exported
    /// (`published`).
    pub(super) published: HashSet<String>,
    /// The names of the variables it defines that other libraries' code may
    /// link: those it exports and those it publishes. Filled in when the
    /// unit is finished.
    pub(super) linkable: HashSet<String>,
    /// The units it imports, which its instances need first.
    pub(super) imports: Vec<usize>,
    /// Its compiled module, once there is one. A library's converts to
    /// bytecode as its functions are first called, unless eager conversion
    /// is asked for (`crate::eager_conversion`).
    pub(super) module: Option<Converted>,
    /// Whether it is one of Vali's own libraries, or a program of Vali's
    /// own tests, which may import `(vali primitive)`.
    pub(super) own: bool,
    /// Whether it is a REPL's input, compiled in the interaction
    /// environment: its top-level definitions are REPL globals, and an
    /// unbound identifier names one.
    pub(super) repl: bool,
    /// Its syntax export, once it is finished: its context, and the
    /// macros, compiled syntax and bindings there, which an importer
    /// remaps into its own context. None for a native library.
    pub(super) syntax: Option<Arc<ContextState>>,
    /// What its compiled syntax is known by, which its code names: a
    /// library's identity, or for a program, an `eval` or a REPL input,
    /// its identity and a hash of its text, so two programs loaded into
    /// one isolate under one name keep their syntax apart.
    pub(super) key: String,
    /// Its module as distribution code, once there is one. None for a
    /// native library.
    pub(super) ir: Option<Arc<vali_dist::Module>>,
    /// A hash of what its importers observe, its `library` and `syntax`
    /// sections, once something asked.
    pub(super) export_hash: Option<Digest>,
    /// A hash of its whole artifact, once something asked: what a library
    /// whose expansion ran its code relies on.
    pub(super) artifact_hash: Option<Digest>,
    /// A hash of what was read to make it: its source, and the files it
    /// includes. None if its source was not read.
    pub(super) source: Option<Digest>,
    /// The libraries whose code ran while it was expanded.
    pub(super) instantiated: Vec<usize>,
    /// The libraries its expansion asked about, by name, and whether each
    /// was available: what a `cond-expand` found, whether in its own text
    /// or in a macro's template from another library. Standard libraries
    /// are not recorded, which the build stamp fixes.
    pub(super) asked: Vec<(String, bool)>,
    /// Whether its syntax export holds only its own macros and compiled
    /// syntax, as one loaded from an artifact does: importing it imports
    /// its imports' too.
    pub(super) pruned: bool,
    /// Whether it was compiled, then written and loaded again
    /// (`round_trip`).
    pub(super) round_tripped: bool,
}

impl Unit {
    /// Settles what other libraries' code may link: the variables it
    /// defines that it exports, and those it publishes, which are the
    /// private ones among `published`.
    pub(super) fn seal(&mut self, index: usize) {
        let exported: HashSet<String> = self
            .interface
            .values()
            .filter_map(|b| match b {
                Binding::Variable(owner, name) if *owner == index => Some(name.clone()),
                _ => None,
            })
            .collect();
        self.published.retain(|name| !exported.contains(name));
        self.linkable = exported;
        self.linkable.extend(self.published.iter().cloned());
    }
}

/// Where what an expander has compiled ends (`Expander::mark`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    units: usize,
    vars: usize,
    lambdas: usize,
}

/// The sections a library's artifact has, all required.
pub(super) const LIBRARY_SECTIONS: &[&str] =
    &["build", "source", "library", "syntax", "invoke", "depends"];

/// What compiled an artifact: one Vali reads only its own (ADR 0001 #8).
/// It names the compiler's version, the formats and ABI it writes, and a
/// hash of the sources of the compiler and the crates beneath it (see the
/// crate's build script), so two builds whose output may differ never
/// share one.
pub(crate) fn build_stamp() -> String {
    format!(
        "vali-compiler {} format {} abi {} build {}",
        env!("CARGO_PKG_VERSION"),
        vali_dist::FORMAT_VERSION,
        vali_abi::ABI_VERSION,
        env!("VALI_COMPILER_SOURCE_HASH"),
    )
}

/// What a library's `depends` section records.
#[derive(Debug, Default)]
pub(crate) struct Depends {
    /// Each library it imports, by identity, and the hash of that
    /// library's exports it was compiled against.
    pub(crate) imports: Vec<(String, Digest)>,
    /// Each library whose code ran while it was expanded, by identity,
    /// and the hash of that library's whole artifact.
    pub(crate) expansion: Vec<(String, Digest)>,
    /// Each library a `cond-expand` asked about while it was expanded,
    /// and whether it was found.
    pub(crate) libraries: Vec<(String, bool)>,
}

fn digest(r: &mut vali_dist::Reader<'_>) -> std::result::Result<Digest, vali_dist::DecodeError> {
    Ok(Digest(r.raw(32)?.try_into().expect("32 bytes")))
}

fn encode_depends(depends: &Depends) -> Vec<u8> {
    let mut w = vali_dist::Writer::new();
    for list in [&depends.imports, &depends.expansion] {
        w.usize(list.len());
        for (identity, hash) in list {
            w.str(identity);
            w.raw(&hash.0);
        }
    }
    w.usize(depends.libraries.len());
    for (name, found) in &depends.libraries {
        w.str(name);
        w.bool(*found);
    }
    w.finish()
}

fn decode_depends(bytes: &[u8]) -> std::result::Result<Depends, vali_dist::DecodeError> {
    let mut r = vali_dist::Reader::new(bytes);
    let mut lists = [Vec::new(), Vec::new()];
    for list in &mut lists {
        let n = r.count(33)?;
        for _ in 0..n {
            let identity = r.str()?.to_owned();
            list.push((identity, digest(&mut r)?));
        }
    }
    let mut libraries = Vec::new();
    for _ in 0..r.count(2)? {
        let name = r.str()?.to_owned();
        libraries.push((name, r.bool()?));
    }
    r.end()?;
    let [imports, expansion] = lists;
    Ok(Depends {
        imports,
        expansion,
        libraries,
    })
}

/// A library's artifact, decoded, if this build of Vali wrote it, with
/// what its `depends` section says.
pub(crate) fn decode_library_artifact(
    bytes: &[u8],
) -> std::result::Result<(vali_dist::Artifact, Depends), String> {
    let artifact =
        vali_dist::Artifact::decode(bytes, LIBRARY_SECTIONS).map_err(|e| e.to_string())?;
    let section = |name: &str| {
        artifact
            .section(name)
            .ok_or_else(|| format!("the artifact has no {name} section"))
    };
    let stamp = section("build")?;
    if stamp != build_stamp().as_bytes() {
        return Err(format!(
            "the artifact is from another build of Vali (it says {}, this is {})",
            String::from_utf8_lossy(stamp),
            build_stamp()
        ));
    }
    let depends =
        decode_depends(section("depends")?).map_err(|e| format!("its dependencies: {e}"))?;
    Ok((artifact, depends))
}

/// The source hash an artifact's `source` section records, if it does.
fn decode_source(bytes: &[u8]) -> std::result::Result<Option<Digest>, vali_dist::DecodeError> {
    let mut r = vali_dist::Reader::new(bytes);
    let source = if r.bool()? {
        Some(digest(&mut r)?)
    } else {
        None
    };
    r.end()?;
    Ok(source)
}

/// `ir` converted to run-format bytecode. It is not validated here: loading
/// a module validates it (`Cx::load_library` and the others), which is the
/// check that guards the VM, so a second pass only costs time.
pub(super) fn converted(ir: &vali_dist::Module) -> Result<Module> {
    vali_bytecode::convert(ir).map_err(|e| CompileError {
        message: format!("{}: {e}", ir.name),
        span: None,
        location: None,
    })
}

/// The module of a unit that is not a library, which converts whole.
fn whole(converted: Converted) -> Module {
    match converted {
        Converted::Eager(module) => module,
        Converted::Lazy(_) => unreachable!("only a library converts lazily"),
    }
}

/// `ir` converted for loading as a library: to be converted a function at a
/// time as each is first called, unless `lazy` is false or eager conversion
/// is asked for. As for [`converted`], loading validates the result.
pub(super) fn converted_library(ir: &Arc<vali_dist::Module>, lazy: bool) -> Result<Converted> {
    let failed = |e: vali_bytecode::ConvertError| CompileError {
        message: format!("{}: {e}", ir.name),
        span: None,
        location: None,
    };
    if lazy && !crate::eager_conversion() {
        vali_bytecode::convert_lazy(Arc::clone(ir))
            .map(Converted::Lazy)
            .map_err(failed)
    } else {
        vali_bytecode::convert(ir)
            .map(Converted::Eager)
            .map_err(failed)
    }
}

impl Expander {
    /// The unit for a native library (ADR 0016 #8): a module whose
    /// variables link straight to the library's functions as it loads, and
    /// export them. `form` names the functions; it has no body.
    pub(super) fn native_library(&mut self, identity: &str, form: &LibraryForm) -> Result<Module> {
        let index = self.units.len();
        let mut builder = ModuleBuilder::new(identity);
        let mut interface = HashMap::new();
        for spec in &form.exports {
            let variable = builder.native(identity, &spec.internal);
            builder.export(&spec.external, variable);
            interface.insert(
                spec.external.clone(),
                Binding::Variable(index, spec.external.clone()),
            );
        }
        let unspecified = builder.constant(vali_bytecode::Constant::Unspecified);
        let mut body = vali_bytecode::FunctionBuilder::new(
            Some(identity),
            vali_bytecode::Arity::exactly(0),
            2,
            0,
        );
        body.emit(vali_bytecode::Instruction::LoadConstant {
            dst: 1,
            constant: unspecified,
        })
        .emit(vali_bytecode::Instruction::Return { src: 1 });
        let body = body
            .finish()
            .map_err(|e| error(format!("{identity}: {e}"), form.span))?;
        let body = builder.function(body);
        let module = builder.finish(body);
        validate(&module).map_err(|e| error(format!("{identity}: {e}"), form.span))?;
        self.units.push(Unit {
            identity: identity.to_owned(),
            linkable: interface.keys().cloned().collect(),
            interface,
            published: HashSet::new(),
            imports: Vec::new(),
            module: Some(Converted::Eager(module.clone())),
            own: false,
            repl: false,
            syntax: None,
            key: identity.to_owned(),
            ir: None,
            export_hash: None,
            artifact_hash: None,
            source: None,
            instantiated: Vec::new(),
            asked: Vec::new(),
            pruned: false,
            round_tripped: false,
        });
        Ok(module)
    }

    /// Starts the context the next unit expands in, which imports
    /// `imports`: for a REPL's input, the interaction environment's own;
    /// otherwise a copy of the syntax export of the first library it
    /// imports that has one, which is remapping it with every scope mapped
    /// to itself, or a fresh context. The state of the unit that had the
    /// live context becomes its syntax export; the REPL's is set aside for
    /// its next input.
    pub(super) fn start_context(&mut self, repl: bool, imports: &[ImportSet]) -> Result<()> {
        if repl && matches!(self.owner, Some(ContextOwner::Repl)) {
            // The REPL's next input, with nothing in between.
            return Ok(());
        }
        self.finish_context()?;
        let mut seeded = None;
        let seed = imports.iter().find_map(|set| {
            let unit = *self.by_name.get(set.library())?;
            self.units[unit].syntax.as_ref().map(|s| (unit, s))
        });
        let next = if repl && self.repl_state.is_some() {
            self.repl_state.take().expect("checked")
        } else if let Some((unit, export)) = seed {
            seeded = Some(unit);
            let mut copy = ContextState::clone(export);
            copy.imported.insert(unit);
            copy.next_macro = 0;
            // Transformers are made afresh in each unit's isolates.
            for m in copy.macros.values_mut() {
                if let Transformer::Procedure(p) = &mut m.transformer {
                    p.made = None;
                }
            }
            copy
        } else {
            ContextState::fresh()?
        };
        self.swap_state(next);
        self.owner = Some(if repl {
            ContextOwner::Repl
        } else {
            ContextOwner::Unit(self.units.len())
        });
        if let Some(unit) = seeded {
            self.import_dependencies(unit)?;
        }
        Ok(())
    }

    /// Ends the live context's ownership: a unit's state becomes its
    /// syntax export, the REPL's is set aside for its next input.
    pub(super) fn finish_context(&mut self) -> Result<()> {
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        let mut state = self.swap_state(ContextState::empty(self.core_scope, self.core));
        match owner {
            ContextOwner::Unit(unit) => {
                if !self.units[unit].repl {
                    let key = self.units[unit].key.clone();
                    let inferred = super::published::infer(&mut state, unit, &key)?;
                    let u = &mut self.units[unit];
                    u.published.extend(inferred);
                    u.seal(unit);
                    // A library's importers need its own syntax only, and
                    // import its imports' from their exports, as for one
                    // loaded from an artifact. Keeping the whole context,
                    // which holds everything it imported, made a chain of
                    // libraries take memory and time quadratic in its
                    // length.
                    if self.by_name.values().any(|&named| named == unit) {
                        state = self.pruned_export(unit, &state)?;
                        let u = &mut self.units[unit];
                        u.pruned = true;
                        // Compiled here, not loaded from an artifact.
                        u.round_tripped = true;
                    }
                }
                // Kept as the unit's syntax export for as long as the
                // expander lives, so without the caches that only saved time.
                state.context.clear_caches();
                state.table.clear_caches();
                self.units[unit].syntax = Some(Arc::new(state));
            }
            ContextOwner::Repl => self.repl_state = Some(state),
        }
        Ok(())
    }

    /// Where what the expander has compiled ends, to drop what is compiled
    /// after it with `forget`.
    pub(crate) fn mark(&self) -> Mark {
        Mark {
            units: self.units.len(),
            vars: self.vars.len(),
            lambdas: self.lambdas.len(),
        }
    }

    /// Drops the units compiled since `mark`, which must be evaluated code
    /// or REPL inputs, never libraries, once their modules are loaded,
    /// whether they compiled or not: nothing names them, no unit imports
    /// them, and their code holds what it needs. Their variables and
    /// `lambda`s go too, and a unit's context, which would otherwise
    /// become its syntax export. The interaction environment's context
    /// stays, holding what its inputs defined.
    pub(crate) fn forget(&mut self, mark: Mark) {
        debug_assert!(
            self.by_name.values().all(|&unit| unit < mark.units),
            "only unnamed units are forgotten"
        );
        if matches!(self.owner, Some(ContextOwner::Unit(unit)) if unit >= mark.units) {
            self.owner = None;
            self.swap_state(ContextState::empty(self.core_scope, self.core));
        }
        self.forget_locals();
        self.units.truncate(mark.units);
        self.vars.truncate(mark.vars);
        self.lambdas.truncate(mark.lambdas);
        self.check_no_stale_variables();
        self.last_ir = None;
        self.last_lowered = None;
    }

    /// Drops the local variables of the REPL input that was just compiled
    /// from the interaction environment's table: the `Local` and
    /// `PatternVar` entries, whose variables are about to go (`forget`).
    /// Nothing a later input can write reaches them: a phase-0 local is
    /// visible only to code inside the form that bound it, and the macros
    /// defined there are local too. Imports never bring locals across
    /// (`remap_bindings`).
    ///
    /// The input's scopes are those numbered from `repl_floor`, since a
    /// context numbers scopes in the order it makes them and never reuses
    /// a number, so a scope a macro the input used made counts too. An
    /// entry goes if its set has one of them that a binder made.
    ///
    /// Nothing else goes. The input's top-level definitions are REPL
    /// globals, and those, top-level macros and anything a macro defined at
    /// top level are bound with the top-level scopes, maybe with macro and
    /// use-site scopes of the input added. Local macros stay too, with their
    /// entries in `macros`: a top-level macro's transformer can be wrapped
    /// in a `let-syntax`, and its output then names the local macro under
    /// the binder's scope, as does that of an imported macro whose helper a
    /// library bound under a binder scope, which an import remaps in.
    ///
    /// A syntax object that outlives the input, with a dropped scope in its
    /// set, resolves as if the variable were not there: outward, to the
    /// global of that name or to nothing. Scope numbers are not reused, so
    /// it can never meet another input's binding.
    fn forget_locals(&mut self) {
        let Some(from) = self.repl_floor.take() else {
            return;
        };
        if !matches!(self.owner, Some(ContextOwner::Repl)) {
            return;
        }
        let context = &self.context;
        self.table.forget_tracked(|set, binding| {
            matches!(binding, Binding::Local(_) | Binding::PatternVar(..))
                && context
                    .scopes(set)
                    .iter()
                    .any(|&s| s.index() >= from && context.kind(s) == ScopeKind::Binder)
        });
    }

    /// Checks that no entry of the interaction environment's table names a
    /// variable that `forget` truncated, which a later input's variable
    /// would take the number of. It walks the whole table, so it is for
    /// debug builds only, and a debug REPL session pays for it per input.
    fn check_no_stale_variables(&self) {
        if cfg!(debug_assertions) && matches!(self.owner, Some(ContextOwner::Repl)) {
            for (symbol, _, _, binding) in self.table.entries() {
                if let Binding::Local(v) | Binding::PatternVar(v, _) = binding {
                    assert!(
                        *v < self.vars.len(),
                        "{symbol} is bound to variable {v}, which was forgotten"
                    );
                }
            }
        }
    }

    /// The number of entries in the live context's binding table, for tests.
    #[cfg(test)]
    pub(crate) fn table_len(&self) -> usize {
        self.table.entries().count()
    }

    /// The artifact of `unit`, a library this build compiled or loaded:
    /// its record, syntax export, module as distribution code, and what
    /// it relied on of its imports (ADRs 0001 #5 and 0012 #5).
    pub(crate) fn library_artifact(&mut self, unit: usize) -> Result<Vec<u8>> {
        if matches!(self.owner, Some(ContextOwner::Unit(u)) if u == unit) {
            self.finish_context()?;
        }
        let fail = |message: String| CompileError {
            message,
            span: None,
            location: None,
        };
        let identity = self.units[unit].identity.clone();
        let (Some(syntax), Some(ir)) =
            (self.units[unit].syntax.clone(), self.units[unit].ir.clone())
        else {
            return Err(fail(format!("{identity} has no artifact")));
        };
        let library = self.library_record(unit)?;
        let syntax = self.encode_syntax(unit, &syntax)?;
        let invoke = vali_dist::encode(&ir).map_err(|e| fail(format!("{identity}: {e}")))?;
        let mut depends = Depends::default();
        for dependency in self.units[unit].imports.clone() {
            let hash = self.export_hash(dependency)?;
            depends
                .imports
                .push((self.units[dependency].identity.clone(), hash));
        }
        let mut ran = self.units[unit].instantiated.clone();
        ran.retain(|&u| self.units[u].ir.is_some());
        ran.sort_by(|&a, &b| self.units[a].identity.cmp(&self.units[b].identity));
        for dependency in ran {
            let hash = self.artifact_hash(dependency)?;
            depends
                .expansion
                .push((self.units[dependency].identity.clone(), hash));
        }
        depends.libraries = self.units[unit].asked.clone();
        depends.libraries.sort();
        let mut source = vali_dist::Writer::new();
        source.bool(self.units[unit].source.is_some());
        if let Some(hash) = self.units[unit].source {
            source.raw(&hash.0);
        }
        let mut artifact = vali_dist::Artifact::default();
        artifact.insert("build", true, build_stamp().into_bytes());
        artifact.insert("source", true, source.finish());
        artifact.insert("library", true, library);
        artifact.insert("syntax", true, syntax);
        artifact.insert("invoke", true, invoke);
        artifact.insert("depends", true, encode_depends(&depends));
        Ok(artifact.encode())
    }

    /// The bytes of `unit`'s `library` section.
    pub(super) fn library_record(&self, unit: usize) -> Result<Vec<u8>> {
        let u = &self.units[unit];
        let identities: Vec<String> = self.units.iter().map(|u| u.identity.clone()).collect();
        let record = artifact::LibraryRecord {
            identity: u.identity.clone(),
            key: u.key.clone(),
            imports: u.imports.clone(),
            interface: u.interface.clone(),
            published: u.published.clone(),
        };
        artifact::encode_library(&record, &identities).map_err(|message| CompileError {
            message: format!("{}: {message}", u.identity),
            span: None,
            location: None,
        })
    }

    /// A hash of what importers of `unit` observe: its `library` and
    /// `syntax` sections, as this build would write them.
    pub(super) fn export_hash(&mut self, unit: usize) -> Result<Digest> {
        if let Some(hash) = self.units[unit].export_hash {
            return Ok(hash);
        }
        // Its syntax export is not made until its context is finished,
        // which the next unit compiled would otherwise do.
        if matches!(self.owner, Some(ContextOwner::Unit(u)) if u == unit) {
            self.finish_context()?;
        }
        let mut bytes = self.library_record(unit)?;
        if let Some(syntax) = self.units[unit].syntax.clone() {
            bytes.extend(self.encode_syntax(unit, &syntax)?);
        }
        let hash = Digest::of(&bytes);
        self.units[unit].export_hash = Some(hash);
        Ok(hash)
    }

    /// A hash of `unit`'s whole artifact, as this build would write it:
    /// what a library relies on if its expansion ran `unit`'s code, which
    /// the hash covers along with what `unit` itself relied on.
    pub(super) fn artifact_hash(&mut self, unit: usize) -> Result<Digest> {
        if let Some(hash) = self.units[unit].artifact_hash {
            return Ok(hash);
        }
        let hash = Digest::of(&self.library_artifact(unit)?);
        self.units[unit].artifact_hash = Some(hash);
        Ok(hash)
    }

    /// Loads the library `identity` from its artifact, as compiling it
    /// would have: every library it imports must be loaded, each as it
    /// was when the artifact was written. `own` says whether it is one of
    /// Vali's own libraries, whatever the artifact says; a library that is
    /// not may not link the internal primitives, whose names start with
    /// `%`. Returns its module.
    pub(crate) fn load_library(
        &mut self,
        identity: &str,
        bytes: &[u8],
        own: bool,
        source: Option<Digest>,
    ) -> Result<Converted> {
        let fail = |message: String| CompileError {
            message: format!("{identity}: {message}"),
            span: None,
            location: None,
        };
        // What the libraries it links publish must be settled.
        self.finish_context()?;
        let (artifact, depends) = decode_library_artifact(bytes).map_err(fail)?;
        let section = |name: &str| {
            artifact
                .section(name)
                .ok_or_else(|| fail(format!("the artifact has no {name} section")))
        };
        // Its source, if the caller has read it, must be what it was
        // compiled from.
        let recorded = decode_source(section("source")?)
            .map_err(|e| fail(format!("its source record: {e}")))?;
        if let Some(source) = source
            && recorded != Some(source)
        {
            return Err(fail(match recorded {
                Some(_) => "its source has changed since it was compiled".to_owned(),
                None => "it does not record what source it was compiled from".to_owned(),
            }));
        }
        let mut imported = Vec::new();
        for (dependency, hash) in &depends.imports {
            let unit = self
                .unit_of(dependency)
                .ok_or_else(|| fail(format!("it needs {dependency}, which is not loaded")))?;
            if self.export_hash(unit)? != *hash {
                return Err(fail(format!(
                    "{dependency} has changed since it was compiled"
                )));
            }
            imported.push(unit);
        }
        let mut instantiated = Vec::new();
        for (dependency, hash) in &depends.expansion {
            let unit = self
                .unit_of(dependency)
                .ok_or_else(|| fail(format!("it needs {dependency}, which is not loaded")))?;
            if self.artifact_hash(unit)? != *hash {
                return Err(fail(format!(
                    "{dependency}, whose code ran while it was expanded, has changed \
                     since it was compiled"
                )));
            }
            instantiated.push(unit);
        }
        // What it asked about libraries must still have the same answer:
        // its expansion followed it.
        for (name, found) in &depends.libraries {
            let now = self
                .available
                .iter()
                .any(|(n, available)| *available && n.to_string() == *name);
            if now != *found {
                return Err(fail(format!(
                    "library {name} was {} when it was compiled, and is {} now",
                    if *found { "available" } else { "missing" },
                    if now { "available" } else { "missing" },
                )));
            }
        }
        let ir = vali_dist::decode(section("invoke")?).map_err(|e| fail(e.to_string()))?;
        // Compiled syntax's primitives are allowed in a procedure as in a
        // transformer, but only called for the library's own unit, which
        // is checked once its key is known.
        let mut linkable = MACRO_PRIMITIVES.to_vec();
        linkable.extend(SYNTAX_PRIMITIVES);
        if !own && let Some(name) = links_internal(&ir, &linkable) {
            return Err(fail(format!(
                "it links {name}, which only Vali's own libraries may"
            )));
        }
        if let Some(name) = links_repl(&ir) {
            return Err(fail(format!(
                "it links {name}, an isolate global, which only a REPL's input may"
            )));
        }
        self.check_links(&ir).map_err(|e| fail(format!("it {e}")))?;
        let ir = Arc::new(ir);
        let mut module = converted_library(&ir, true)?;
        // The digest that tells this build from another is the IR's
        // encoding's, as `library` computes it for a library made from
        // source.
        module.module_mut().digest = Some(Digest::of(section("invoke")?).0);
        // The unit goes in first: what it exports names it.
        let index = self.units.len();
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface: HashMap::new(),
            published: HashSet::new(),
            linkable: HashSet::new(),
            imports: Vec::new(),
            module: Some(module.clone()),
            own,
            repl: false,
            syntax: None,
            key: identity.to_owned(),
            ir: Some(ir.clone()),
            export_hash: None,
            artifact_hash: None,
            source: None,
            instantiated: Vec::new(),
            asked: Vec::new(),
            pruned: true,
            round_tripped: false,
        });
        let filled = (|| {
            let identities: Vec<String> = self.units.iter().map(|u| u.identity.clone()).collect();
            let resolve = |identity: &str| identities.iter().position(|i| i == identity);
            let record = artifact::decode_library(section("library")?, &resolve)?;
            if record.identity != identity {
                return Err(fail(format!("the artifact is {}'s", record.identity)));
            }
            let syntax = artifact::decode(section("syntax")?, &resolve, &mut self.sources)?;
            self.check_export(index, own, &record, &syntax)
                .map_err(fail)?;
            // The primitives that build and match syntax read the tables
            // of the unit they name, and those hold every loaded library's,
            // so code may call them only for its own unit.
            if !own && let Some(name) = misuses_syntax_primitive(&ir, &record.key) {
                return Err(fail(format!(
                    "it uses {name} other than to call it for its own syntax"
                )));
            }
            let unit = &mut self.units[index];
            // The records of what it imports must agree.
            if record.imports != imported {
                return Err(fail(
                    "its imports and its dependency records name different libraries".to_owned(),
                ));
            }
            unit.key = record.key;
            unit.imports = record.imports;
            unit.source = recorded;
            unit.instantiated = instantiated;
            unit.asked = depends.libraries;
            unit.interface = record.interface;
            unit.published = record.published;
            unit.seal(index);
            unit.syntax = Some(Arc::new(syntax));
            // Writing it again would give these bytes, so they are what its
            // importers' artifacts hash, without encoding it again.
            let mut observed = section("library")?.to_vec();
            observed.extend_from_slice(section("syntax")?);
            unit.export_hash = Some(Digest::of(&observed));
            Ok(())
        })();
        if let Err(e) = filled {
            self.units.pop();
            return Err(e);
        }
        Ok(module)
    }

    /// Checks that every library variable `ir` links is accessible: its
    /// library is loaded and exports or publishes the name, and only then.
    /// What is private to a library is unreachable from the code of
    /// another, whatever that code says (ADR 0012 #9). Returns what the
    /// first link that is not does wrong, to follow "it".
    fn check_links(&self, ir: &vali_dist::Module) -> std::result::Result<(), String> {
        let mut units: HashMap<&str, Option<usize>> = HashMap::new();
        for g in &ir.globals {
            let vali_dist::GlobalLink::Imported(library) = &g.link else {
                continue;
            };
            let Some(unit) = *units
                .entry(library)
                .or_insert_with(|| self.unit_of(library))
            else {
                return Err(format!("links {library}, which is not loaded"));
            };
            if !self.units[unit].linkable.contains(&*g.name) {
                return Err(format!("links {library}'s private {}", g.name));
            }
        }
        Ok(())
    }

    /// Checks that what a library's record and syntax export bind is there to
    /// be used: each macro exists, in this library or the one it names, and
    /// only Vali's own libraries bind an internal primitive. The expander
    /// trusts a binding it finds, so one that names nothing is refused here
    /// and not met in the middle of expanding a program.
    fn check_export(
        &self,
        index: usize,
        own: bool,
        record: &artifact::LibraryRecord,
        syntax: &ContextState,
    ) -> std::result::Result<(), String> {
        // An export holds its own library's macros and compiled syntax and
        // no others': importers copy what it holds, and the first copy of a
        // macro wins, so one that names another library's would replace it.
        if let Some(id) = syntax.macros.keys().find(|id| id.unit as usize != index) {
            return Err(format!(
                "its syntax export defines macro {} of another library",
                id.index
            ));
        }
        if let Some(key) = syntax.compiled.keys().find(|key| **key != record.key) {
            return Err(format!(
                "its syntax export has compiled syntax under {key}, not its own key"
            ));
        }
        // A transformer runs in the expander's isolate, so what its code
        // links is held to what a library's own may, and it may not touch
        // the isolate's globals or link what another library keeps private.
        for m in syntax.macros.values() {
            if let Transformer::Procedure(p) = &m.transformer {
                if let Some(name) = links_repl(&p.ir) {
                    return Err(format!(
                        "the transformer {} links {name}, an isolate global, which only a \
                         REPL's input may",
                        m.name
                    ));
                }
                if !own && let Some(name) = misuses_syntax_primitive(&p.ir, &record.key) {
                    return Err(format!(
                        "the transformer {} uses {name} other than to call it for its own \
                         syntax",
                        m.name
                    ));
                }
                self.check_links(&p.ir)
                    .map_err(|e| format!("the transformer {} {e}", m.name))?;
            }
        }
        if !own {
            let mut allowed = SYNTAX_PRIMITIVES.to_vec();
            allowed.extend(MACRO_PRIMITIVES);
            for m in syntax.macros.values() {
                if let Transformer::Procedure(p) = &m.transformer
                    && let Some(name) = links_internal(&p.ir, &allowed)
                {
                    return Err(format!(
                        "the transformer {} links {name}, which only Vali's own libraries may",
                        m.name
                    ));
                }
            }
        }
        // The variables of another library this export may name: what that
        // library's interface exports, and, in the syntax export only, what
        // came in when this library imported it, which a macro's template
        // may need (see `reproduces`). Its other variables are its own: no
        // export of another library reaches them.
        let exported_by = |owner: usize| -> HashSet<&str> {
            self.units.get(owner).map_or_else(HashSet::new, |u| {
                u.interface
                    .values()
                    .filter_map(|b| match b {
                        Binding::Variable(o, n) if *o == owner => Some(n.as_str()),
                        _ => None,
                    })
                    .collect()
            })
        };
        let mut exported: HashMap<usize, HashSet<&str>> = HashMap::new();
        let mut brought: HashMap<usize, Brought<'_>> = HashMap::new();
        let own_scopes = own_scopes(syntax, index);
        let interface = record.interface.values().map(|b| (None, b));
        let table = syntax
            .table
            .entries()
            .map(|(symbol, set, _, binding)| (Some((symbol, set)), binding));
        for (entry, binding) in interface.chain(table) {
            match binding {
                Binding::Global(name) => {
                    return Err(format!(
                        "it binds {name}, an isolate global, which only a REPL's input may"
                    ));
                }
                Binding::Variable(owner, name) if *owner != index => {
                    let unit = self.units.get(*owner).filter(|u| !u.repl).ok_or_else(|| {
                        format!("it binds {name} of a library that is not loaded")
                    })?;
                    let names = exported
                        .entry(*owner)
                        .or_insert_with(|| exported_by(*owner));
                    if names.contains(name.as_str()) {
                        continue;
                    }
                    let theirs = unit.syntax.as_deref();
                    let kept = entry.zip(theirs).is_some_and(|((symbol, set), theirs)| {
                        reproduces(
                            theirs,
                            syntax,
                            &own_scopes,
                            brought
                                .entry(*owner)
                                .or_insert_with(|| Brought::of(*owner, theirs)),
                            symbol,
                            set,
                            name,
                        )
                    });
                    if !kept {
                        return Err(format!(
                            "it binds {name} of {}, which does not export it",
                            unit.identity
                        ));
                    }
                }
                _ => {}
            }
            match binding {
                Binding::Macro(id) => {
                    let defined = |syntax: &ContextState| syntax.macros.contains_key(id);
                    let there = if id.unit as usize == index {
                        defined(syntax)
                    } else {
                        self.units
                            .get(id.unit as usize)
                            .and_then(|u| u.syntax.as_deref())
                            .is_some_and(defined)
                    };
                    if !there {
                        return Err(format!(
                            "it binds macro {} of {}, which does not exist",
                            id.index,
                            self.units
                                .get(id.unit as usize)
                                .map_or("a library that is not loaded", |u| u.identity.as_str())
                        ));
                    }
                }
                Binding::Primitive(name) if !own && name.starts_with('%') => {
                    return Err(format!(
                        "it binds {name}, an internal primitive that only Vali's own \
                         libraries may"
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Expands and compiles a program. With imports, it sees only what it
    /// imports. An `implicit` import, of `(scheme base)` for a program with
    /// no `import` form (ADR 0007 #11), is bound in a scope around the
    /// program's own, so the program can define names it also imports, as
    /// a script or a REPL can.
    pub(crate) fn program(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        implicit: bool,
        body: &[vali_syntax::Datum],
        internal: bool,
    ) -> Result<Module> {
        self.start_context(false, imports)?;
        self.next_own = internal;
        self.next_hashed = true;
        let span = body.first().map_or(
            Span {
                source: SourceId(0),
                range: Default::default(),
            },
            |d| d.span,
        );
        let scope = self.scope(ScopeKind::Library)?;
        if implicit {
            let around = self.scope(ScopeKind::Library)?;
            let imported = self.context.intern(&[around]).map_err(too_many)?;
            let top = self.context.intern(&[around, scope]).map_err(too_many)?;
            return self
                .unit(identity, imports, imported, top, body, &[], span, false)
                .map(whole);
        }
        let top = self.context.intern(&[scope]).map_err(too_many)?;
        self.unit(identity, imports, top, top, body, &[], span, false)
            .map(whole)
    }

    /// Expands and compiles a REPL's input in the interaction environment
    /// (R7RS 6.12's `interaction-environment`): `imports` bound around the
    /// REPL's own definitions, which persist from one input to the next.
    pub(crate) fn interaction(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        body: &[vali_syntax::Datum],
    ) -> Result<Module> {
        self.start_context(true, imports)?;
        self.repl_floor = Some(self.context.scope_count() as u32);
        self.table.track();
        self.next_hashed = true;
        let span = body.first().map_or(
            Span {
                source: SourceId(0),
                range: Default::default(),
            },
            |d| d.span,
        );
        let (imported, top) = match self.repl {
            Some(scopes) => scopes,
            None => {
                let around = self.scope(ScopeKind::Library)?;
                let scope = self.scope(ScopeKind::Library)?;
                let imported = self.context.intern(&[around]).map_err(too_many)?;
                let top = self.context.intern(&[around, scope]).map_err(too_many)?;
                self.repl = Some((imported, top));
                (imported, top)
            }
        };
        // Imports bound before are bound again, which is a no-op.
        let module = self
            .unit(identity, imports, imported, top, body, &[], span, true)
            .map(whole);
        // The context lives as long as the REPL does, so what only saves
        // time is dropped with each input, or it grows for the whole
        // session. The next input rebuilds what it uses.
        self.context.clear_caches();
        self.table.clear_caches();
        module
    }

    /// How many entries the live context's caches hold, for tests.
    #[cfg(test)]
    pub(crate) fn cache_len(&mut self) -> usize {
        self.context.cache_len() + self.table.cache_len()
    }

    /// Whether the unit being compiled is a REPL's input.
    pub(super) fn in_repl(&self) -> bool {
        self.units.last().is_some_and(|u| u.repl)
    }

    /// `(define id ...)` at a REPL's top level: binds `id` to a REPL
    /// global, replacing whatever it was bound to there, and returns the
    /// global's name.
    pub(super) fn define_repl(&mut self, id: &Syntax) -> Result<String> {
        let name = id
            .as_identifier()
            .ok_or_else(|| error("define: expected a name", id.span()))?;
        let global = if id.scopes() == self.top {
            repl_global(name)
        } else {
            self.repl_fresh += 1;
            repl_global(&format!("{name}~{}", self.repl_fresh))
        };
        self.table.rebind(
            &self.context,
            name.to_owned(),
            id.scopes(),
            Phases::All,
            Binding::Global(global.clone()),
        );
        Ok(global)
    }

    /// Expands `body` as a new unit whose forms carry `top`, with
    /// `imports` bound at `imported`, and compiles it into a module that
    /// exports every variable it defines, by name, for linking.
    #[expect(
        clippy::too_many_arguments,
        reason = "each says how the unit is scoped"
    )]
    pub(super) fn unit(
        &mut self,
        identity: &str,
        imports: &[ImportSet],
        imported: ScopeSetId,
        top: ScopeSetId,
        body: &[vali_syntax::Datum],
        exports: &[ExportSpec],
        span: Span,
        repl: bool,
    ) -> Result<Converted> {
        let index = self.units.len();
        // A new unit expands with fresh expansion-time instances, and
        // numbers what it makes from zero, so its output depends only on
        // itself (ADR 0001 #4).
        self.phases.clear();
        self.generation += 1;
        self.fresh = 0;
        // A unit that failed to compile can leave its lambdas open, and
        // `forget` reuses their numbers.
        self.stack.clear();
        self.floors.clear();
        let imported_at = imported;
        let library = std::mem::take(&mut self.next_library);
        let mut imported = Vec::new();
        for set in imports {
            if let Some(&unit) = self.by_name.get(set.library()) {
                imported.push(unit);
            }
        }
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface: HashMap::new(),
            published: HashSet::new(),
            linkable: HashSet::new(),
            imports: imported,
            module: None,
            own: std::mem::take(&mut self.next_own),
            repl,
            syntax: None,
            key: if std::mem::take(&mut self.next_hashed) {
                format!("{identity}@{:016x}", body_hash(body))
            } else {
                identity.to_owned()
            },
            ir: None,
            export_hash: None,
            artifact_hash: None,
            source: None,
            instantiated: Vec::new(),
            asked: Vec::new(),
            pruned: false,
            round_tripped: false,
        });
        self.top = top;
        for set in imports {
            self.import(set, imported_at, span)?;
        }
        let forms = body.iter().map(|d| Syntax::from_datum(d, top)).collect();
        let lambda = self.open_lambda(None);
        let expr = self.body(forms, true, span)?;
        self.close_lambda(lambda, Vec::new(), None, expr);
        let mut interface = HashMap::new();
        for spec in exports {
            let id = Syntax::identifier(&spec.internal, top, spec.span, None);
            let Some(binding) = self.resolve(&id)? else {
                return Err(error(
                    format!(
                        "{} is exported but neither defined nor imported",
                        spec.internal
                    ),
                    spec.span,
                ));
            };
            if interface.insert(spec.external.clone(), binding).is_some() {
                return Err(error(
                    format!("{} is exported twice", spec.external),
                    spec.span,
                ));
            }
        }
        let module = self.module(lambda, identity, library)?;
        let ir = self.last_ir.clone();
        let unit = &mut self.units[index];
        unit.interface = interface;
        unit.module = Some(module.clone());
        unit.ir = ir;
        Ok(module)
    }

    /// Compiles the `lambda` `body` as a module's body, exporting every
    /// variable the module defines under its own name. Everything
    /// compiled goes through the IR (ADR 0017): the tree is
    /// lowered to it, then converted to run-format bytecode: for a
    /// `library`, as its functions are first called (`converted_library`).
    pub(super) fn module(
        &mut self,
        body: LambdaId,
        name: &str,
        library: bool,
    ) -> Result<Converted> {
        let lowered = self.lower_only(body, name)?;
        let mut ir = lowered.clone();
        self.optimised(&mut ir);
        let ir = Arc::new(ir);
        let module = converted_library(&ir, library)?;
        self.last_lowered = Some(lowered);
        self.last_ir = Some(ir);
        Ok(module)
    }

    /// The `lambda` `body` lowered to the IR, as a module's body, and
    /// optimised (`optimised`).
    pub(super) fn lowered(&self, body: LambdaId, name: &str) -> Result<vali_dist::Module> {
        let mut ir = self.lower_only(body, name)?;
        self.optimised(&mut ir);
        Ok(ir)
    }

    /// The `lambda` `body` lowered to the IR, as a module's body.
    pub(super) fn lower_only(&self, body: LambdaId, name: &str) -> Result<vali_dist::Module> {
        crate::lower::lower(&self.vars, &self.lambdas, &self.sources, body, name)
    }

    /// Optimises `ir`: fully, unless the unit is evaluated code or a REPL
    /// input, which gets only contification, so its loops are loops (ADR
    /// 0013 #13); not at all if optimising is off.
    pub(super) fn optimised(&self, ir: &mut vali_dist::Module) {
        if !optimising() {
        } else if self.optimise {
            vali_opt::optimise(ir);
        } else {
            vali_opt::contify(ir);
        }
    }

    /// What the library `name` exports, by name.
    pub(super) fn interface(
        &self,
        name: &LibraryName,
        span: Span,
    ) -> Result<Vec<(String, Binding)>> {
        if *name == LibraryName::new(["vali", "core"]) {
            return Ok(self.core_interface.clone());
        }
        if *name == LibraryName::new(["vali", "primitive"]) {
            // The syntax procedures exist only in the phase-1 isolate, but
            // are bound everywhere, so transformers in libraries can name
            // them.
            return Ok(vali_runtime::primitive_names()
                .into_iter()
                .chain(phase::SYNTAX_PROCEDURES.iter().copied())
                .chain(crate::eval::EVAL_PROCEDURES.iter().copied())
                .map(|n| (n.to_owned(), Binding::Primitive(n.to_owned())))
                .collect());
        }
        let Some(&unit) = self.by_name.get(name) else {
            return Err(error(format!("library {name} is not available"), span));
        };
        let mut bindings: Vec<_> = self.units[unit]
            .interface
            .iter()
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect();
        bindings.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(bindings)
    }

    /// What an import set brings in, by the names it brings them in under.
    pub(super) fn import_bindings(
        &self,
        set: &ImportSet,
        span: Span,
    ) -> Result<Vec<(String, Binding)>> {
        let missing = |name: &str, library: &LibraryName| {
            error(format!("{library} does not export {name}"), span)
        };
        Ok(match set {
            ImportSet::Library(name, _) => self.interface(name, span)?,
            ImportSet::Only(inner, names) => {
                let all = self.import_bindings(inner, span)?;
                let mut kept = Vec::new();
                for name in names {
                    let found = all
                        .iter()
                        .find(|(n, _)| n == name)
                        .ok_or_else(|| missing(name, set.library()))?;
                    kept.push(found.clone());
                }
                kept
            }
            ImportSet::Except(inner, names) => {
                let all = self.import_bindings(inner, span)?;
                for name in names {
                    if !all.iter().any(|(n, _)| n == name) {
                        return Err(missing(name, set.library()));
                    }
                }
                all.into_iter()
                    .filter(|(n, _)| !names.contains(n))
                    .collect()
            }
            ImportSet::Prefix(inner, prefix) => self
                .import_bindings(inner, span)?
                .into_iter()
                .map(|(n, b)| (format!("{prefix}{n}"), b))
                .collect(),
            ImportSet::Rename(inner, renames) => {
                let mut all = self.import_bindings(inner, span)?;
                for (from, to) in renames {
                    let entry = all
                        .iter_mut()
                        .find(|(n, _)| n == from)
                        .ok_or_else(|| missing(from, set.library()))?;
                    entry.0.clone_from(to);
                }
                all
            }
        })
    }

    /// Binds what `set` imports at `top`. Importing one name twice is
    /// fine if both mean the same.
    pub(super) fn import(&mut self, set: &ImportSet, top: ScopeSetId, span: Span) -> Result<()> {
        // The internal procedures are for Vali's own libraries, which build
        // the standard ones on them; anything else reaching them could
        // bypass what its imports grant (ADR 0009).
        if *set.library() == LibraryName::new(["vali", "primitive"])
            && !self.units.last().is_some_and(|u| u.own)
        {
            return Err(error(
                format!(
                    "{} is internal to Vali: import the standard library that provides what you need",
                    set.library()
                ),
                set.span().unwrap_or(span),
            ));
        }
        // Its macros, and what they refer to, come across first.
        if let Some(&unit) = self.by_name.get(set.library()) {
            self.import_syntax(unit)?;
        }
        for (name, binding) in self.import_bindings(set, span)? {
            match self.table.bind(
                &self.context,
                name.clone(),
                top,
                Phases::All,
                binding.clone(),
            ) {
                Ok(()) => {}
                Err(AlreadyBound { existing }) if *existing == binding => {}
                Err(_) => {
                    return Err(error(
                        format!("{name} is imported twice, with different meanings"),
                        span,
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The first of `ir`'s globals that only Vali's own libraries may link: an
/// internal primitive, whose name starts with `%`, other than those in
/// `allowed`, or a native function.
fn links_internal(ir: &vali_dist::Module, allowed: &[&str]) -> Option<String> {
    ir.globals
        .iter()
        .find(|g| {
            matches!(g.link, vali_dist::GlobalLink::Primitive)
                && g.name.starts_with('%')
                && !allowed.contains(&&*g.name)
                || matches!(g.link, vali_dist::GlobalLink::Native(_))
        })
        .map(|g| g.name.to_string())
}

/// Checks that `ir` uses the syntax primitives only as the expander emits
/// them: each read of one is the callee of calls whose unit argument (the
/// first, or for `%syntax-match` the second) is the string constant `key`,
/// the unit whose tables back the library, and
/// is used for nothing else. The primitives look up the unit they are
/// given in tables that hold every loaded library's, so a call naming
/// another unit, a computed name, or the primitive passed on as a value
/// would reach what is not the library's own. Returns the primitive that
/// is not.
fn misuses_syntax_primitive(ir: &vali_dist::Module, key: &str) -> Option<String> {
    use vali_dist::{Operand, Term, Value};
    let is_key = |op: &Operand, consts: &HashMap<u32, u32>| {
        let id = match op {
            Operand::Const(c) => Some(c.0),
            Operand::Var(v) => consts.get(&v.0).copied(),
        };
        id.and_then(|id| ir.constants.get(id as usize))
            .is_some_and(|c| matches!(c, vali_dist::Constant::String(s) if &**s == key))
    };
    for function in &ir.functions {
        // What each variable is bound to, where it is a primitive or a
        // constant, and how many times each is bound at all.
        let mut bound: HashMap<u32, u32> = HashMap::new();
        let mut prims: HashMap<u32, &str> = HashMap::new();
        let mut consts: HashMap<u32, u32> = HashMap::new();
        for v in function.params.iter().chain(&function.free) {
            *bound.entry(v.0).or_default() += 1;
        }
        for term in &function.terms {
            match term {
                Term::Let { var, value, .. } => {
                    *bound.entry(var.0).or_default() += 1;
                    match value {
                        Value::Global(g) => {
                            if let Some(g) = ir.globals.get(g.0 as usize)
                                && matches!(g.link, vali_dist::GlobalLink::Primitive)
                                && SYNTAX_PRIMITIVES.contains(&&*g.name)
                            {
                                prims.insert(var.0, &g.name);
                            }
                        }
                        Value::Operand(Operand::Const(c)) => {
                            consts.insert(var.0, c.0);
                        }
                        _ => {}
                    }
                }
                Term::Join { params, .. } => {
                    for v in params {
                        *bound.entry(v.0).or_default() += 1;
                    }
                }
                _ => {}
            }
        }
        if prims.is_empty() {
            continue;
        }
        // Every use of a variable, and those that are calls with the key.
        let mut uses: HashMap<u32, (u32, u32)> = HashMap::new();
        let mut note = |op: &Operand, good: bool| {
            if let Operand::Var(v) = op {
                let u = uses.entry(v.0).or_default();
                u.0 += 1;
                u.1 += u32::from(good);
            }
        };
        let mut call = |callee: &Operand, args: &[Operand]| {
            // `%syntax-match` takes the syntax it matches before the unit.
            let good = match callee {
                Operand::Var(v) => prims.get(&v.0).is_some_and(|name| {
                    let at = usize::from(*name == SYNTAX_MATCH);
                    args.get(at).is_some_and(|a| is_key(a, &consts))
                }),
                Operand::Const(_) => false,
            };
            note(callee, good);
            for a in args {
                note(a, false);
            }
        };
        for term in &function.terms {
            match term {
                Term::Let { value, .. } => match value {
                    Value::Operand(a) | Value::Box(a) | Value::Unbox(a) => {
                        call(a, &[]);
                    }
                    Value::Primitive { args, .. } | Value::CallDirect { args, .. } => {
                        for a in args {
                            call(a, &[]);
                        }
                    }
                    Value::Call { callee, args } => call(callee, args),
                    Value::Closure { free, .. } => {
                        for a in free {
                            call(a, &[]);
                        }
                    }
                    Value::Global(_) => {}
                    Value::SetGlobal(_, a) => call(a, &[]),
                    Value::SetBox(a, b) => {
                        call(a, &[]);
                        call(b, &[]);
                    }
                    Value::SetFree { closure, value, .. } => {
                        call(closure, &[]);
                        call(value, &[]);
                    }
                },
                Term::Join { .. } => {}
                Term::Jump { args, .. } | Term::TailCallDirect { args, .. } => {
                    for a in args {
                        call(a, &[]);
                    }
                }
                Term::If { test, .. } => call(test, &[]),
                Term::Return(a) => call(a, &[]),
                Term::TailCall { callee, args, .. } => call(callee, args),
            }
        }
        for (var, name) in prims {
            let (total, good) = uses.get(&var).copied().unwrap_or_default();
            // A variable bound twice might be a primitive at one binding
            // and anything at the other.
            if bound[&var] != 1 || total != good {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// The first of `ir`'s globals that is an isolate global, a REPL's variable:
/// no library's code may link one, since it would read or assign what the
/// host and other code keep there.
fn links_repl(ir: &vali_dist::Module) -> Option<String> {
    ir.globals
        .iter()
        .find(|g| matches!(g.link, vali_dist::GlobalLink::Repl))
        .map(|g| g.name.to_string())
}

/// The scopes `syntax` binds its own definitions and macros under, other
/// than the core forms': what a library's own identifiers carry, which
/// what it imported never does.
fn own_scopes(syntax: &ContextState, index: usize) -> HashSet<Scope> {
    let mut scopes = HashSet::new();
    for (_, set, _, binding) in syntax.table.entries() {
        let own = match binding {
            Binding::Variable(owner, _) => *owner == index,
            Binding::Macro(id) => id.unit as usize == index,
            _ => false,
        };
        if own {
            scopes.extend(syntax.context.scopes(set).iter().copied());
        }
    }
    scopes.remove(&syntax.core_scope);
    scopes
}

/// What importing a library brought in, as `reproduces` matches it: the
/// scope sets its own syntax export binds each symbol to each of its
/// variables under.
#[derive(Default)]
struct Brought<'a> {
    bound: HashMap<(&'a str, &'a str), Vec<ScopeSetId>>,
}

impl<'a> Brought<'a> {
    fn of(owner_index: usize, owner: &'a ContextState) -> Self {
        let mut brought = Brought::default();
        for (symbol, set, _, binding) in owner.table.entries() {
            if let Binding::Variable(o, name) = binding
                && *o == owner_index
            {
                brought
                    .bound
                    .entry((symbol.as_str(), name.as_str()))
                    .or_default()
                    .push(set);
            }
        }
        brought
    }
}

/// How many scopes of each kind `set` holds in `state`, apart from the core
/// forms' scope, and whether it holds that.
fn shape(state: &ContextState, set: ScopeSetId) -> ([usize; 5], bool) {
    let mut kinds = [0; 5];
    let mut core = false;
    for s in state.context.scopes(set) {
        if s == state.core_scope {
            core = true;
            continue;
        }
        kinds[match state.context.kind(s) {
            ScopeKind::Macro => 0,
            ScopeKind::UseSite => 1,
            ScopeKind::Binder => 2,
            ScopeKind::Library => 3,
            ScopeKind::Other => 4,
        }] += 1;
    }
    (kinds, core)
}

/// Whether `symbol` bound under `set` in `syntax` to the variable `name` of
/// `owner`, one it does not export, is what importing `owner` brought in:
/// `owner`'s own syntax export binds that name to that variable under a set
/// whose scopes could map onto this one's, one to one, by kind, with the
/// core forms' scope the same (`remap_export`). Each entry is matched on its
/// own, by the number of scopes of each kind: which scope of a kind maps to
/// which is not recoverable (a remap numbers scopes as it meets them, not in
/// the owner's order), and an entry is never refused for a pairing that
/// another could contradict. The scopes a library's own definitions are
/// bound under are not a mapping of anyone's.
///
/// This keeps an export's bindings honest; it does not keep a variable
/// private. A crafted artifact can add a scope that binds none of its
/// definitions, so it is not in `own_scopes`, and bind the variable under
/// it: that matches the owner's entry as a real import's scope does, and
/// the loader cannot tell the two apart. What keeps the variable private is
/// that the owner neither exports nor publishes it, so no code of another
/// library may link it: the loader refuses a link (`check_links`), and the
/// expander refuses a reference to it (`Expander::variable`; ADR 0012 #9).
fn reproduces(
    owner: &ContextState,
    syntax: &ContextState,
    own_scopes: &HashSet<Scope>,
    brought: &Brought<'_>,
    symbol: &str,
    set: ScopeSetId,
    name: &str,
) -> bool {
    if syntax
        .context
        .scopes(set)
        .iter()
        .any(|s| own_scopes.contains(s))
    {
        return false;
    }
    let Some(candidates) = brought.bound.get(&(symbol, name)) else {
        return false;
    };
    let here = shape(syntax, set);
    candidates
        .iter()
        .any(|&theirs| shape(owner, theirs) == here)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Entries<'a> = &'a [(&'a str, &'a str, &'a [usize])];

    /// A state whose table binds `entries`, each a symbol, a variable name
    /// and the scopes, by number from 1, of the kinds `kinds` made, in the
    /// order `order` numbers them.
    fn state(kinds: &[ScopeKind], order: &[usize], entries: Entries) -> ContextState {
        let mut state = ContextState::fresh().unwrap();
        let made: Vec<Scope> = order
            .iter()
            .map(|&i| state.context.new_scope(kinds[i - 1]).unwrap())
            .collect();
        for (symbol, name, scopes) in entries {
            let mut all = vec![state.core_scope];
            all.extend(
                scopes
                    .iter()
                    .map(|&i| made[order.iter().position(|&o| o == i).unwrap()]),
            );
            let set = state.context.intern(&all).unwrap();
            state
                .table
                .bind(
                    &state.context,
                    (*symbol).to_owned(),
                    set,
                    Phases::All,
                    Binding::Variable(0, (*name).to_owned()),
                )
                .unwrap();
        }
        state
    }

    fn holds(owner: &ContextState, syntax: &ContextState) -> bool {
        let brought = Brought::of(0, owner);
        syntax.table.entries().all(|(symbol, set, _, binding)| {
            // The core forms `fresh` binds are not what is tested.
            let Binding::Variable(_, name) = binding else {
                return true;
            };
            reproduces(owner, syntax, &HashSet::new(), &brought, symbol, set, name)
        })
    }

    #[test]
    fn same_kind_scopes_may_be_numbered_in_the_opposite_order() {
        // The owner binds `one` under two macro scopes and `two` under the
        // first of them; an importer numbered them the other way round.
        let kinds = [ScopeKind::Macro, ScopeKind::Macro];
        let entries: Entries = &[("one", "p", &[1, 2]), ("two", "q", &[1])];
        let owner = state(&kinds, &[1, 2], entries);
        assert!(holds(&owner, &state(&kinds, &[1, 2], entries)));
        assert!(holds(&owner, &state(&kinds, &[2, 1], entries)));
    }

    #[test]
    fn a_binding_the_owner_does_not_have_is_not_reproduced() {
        let kinds = [ScopeKind::Macro, ScopeKind::Binder];
        let owner = state(&kinds, &[1, 2], &[("one", "p", &[1, 2])]);
        // Another variable, another symbol, another number of scopes.
        for entries in [
            [("one", "other", &[1, 2][..])],
            [("another", "p", &[1, 2][..])],
            [("one", "p", &[1][..])],
        ] {
            let fake = state(&kinds, &[1, 2], &entries);
            assert!(!holds(&owner, &fake), "{entries:?}");
        }
        // Another kind of scope.
        let kinds = [ScopeKind::Macro, ScopeKind::Macro];
        let fake = state(&kinds, &[1, 2], &[("one", "p", &[1, 2])]);
        assert!(!holds(&owner, &fake));
    }
}
