//! Building functions and modules in Rust, with labels for branches. Used
//! by tests and benchmarks until the compiler exists, and by the compiler
//! after.

use std::collections::HashMap;

use crate::instruction::{Instruction, JUMP_RANGE};
use crate::module::{Arity, Constant, Export, Function, Link, Module, Variable};

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
    free: u8,
    code: Vec<u32>,
    labels: Vec<Option<usize>>,
    fixups: Vec<Fixup>,
}

impl FunctionBuilder {
    /// A function named `name`, taking `arity`, with a frame of `slots`
    /// slots and `free` free variables.
    pub fn new(name: Option<&str>, arity: Arity, slots: u16, free: u8) -> FunctionBuilder {
        FunctionBuilder {
            name: name.map(Into::into),
            arity,
            slots,
            free,
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
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
        let at = self.code.len();
        instruction.encode(&mut self.code);
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
        Ok(Function {
            name: self.name,
            arity: self.arity,
            slots: self.slots,
            free: self.free,
            code: self.code,
        })
    }
}

/// Builds a module: its constants, variables and functions.
#[derive(Debug, Default)]
pub struct ModuleBuilder {
    name: Box<str>,
    constants: Vec<Constant>,
    variables: Vec<Variable>,
    variable_index: HashMap<Variable, u16>,
    exports: Vec<Export>,
    functions: Vec<Option<Function>>,
}

impl ModuleBuilder {
    /// An empty module named `name`.
    pub fn new(name: &str) -> ModuleBuilder {
        ModuleBuilder {
            name: name.into(),
            ..ModuleBuilder::default()
        }
    }

    /// The index of `constant`, adding it if it is new.
    ///
    /// # Panics
    ///
    /// If the pool would exceed 65,536 constants.
    pub fn constant(&mut self, constant: Constant) -> u16 {
        let index = match self.constants.iter().position(|c| c == &constant) {
            Some(index) => index,
            None => {
                self.constants.push(constant);
                self.constants.len() - 1
            }
        };
        u16::try_from(index).expect("at most 65,536 constants")
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
    pub fn finish(self, body: u32) -> Module {
        let functions = self
            .functions
            .into_iter()
            .enumerate()
            .map(|(i, f)| f.unwrap_or_else(|| panic!("function {i} declared but not defined")))
            .collect();
        Module {
            name: self.name,
            constants: self.constants,
            variables: self.variables,
            exports: self.exports,
            functions,
            body,
        }
    }
}
