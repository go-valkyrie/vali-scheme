//! The checks that make a module well formed.
//!
//! A module that passes can be converted to run-format bytecode without
//! further checks of its structure: every index is in range, every
//! variable and join point is bound once and used only where it is in
//! scope, every term is part of its function's body exactly once, and
//! every operation has the operands it takes. What only running can tell,
//! such as whether a callee is a procedure or a `set-free` index is within
//! its closure, conversion checks at run time.
//!
//! The walk is iterative: no input can make it recurse.

use std::fmt;

use crate::ir::{
    Constant, FunId, Function, GlobalLink, JoinId, Module, Operand, Position, Term, TermId, Value,
    Var,
};

/// Why a module is not well formed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// The function the problem is in, if it is in one.
    pub function: Option<FunId>,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.function {
            Some(FunId(n)) => write!(f, "function f{n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Checks that `module` is well formed.
pub fn validate(module: &Module) -> Result<(), ValidationError> {
    let fail = |message: String| ValidationError {
        function: None,
        message,
    };
    for (n, constant) in module.constants.iter().enumerate() {
        check_constant(constant).map_err(|why| fail(format!("constant c{n}: {why}")))?;
    }
    let body = module
        .functions
        .get(module.body.0 as usize)
        .ok_or_else(|| fail(format!("the body f{} is not a function", module.body.0)))?;
    if body.arity.parameters() != 0 || !body.free.is_empty() {
        return Err(fail(format!(
            "the body f{} takes arguments or free variables",
            module.body.0
        )));
    }
    for (name, global) in &module.exports {
        if global.0 as usize >= module.globals.len() {
            return Err(fail(format!("export {name} names no global")));
        }
    }
    let mut names: Vec<&str> = module.exports.iter().map(|(name, _)| &**name).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(fail(format!("{} is exported twice", pair[0])));
    }
    for (n, function) in module.functions.iter().enumerate() {
        let id = FunId(n as u32);
        Checker::new(module, function)
            .run()
            .map_err(|message| ValidationError {
                function: Some(id),
                message,
            })?;
    }
    Ok(())
}

/// Checks a constant's canonical form, iteratively.
fn check_constant(constant: &Constant) -> Result<(), String> {
    let mut pending = vec![constant];
    while let Some(constant) = pending.pop() {
        match constant {
            Constant::Bignum { negative, limbs } => {
                let fits = match limbs[..] {
                    [] => true,
                    [limb] => limb <= i64::MAX as u64 + u64::from(*negative),
                    [.., top] => top == 0,
                };
                if fits {
                    return Err(
                        "a bignum must need more than 64 bits, with no zero top limb".into(),
                    );
                }
            }
            Constant::Ratio(numerator, denominator) => {
                if !is_integer(numerator) || !is_integer(denominator) {
                    return Err("a ratio's parts must be integers".into());
                }
                match (&**numerator, &**denominator) {
                    (_, Constant::Integer(d)) if *d <= 1 => {
                        return Err("a ratio's denominator must be above 1".into());
                    }
                    (_, Constant::Bignum { negative: true, .. }) => {
                        return Err("a ratio's denominator must be positive".into());
                    }
                    (Constant::Integer(n), Constant::Integer(d))
                        if gcd(n.unsigned_abs(), d.unsigned_abs()) != 1 =>
                    {
                        return Err("a ratio must be in lowest terms".into());
                    }
                    _ => {}
                }
                pending.push(numerator);
                pending.push(denominator);
            }
            Constant::Complex(real, imaginary) => {
                let exact = is_exact_real(real) && is_exact_real(imaginary);
                let inexact = matches!(&**real, Constant::Flonum(_))
                    && matches!(&**imaginary, Constant::Flonum(_));
                if !exact && !inexact {
                    return Err(
                        "a complex number's parts must be both exact or both flonums".into(),
                    );
                }
                if exact && matches!(&**imaginary, Constant::Integer(0)) {
                    return Err(
                        "an exact complex number must have a non-zero imaginary part".into(),
                    );
                }
                pending.push(real);
                pending.push(imaginary);
            }
            Constant::Pair(car, cdr) => {
                pending.push(car);
                pending.push(cdr);
            }
            Constant::Vector(items) => pending.extend(items),
            _ => {}
        }
    }
    Ok(())
}

fn is_integer(constant: &Constant) -> bool {
    matches!(constant, Constant::Integer(_) | Constant::Bignum { .. })
}

fn is_exact_real(constant: &Constant) -> bool {
    is_integer(constant) || matches!(constant, Constant::Ratio(..))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// One step of the walk over a function's terms.
enum Task<'a> {
    Enter(TermId),
    Bind(&'a [Var]),
    Unbind(&'a [Var]),
    BindJoin(JoinId, usize),
    UnbindJoin(JoinId),
}

/// The state of the walk over one function.
struct Checker<'a> {
    module: &'a Module,
    function: &'a Function,
    /// Whether each variable has been bound, and whether it is in scope.
    defined: Vec<bool>,
    visible: Vec<bool>,
    /// Whether each join point has been bound, and its parameter count
    /// while it is in scope.
    joins_defined: Vec<bool>,
    joins_visible: Vec<Option<usize>>,
    /// Whether each term has been reached.
    seen: Vec<bool>,
}

impl<'a> Checker<'a> {
    fn new(module: &'a Module, function: &'a Function) -> Checker<'a> {
        Checker {
            module,
            function,
            defined: vec![false; function.vars as usize],
            visible: vec![false; function.vars as usize],
            joins_defined: vec![false; function.joins as usize],
            joins_visible: vec![None; function.joins as usize],
            seen: vec![false; function.terms.len()],
        }
    }

    fn run(mut self) -> Result<(), String> {
        let function = self.function;
        if function.params.len() != function.arity.parameters() {
            return Err(format!(
                "{} parameters for an arity that binds {}",
                function.params.len(),
                function.arity.parameters()
            ));
        }
        self.define(&function.params)?;
        self.define(&function.free)?;
        self.set_visible(&function.params, true);
        self.set_visible(&function.free, true);
        let mut tasks = vec![Task::Enter(function.body)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Enter(id) => self.enter(id, &mut tasks)?,
                Task::Bind(vars) => self.set_visible(vars, true),
                Task::Unbind(vars) => self.set_visible(vars, false),
                Task::BindJoin(join, arity) => self.joins_visible[join.0 as usize] = Some(arity),
                Task::UnbindJoin(join) => self.joins_visible[join.0 as usize] = None,
            }
        }
        if let Some(n) = self.seen.iter().position(|seen| !seen) {
            return Err(format!("term t{n} is not part of the body"));
        }
        Ok(())
    }

    fn enter(&mut self, id: TermId, tasks: &mut Vec<Task<'a>>) -> Result<(), String> {
        let n = id.0 as usize;
        match self.seen.get(n) {
            None => return Err(format!("term t{n} does not exist")),
            Some(true) => return Err(format!("term t{n} is part of the body twice")),
            Some(false) => self.seen[n] = true,
        }
        let function = self.function;
        match function.term(id) {
            Term::Let {
                var,
                value,
                body,
                position,
            } => {
                self.value(value)?;
                self.position(position)?;
                let var = std::slice::from_ref(var);
                self.define(var)?;
                tasks.push(Task::Unbind(var));
                tasks.push(Task::Enter(*body));
                tasks.push(Task::Bind(var));
            }
            Term::Join {
                join,
                params,
                recursive,
                handler,
                body,
            } => {
                let slot = self
                    .joins_defined
                    .get_mut(join.0 as usize)
                    .ok_or_else(|| format!("join point j{} is out of range", join.0))?;
                if std::mem::replace(slot, true) {
                    return Err(format!("join point j{} is bound twice", join.0));
                }
                self.define(params)?;
                let arity = params.len();
                tasks.push(Task::UnbindJoin(*join));
                tasks.push(Task::Enter(*body));
                tasks.push(Task::BindJoin(*join, arity));
                if *recursive {
                    tasks.push(Task::UnbindJoin(*join));
                }
                tasks.push(Task::Unbind(params));
                tasks.push(Task::Enter(*handler));
                tasks.push(Task::Bind(params));
                if *recursive {
                    tasks.push(Task::BindJoin(*join, arity));
                }
            }
            Term::Jump { join, args } => {
                let arity = self
                    .joins_visible
                    .get(join.0 as usize)
                    .copied()
                    .flatten()
                    .ok_or_else(|| format!("join point j{} is not in scope", join.0))?;
                if arity != args.len() {
                    return Err(format!(
                        "a jump to j{} with {} arguments for {arity} parameters",
                        join.0,
                        args.len()
                    ));
                }
                self.operands(args)?;
            }
            Term::If {
                test,
                then,
                otherwise,
            } => {
                self.operand(test)?;
                tasks.push(Task::Enter(*otherwise));
                tasks.push(Task::Enter(*then));
            }
            Term::Return(value) => self.operand(value)?,
            Term::TailCall {
                callee,
                args,
                position,
            } => {
                self.operand(callee)?;
                self.operands(args)?;
                self.position(position)?;
            }
            Term::TailCallDirect {
                function,
                args,
                position,
            } => {
                self.direct(*function, args)?;
                self.operands(args)?;
                self.position(position)?;
            }
        }
        Ok(())
    }

    /// Checks that a direct call's `function` exists, has no free
    /// variables, and takes as many arguments as `args`.
    fn direct(&self, function: FunId, args: &[Operand]) -> Result<(), String> {
        let target = self
            .module
            .functions
            .get(function.0 as usize)
            .ok_or_else(|| format!("direct call of f{}, which does not exist", function.0))?;
        if !target.free.is_empty() {
            return Err(format!(
                "direct call of f{}, which has free variables",
                function.0
            ));
        }
        if !target.arity.accepts(args.len()) {
            return Err(format!(
                "direct call of f{} with {} arguments, which it does not take",
                function.0,
                args.len()
            ));
        }
        Ok(())
    }

    fn value(&self, value: &Value) -> Result<(), String> {
        match value {
            Value::Operand(operand) | Value::Box(operand) | Value::Unbox(operand) => {
                self.operand(operand)
            }
            Value::Primitive { op, args } => {
                if args.len() != op.arity() {
                    return Err(format!(
                        "{op} with {} operands for {}",
                        args.len(),
                        op.arity()
                    ));
                }
                self.operands(args)
            }
            Value::Call { callee, args } => {
                self.operand(callee)?;
                self.operands(args)
            }
            Value::CallDirect { function, args } => {
                self.direct(*function, args)?;
                self.operands(args)
            }
            Value::Closure { function, free } => {
                let target = self
                    .module
                    .functions
                    .get(function.0 as usize)
                    .ok_or_else(|| format!("closure of f{}, which does not exist", function.0))?;
                if target.free.len() != free.len() {
                    return Err(format!(
                        "closure of f{} with {} free variables for {}",
                        function.0,
                        free.len(),
                        target.free.len()
                    ));
                }
                self.operands(free)
            }
            Value::Global(global) => self.global(global.0).map(|_| ()),
            Value::SetGlobal(global, operand) => {
                match self.global(global.0)? {
                    GlobalLink::Defined | GlobalLink::Repl => {}
                    _ => return Err(format!("g{} is assigned but not defined here", global.0)),
                }
                self.operand(operand)
            }
            Value::SetBox(a, b) => {
                self.operand(a)?;
                self.operand(b)
            }
            Value::SetFree { closure, value, .. } => {
                self.operand(closure)?;
                self.operand(value)
            }
        }
    }

    fn global(&self, n: u32) -> Result<&'a GlobalLink, String> {
        self.module
            .globals
            .get(n as usize)
            .map(|global| &global.link)
            .ok_or_else(|| format!("global g{n} does not exist"))
    }

    fn operands(&self, operands: &[Operand]) -> Result<(), String> {
        operands
            .iter()
            .try_for_each(|operand| self.operand(operand))
    }

    fn operand(&self, operand: &Operand) -> Result<(), String> {
        match operand {
            Operand::Var(Var(n)) => match self.visible.get(*n as usize) {
                Some(true) => Ok(()),
                _ => Err(format!("v{n} is used where it is not in scope")),
            },
            Operand::Const(c) if (c.0 as usize) < self.module.constants.len() => Ok(()),
            Operand::Const(c) => Err(format!("constant c{} does not exist", c.0)),
        }
    }

    fn position(&self, position: &Option<Position>) -> Result<(), String> {
        match position {
            Some(position) if usize::from(position.source) >= self.module.sources.len() => {
                Err(format!("source {} does not exist", position.source))
            }
            _ => Ok(()),
        }
    }

    fn define(&mut self, vars: &[Var]) -> Result<(), String> {
        for Var(n) in vars {
            let slot = self
                .defined
                .get_mut(*n as usize)
                .ok_or_else(|| format!("v{n} is out of range"))?;
            if std::mem::replace(slot, true) {
                return Err(format!("v{n} is bound twice"));
            }
        }
        Ok(())
    }

    fn set_visible(&mut self, vars: &[Var], visible: bool) {
        for Var(n) in vars {
            self.visible[*n as usize] = visible;
        }
    }
}
