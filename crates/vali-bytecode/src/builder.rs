//! Building functions and modules in Rust, with labels for branches. Used
//! by tests and benchmarks until the compiler exists, and by the compiler
//! after.

use std::collections::HashMap;

use crate::instruction::{Instruction, JUMP_RANGE};
use crate::module::{
    Arity, Constant, Export, Function, Inlined, Link, Module, Position, Scope, Variable,
};

/// A position in a function's code, bound once and used by any number of
/// branches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(usize);

/// What went wrong building a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// A label was used but never bound.
    UnboundLabel(Label),
    /// A branch's target is too far away for its offset field.
    OffsetOutOfRange,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::UnboundLabel(label) => write!(f, "label {} is never bound", label.0),
            BuildError::OffsetOutOfRange => f.write_str("a branch target is out of range"),
        }
    }
}

impl std::error::Error for BuildError {}

#[derive(Debug)]
struct Fixup {
    /// The index of the instruction's first word.
    at: usize,
    /// The index just past the instruction.
    end: usize,
    label: Label,
}

/// Builds one function's code.
#[derive(Debug)]
pub struct FunctionBuilder {
    name: Option<Box<str>>,
    arity: Arity,
    slots: u16,
    free: u16,
    code: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
    positions: Vec<Position>,
    inlined: Vec<Inlined>,
    scopes: Vec<Scope>,
}

impl FunctionBuilder {
    /// A function named `name`, taking `arity`, with a frame of `slots`
    /// slots and `free` free variables.
    pub fn new(name: Option<&str>, arity: Arity, slots: u16, free: u16) -> FunctionBuilder {
        FunctionBuilder {
            name: name.map(Into::into),
            arity,
            slots,
            free,
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            positions: Vec::new(),
            inlined: Vec::new(),
            scopes: Vec::new(),
        }
    }

    /// Notes that the next instruction, and those after it, came from
    /// `line` and `column` of the module's source `source`.
    pub fn position(&mut self, source: u16, line: u32, column: u32) -> &mut Self {
        let pc = u32::try_from(self.code.len()).expect("a function under 4 G instructions");
        let position = Position {
            pc,
            source,
            line,
            column,
        };
        match self.positions.last_mut() {
            // A later position for the same instruction replaces the earlier.
            Some(last) if last.pc == pc => *last = position,
            Some(last) if (last.source, last.line, last.column) == (source, line, column) => {}
            _ => self.positions.push(position),
        }
        self
    }

    /// Notes that the next instruction, and those after it, are in copy
    /// `inlined` of another function: 0 for this function's own code,
    /// `i + 1` for the `i`th copy [`FunctionBuilder::inline`] added.
    pub fn scope(&mut self, inlined: u32) -> &mut Self {
        let pc = u32::try_from(self.code.len()).expect("a function under 4 G instructions");
        let current = self.scopes.last().map_or(0, |s| s.inlined);
        match self.scopes.last_mut() {
            Some(last) if last.pc == pc => last.inlined = inlined,
            _ if current == inlined => {}
            _ => self.scopes.push(Scope { pc, inlined }),
        }
        self
    }

    /// Adds a copy of another function inlined into this one.
    pub fn inline(&mut self, copy: Inlined) -> &mut Self {
        self.inlined.push(copy);
        self
    }

    /// A new, unbound label.
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Binds `label` to the next instruction.
    ///
    /// # Panics
    ///
    /// If the label is already bound.
    pub fn bind(&mut self, label: Label) {
        assert!(self.labels[label.0].is_none(), "label bound twice");
        self.labels[label.0] = Some(self.code.len());
    }

    /// Appends an instruction that does not branch.
    pub fn emit(&mut self, instruction: Instruction) -> &mut Self {
        instruction.encode(&mut self.code);
        self
    }

    /// Appends a branching instruction, whose offset is filled in to reach
    /// `target` when the function is finished. `instruction`'s own offset
    /// is ignored.
    pub fn branch(&mut self, instruction: Instruction, target: Label) -> &mut Self {
        let mut at = self.code.len();
        instruction.encode(&mut self.code);
        // The offset is the instruction's, after any `wide` prefix.
        if self.code[at] as u8 == crate::Opcode::Wide as u8 {
            at += 1;
        }
        self.fixups.push(Fixup {
            at,
            end: self.code.len(),
            label: target,
        });
        self
    }

    /// Appends a jump to `target`.
    pub fn jump(&mut self, target: Label) -> &mut Self {
        self.branch(Instruction::Jump { offset: 0 }, target)
    }

    /// The finished function.
    pub fn finish(mut self) -> Result<Function, BuildError> {
        for fixup in &self.fixups {
            let target = self.labels[fixup.label.0].ok_or(BuildError::UnboundLabel(fixup.label))?;
            let offset = i32::try_from(target as i64 - fixup.end as i64)
                .map_err(|_| BuildError::OffsetOutOfRange)?;
            let first = self.code[fixup.at];
            if fixup.end - fixup.at == 1 {
                // A jump: the offset is the 24-bit field of the first word.
                if !JUMP_RANGE.contains(&offset) {
                    return Err(BuildError::OffsetOutOfRange);
                }
                self.code[fixup.at] = (first & 0xFF) | ((offset as u32) & 0xFF_FFFF) << 8;
            } else {
                self.code[fixup.at + 1] = offset as u32;
            }
        }
        // A position noted after the last instruction covers nothing.
        let end = self.code.len();
        self.positions.retain(|p| (p.pc as usize) < end);
        self.scopes.retain(|s| (s.pc as usize) < end);
        // A function stays in memory as long as its module does, and
        // `Vec` growth leaves up to half of each buffer empty.
        self.code.shrink_to_fit();
        self.positions.shrink_to_fit();
        self.inlined.shrink_to_fit();
        self.scopes.shrink_to_fit();
        Ok(Function {
            name: self.name,
            arity: self.arity,
            slots: self.slots,
            free: self.free,
            code: self.code,
            positions: self.positions,
            inlined: self.inlined,
            scopes: self.scopes,
        })
    }
}

/// Builds a module: its constants, variables and functions.
#[derive(Debug, Default)]
pub struct ModuleBuilder {
    name: Box<str>,
    constants: Vec<Constant>,
    /// The pool's shareable constants, by [`shallow_hash`].
    constant_index: HashMap<u64, Vec<u16>>,
    variables: Vec<Variable>,
    variable_index: HashMap<Variable, u16>,
    exports: Vec<Export>,
    functions: Vec<Option<Function>>,
    sources: Vec<Box<str>>,
    source_index: HashMap<Box<str>, u16>,
}

impl ModuleBuilder {
    /// An empty module named `name`.
    pub fn new(name: &str) -> ModuleBuilder {
        ModuleBuilder {
            name: name.into(),
            ..ModuleBuilder::default()
        }
    }

    /// The index of `constant`, adding it if it is new. Equal constants share
    /// an entry, except pairs and vectors: each is a mutable object, and
    /// two equal quoted lists are two objects, so changing one must not
    /// change the other.
    ///
    /// # Panics
    ///
    /// If the pool would exceed 65,536 constants.
    pub fn constant(&mut self, constant: Constant) -> u16 {
        self.try_constant(constant)
            .expect("at most 65,536 constants")
    }

    /// As [`ModuleBuilder::constant`], but `None` if the pool is full and
    /// `constant` is not already in it. Finding an equal constant takes time
    /// in the constant's size, not the pool's.
    pub fn try_constant(&mut self, constant: Constant) -> Option<u16> {
        let mutable = matches!(
            constant,
            Constant::Pair(..) | Constant::Vector(_) | Constant::Labelled(..)
        );
        let hash = if mutable {
            None
        } else {
            Some(shallow_hash(&constant))
        };
        if let Some(hash) = hash
            && let Some(found) = self.constant_index.get(&hash).and_then(|bucket| {
                bucket
                    .iter()
                    .copied()
                    .find(|&i| identical(&self.constants[usize::from(i)], &constant))
            })
        {
            return Some(found);
        }
        let index = u16::try_from(self.constants.len()).ok()?;
        self.constants.push(constant);
        if let Some(hash) = hash {
            self.constant_index.entry(hash).or_default().push(index);
        }
        Some(index)
    }

    /// The index of the global variable named `name`, adding it if it is
    /// new.
    ///
    /// # Panics
    ///
    /// If the module would exceed 65,536 variables.
    pub fn variable(&mut self, name: &str) -> u16 {
        self.linked(name, Link::Global)
    }

    /// The index of the variable `name` that this module defines, adding
    /// it if it is new.
    ///
    /// # Panics
    ///
    /// As [`ModuleBuilder::variable`].
    pub fn defined(&mut self, name: &str) -> u16 {
        self.linked(name, Link::Defined)
    }

    /// The index of the variable that the library `library` exports as
    /// `name`, adding it if it is new.
    ///
    /// # Panics
    ///
    /// As [`ModuleBuilder::variable`].
    pub fn imported(&mut self, library: &str, name: &str) -> u16 {
        self.linked(name, Link::Imported(library.into()))
    }

    /// The index of the variable holding Vali's primitive `name`, adding
    /// it if it is new.
    ///
    /// # Panics
    ///
    /// Panics past 65,536 variables.
    pub fn primitive(&mut self, name: &str) -> u16 {
        self.linked(name, Link::Primitive)
    }

    /// The index of the variable holding the function `name` of the native
    /// library `library`, adding it if it is new.
    ///
    /// # Panics
    ///
    /// Panics past 65,536 variables.
    pub fn native(&mut self, library: &str, name: &str) -> u16 {
        self.linked(name, Link::Native(library.into()))
    }

    fn linked(&mut self, name: &str, link: Link) -> u16 {
        let variable = Variable {
            name: name.into(),
            link,
        };
        if let Some(&index) = self.variable_index.get(&variable) {
            return index;
        }
        let index = u16::try_from(self.variables.len()).expect("at most 65,536 variables");
        self.variables.push(variable.clone());
        self.variable_index.insert(variable, index);
        index
    }

    /// Exports the variable at `index` as `name`.
    pub fn export(&mut self, name: &str, variable: u16) -> &mut Self {
        self.exports.push(Export {
            name: name.into(),
            variable,
        });
        self
    }

    /// Reserves an index for a function defined later with
    /// [`ModuleBuilder::define`], so functions can refer to each other.
    pub fn declare(&mut self) -> u32 {
        self.functions.push(None);
        (self.functions.len() - 1) as u32
    }

    /// Defines the function at a declared index.
    ///
    /// # Panics
    ///
    /// If the index was not declared, or is already defined.
    pub fn define(&mut self, index: u32, function: Function) {
        let slot = &mut self.functions[index as usize];
        assert!(slot.is_none(), "function {index} defined twice");
        *slot = Some(function);
    }

    /// The index of the source `identity` in the module's table, added if
    /// it is not there yet.
    pub fn source(&mut self, identity: &str) -> u16 {
        if let Some(&i) = self.source_index.get(identity) {
            return i;
        }
        let index = u16::try_from(self.sources.len()).expect("at most 65,536 sources");
        self.sources.push(identity.into());
        self.source_index.insert(identity.into(), index);
        index
    }

    /// Adds a function, returning its index.
    pub fn function(&mut self, function: Function) -> u32 {
        self.functions.push(Some(function));
        (self.functions.len() - 1) as u32
    }

    /// The finished module, whose body is function `body`.
    ///
    /// # Panics
    ///
    /// If a declared function was never defined.
    pub fn finish(mut self, body: u32) -> Module {
        // As for a function: a module that is evaluated and kept loaded
        // pays for every spare slot, which for a small expression is
        // most of its bytes.
        self.constants.shrink_to_fit();
        self.variables.shrink_to_fit();
        self.exports.shrink_to_fit();
        self.sources.shrink_to_fit();
        let mut functions: Vec<Function> = self
            .functions
            .into_iter()
            .enumerate()
            .map(|(i, f)| f.unwrap_or_else(|| panic!("function {i} declared but not defined")))
            .collect();
        functions.shrink_to_fit();
        Module {
            name: self.name,
            constants: self.constants,
            variables: self.variables,
            exports: self.exports,
            functions,
            sources: self.sources,
            body,
            digest: None,
        }
    }
}

/// A hash that [`identical`] constants share: of a constant's own value,
/// and of the tag alone for the parts that are mutable objects. Iterative,
/// like [`identical`].
fn shallow_hash(constant: &Constant) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut pending = vec![constant];
    while let Some(c) = pending.pop() {
        std::mem::discriminant(c).hash(&mut h);
        match c {
            Constant::Integer(n) => n.hash(&mut h),
            Constant::Flonum(x) => x.to_bits().hash(&mut h),
            Constant::Boolean(b) => b.hash(&mut h),
            Constant::Character(ch) => ch.hash(&mut h),
            Constant::String(s) | Constant::Symbol(s) | Constant::Keyword(s) => s.hash(&mut h),
            Constant::Bignum { negative, limbs } => {
                negative.hash(&mut h);
                limbs.hash(&mut h);
            }
            Constant::Bytevector(bytes) => bytes.hash(&mut h),
            Constant::Ratio(a, b) | Constant::Complex(a, b) => {
                pending.push(b);
                pending.push(a);
            }
            Constant::LabelRef(n) => n.hash(&mut h),
            Constant::EmptyList
            | Constant::Unspecified
            | Constant::Pair(..)
            | Constant::Vector(_)
            | Constant::Labelled(..) => {}
        }
    }
    h.finish()
}

/// Whether two constants are the same value, so that one pool entry can
/// stand for both. Unlike `==`, flonums compare by their bits: `-0.0` and
/// `0.0` are different constants, as `eqv?` tells them apart. It compares
/// from a stack of its own, so deeply nested constants cannot overflow the
/// native stack.
fn identical(a: &Constant, b: &Constant) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (Constant::Flonum(x), Constant::Flonum(y)) => {
                if x.to_bits() != y.to_bits() {
                    return false;
                }
            }
            (Constant::Pair(a1, a2), Constant::Pair(b1, b2))
            | (Constant::Ratio(a1, a2), Constant::Ratio(b1, b2))
            | (Constant::Complex(a1, a2), Constant::Complex(b1, b2)) => {
                pending.push((a1, b1));
                pending.push((a2, b2));
            }
            (Constant::Vector(xs), Constant::Vector(ys)) => {
                if xs.len() != ys.len() {
                    return false;
                }
                pending.extend(xs.iter().zip(ys));
            }
            (Constant::Pair(..) | Constant::Ratio(..) | Constant::Complex(..), _)
            | (Constant::Vector(_), _)
            | (Constant::Flonum(_), _) => return false,
            _ => {
                if a != b {
                    return false;
                }
            }
        }
    }
    true
}
