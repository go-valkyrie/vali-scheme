//! Libraries, programs and REPL inputs as units: compiling one, its
//! module, interface and imports, and its artifact (ADR 0001 #5).

use super::*;

/// A library or the program, as it is compiled.
pub(super) struct Unit {
    pub(super) identity: String,
    /// What it exports, by external name.
    pub(super) interface: HashMap<String, Binding>,
    /// The units it imports, which its instances need first.
    pub(super) imports: Vec<usize>,
    /// Its compiled module, once there is one.
    pub(super) module: Option<Module>,
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
    pub(super) export_hash: Option<u64>,
    /// Whether its syntax export holds only its own macros and compiled
    /// syntax, as one loaded from an artifact does: importing it imports
    /// its imports' too.
    pub(super) pruned: bool,
    /// Whether it was compiled, then written and loaded again
    /// (`round_trip`).
    pub(super) round_tripped: bool,
}

/// The sections a library's artifact has, all required.
pub(super) const LIBRARY_SECTIONS: &[&str] = &["build", "library", "syntax", "invoke", "depends"];

/// What compiled an artifact: one Vali reads only its own (ADR 0001 #8).
pub(super) fn build_stamp() -> String {
    format!(
        "vali-compiler {} format {}",
        env!("CARGO_PKG_VERSION"),
        vali_dist::FORMAT_VERSION
    )
}

/// The FNV-1a hash of `bytes`.
pub(super) fn bytes_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `ir` converted to run-format bytecode, and validated.
pub(super) fn converted(ir: &vali_dist::Module) -> Result<Module> {
    let fail = |message: String| CompileError {
        message,
        span: None,
        location: None,
    };
    let module = vali_bytecode::convert(ir).map_err(|e| fail(format!("{}: {e}", ir.name)))?;
    validate(&module).map_err(|e| {
        fail(format!(
            "{}: conversion produced invalid bytecode: {e}",
            ir.name
        ))
    })?;
    Ok(module)
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
            interface,
            imports: Vec::new(),
            module: Some(module.clone()),
            own: false,
            repl: false,
            syntax: None,
            key: identity.to_owned(),
            ir: None,
            export_hash: None,
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
        let placeholder = ContextState {
            context: ScopeContext::new(),
            table: BindingTable::new(),
            core_scope: self.core_scope,
            core: self.core,
            macros: HashMap::new(),
            compiled: Default::default(),
            imported: HashSet::new(),
            next_macro: 0,
        };
        let state = self.swap_state(placeholder);
        match owner {
            ContextOwner::Unit(unit) => self.units[unit].syntax = Some(Arc::new(state)),
            ContextOwner::Repl => self.repl_state = Some(state),
        }
        Ok(())
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
        let mut depends = vali_dist::Writer::new();
        let imports = self.units[unit].imports.clone();
        depends.usize(imports.len());
        for dependency in imports {
            depends.str(&self.units[dependency].identity.clone());
            let hash = self.export_hash(dependency)?;
            depends.uint(hash);
        }
        let mut artifact = vali_dist::Artifact::default();
        artifact.insert("build", true, build_stamp().into_bytes());
        artifact.insert("library", true, library);
        artifact.insert("syntax", true, syntax);
        artifact.insert("invoke", true, invoke);
        artifact.insert("depends", true, depends.finish());
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
        };
        artifact::encode_library(&record, &identities).map_err(|message| CompileError {
            message: format!("{}: {message}", u.identity),
            span: None,
            location: None,
        })
    }

    /// A hash of what importers of `unit` observe: its `library` and
    /// `syntax` sections, as this build would write them.
    pub(super) fn export_hash(&mut self, unit: usize) -> Result<u64> {
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
        let hash = bytes_hash(&bytes);
        self.units[unit].export_hash = Some(hash);
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
    ) -> Result<Module> {
        let fail = |message: String| CompileError {
            message: format!("{identity}: {message}"),
            span: None,
            location: None,
        };
        let artifact = vali_dist::Artifact::decode(bytes, LIBRARY_SECTIONS)
            .map_err(|e| fail(e.to_string()))?;
        let section = |name: &str| {
            artifact
                .section(name)
                .ok_or_else(|| fail(format!("the artifact has no {name} section")))
        };
        if section("build")? != build_stamp().as_bytes() {
            return Err(fail("the artifact is from another build of Vali".into()));
        }
        let mut depends = vali_dist::Reader::new(section("depends")?);
        let read = (|| {
            let n = depends.count(2)?;
            let mut needed = Vec::with_capacity(n);
            for _ in 0..n {
                let dependency = depends.str()?.to_owned();
                needed.push((dependency, depends.uint()?));
            }
            depends.end()?;
            Ok::<_, vali_dist::DecodeError>(needed)
        })()
        .map_err(|e| fail(format!("its dependencies: {e}")))?;
        for (dependency, hash) in read {
            let unit = self
                .unit_of(&dependency)
                .ok_or_else(|| fail(format!("it needs {dependency}, which is not loaded")))?;
            if self.export_hash(unit)? != hash {
                return Err(fail(format!(
                    "{dependency} has changed since it was compiled"
                )));
            }
        }
        let ir = vali_dist::decode(section("invoke")?).map_err(|e| fail(e.to_string()))?;
        if !own
            && let Some(global) = ir.globals.iter().find(|g| {
                matches!(g.link, vali_dist::GlobalLink::Primitive) && g.name.starts_with('%')
                    || matches!(g.link, vali_dist::GlobalLink::Native(_))
            })
        {
            return Err(fail(format!(
                "it links {}, which only Vali's own libraries may",
                global.name
            )));
        }
        let module = converted(&ir)?;
        // The unit goes in first: what it exports names it.
        let index = self.units.len();
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface: HashMap::new(),
            imports: Vec::new(),
            module: Some(module.clone()),
            own,
            repl: false,
            syntax: None,
            key: identity.to_owned(),
            ir: Some(Arc::new(ir)),
            export_hash: None,
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
            let unit = &mut self.units[index];
            unit.key = record.key;
            unit.imports = record.imports;
            unit.interface = record.interface;
            unit.syntax = Some(Arc::new(syntax));
            // Writing it again would give these bytes, so they are what its
            // importers' artifacts hash, without encoding it again.
            let mut observed = section("library")?.to_vec();
            observed.extend_from_slice(section("syntax")?);
            unit.export_hash = Some(bytes_hash(&observed));
            Ok(())
        })();
        if let Err(e) = filled {
            self.units.pop();
            return Err(e);
        }
        Ok(module)
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
            return self.unit(identity, imports, imported, top, body, &[], span, false);
        }
        let top = self.context.intern(&[scope]).map_err(too_many)?;
        self.unit(identity, imports, top, top, body, &[], span, false)
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
        self.unit(identity, imports, imported, top, body, &[], span, true)
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
    ) -> Result<Module> {
        let index = self.units.len();
        // A new unit expands with fresh expansion-time instances, and
        // numbers what it makes from zero, so its output depends only on
        // itself (ADR 0001 #4).
        self.phases.clear();
        self.generation += 1;
        self.fresh = 0;
        let imported_at = imported;
        let mut imported = Vec::new();
        for set in imports {
            if let Some(&unit) = self.by_name.get(set.library()) {
                imported.push(unit);
            }
        }
        self.units.push(Unit {
            identity: identity.to_owned(),
            interface: HashMap::new(),
            imports: imported,
            module: None,
            own: std::mem::take(&mut self.next_own),
            repl,
            syntax: None,
            key: if std::mem::take(&mut self.next_hashed) {
                format!("{identity}@{:016x}", text_hash(&format!("{body:?}")))
            } else {
                identity.to_owned()
            },
            ir: None,
            export_hash: None,
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
        let module = self.module(lambda, identity)?;
        let ir = self.last_ir.clone().map(Arc::new);
        let unit = &mut self.units[index];
        unit.interface = interface;
        unit.module = Some(module.clone());
        unit.ir = ir;
        Ok(module)
    }

    /// Compiles the `lambda` `body` as a module's body, exporting every
    /// variable the module defines under its own name. Everything
    /// compiled goes through the IR (ADR 0017): the tree is
    /// lowered to it, then converted to run-format bytecode.
    pub(super) fn module(&mut self, body: LambdaId, name: &str) -> Result<Module> {
        let lowered = self.lower_only(body, name)?;
        let mut ir = lowered.clone();
        self.optimised(&mut ir);
        let module = converted(&ir)?;
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
            ImportSet::Library(name) => self.interface(name, span)?,
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
                span,
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
