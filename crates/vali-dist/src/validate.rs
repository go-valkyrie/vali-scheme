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
//! It also enforces the closure self-tie rules (ADR 0017 #13): for a
//! variable `c` bound by `let c = closure g(..)`, every `set-free c i c` is an ancestor in the term tree of
//! every other use of `c`, `set-free` names only closures made in its own
//! function, and no other `set-free` writes a self-tied index. Later tiers
//! may rely on these to treat a self-tied slot as the closure itself.
//!
//! Limits that bound the work and memory validation and conversion take by
//! the size of the module in memory:
//!
//! - A function declares no more variables than it binds (its parameters,
//!   free variables, `let`s and join-point parameters) and no more join
//!   points than it has `join` terms, so nothing is allocated for a count
//!   that nothing in the function backs.
//! - No two globals have the same name and link, as they would share one
//!   cell once linked.
//!
//! Limits that the run-format converter works within, so that a module
//! that passes always converts and a conversion failure can only mean a
//! converter bug (ADR 0017 #13):
//!
//! - At most 65,535 constants (conversion adds one of its own to the
//!   65,536 a pool holds), and at most 65,536 globals and sources.
//! - At most 65,535 free variables, call or jump arguments, and a
//!   `set-free` index of at most 65,535.
//! - A function's [`bounds`](crate::bounds()): the highest slot its converted frame can
//!   use, which must be addressable, and the most words its converted code
//!   can take, which must keep every branch offset in range. Both are
//!   bounds computed from the IR that the converter is guaranteed to stay
//!   under, not its exact output, which depends on allocation.
//!
//! The walk is iterative: no input can make it recurse.

use std::collections::{BTreeMap, HashSet};
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
    if module.constants.len() > MAX_CONSTANTS
        || module.globals.len() > MAX_POOL
        || module.sources.len() > MAX_POOL
    {
        return Err(fail(format!(
            "more than {MAX_CONSTANTS} constants or {MAX_POOL} globals or sources"
        )));
    }
    for (n, constant) in module.constants.iter().enumerate() {
        check_constant(constant).map_err(|why| fail(format!("constant c{n}: {why}")))?;
        check_labels(constant).map_err(|why| fail(format!("constant c{n}: {why}")))?;
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
    // Two globals with one name and link would be one cell once linked.
    let mut seen = HashSet::new();
    for (n, global) in module.globals.iter().enumerate() {
        if !seen.insert((&*global.name, &global.link)) {
            return Err(fail(format!(
                "global g{n} is defined twice: {} appears again with the same link",
                global.name
            )));
        }
    }
    for (n, function) in module.functions.iter().enumerate() {
        let id = FunId(n as u32);
        let at = |message: String| ValidationError {
            function: Some(id),
            message,
        };
        check_counts(function).map_err(at)?;
        Checker::new(module, function).run().map_err(at)?;
        check_limits(function).map_err(at)?;
    }
    Ok(())
}

/// The most constants a module can have: the converter adds one of its own
/// to a pool of 65,536.
pub const MAX_CONSTANTS: usize = 65_535;

/// The most globals, or sources, a module can have.
pub const MAX_POOL: usize = 65_536;

/// The highest slot a converted frame can address.
pub const MAX_SLOT: usize = 65_534;

/// The most words of run-format code a function can take: a jump's offset
/// is 24 bits signed, so no branch can span more.
pub const MAX_CODE_WORDS: u64 = (1 << 23) - 1;

/// The most free variables, arguments or `set-free` index run format holds.
pub const MAX_COUNT: usize = 65_535;

/// Checks the limits the converter works within for one function.
fn check_limits(function: &Function) -> Result<(), String> {
    if function.free.len() > MAX_COUNT {
        return Err(format!("it has more than {MAX_COUNT} free variables"));
    }
    let mut longest = 0;
    for term in &function.terms {
        match term {
            Term::Let {
                value: Value::SetFree { index, .. },
                ..
            } if *index as usize > MAX_COUNT => {
                return Err(format!("a set-free index above {MAX_COUNT}"));
            }
            Term::Let {
                value: Value::Call { args, .. } | Value::CallDirect { args, .. },
                ..
            }
            | Term::TailCall { args, .. }
            | Term::TailCallDirect { args, .. }
            | Term::Jump { args, .. } => longest = longest.max(args.len()),
            _ => {}
        }
    }
    if longest > MAX_COUNT {
        return Err(format!("a call with more than {MAX_COUNT} arguments"));
    }
    let bounds = crate::bounds(function);
    if bounds.top_slot > MAX_SLOT {
        return Err(format!(
            "it may need {} slots, and a frame has {}: it has too many variables or operands",
            bounds.top_slot + 1,
            MAX_SLOT + 1
        ));
    }
    if bounds.code_words > MAX_CODE_WORDS {
        return Err(format!(
            "its code may take {} words, and a branch spans at most {MAX_CODE_WORDS}: it has too many terms",
            bounds.code_words
        ));
    }
    Ok(())
}

/// Checks that a function does not declare more variables or join points
/// than it binds, before anything is allocated for the declared counts: they
/// are numbers in the input, not sizes of anything in it.
fn check_counts(function: &Function) -> Result<(), String> {
    let mut vars = function.params.len() as u64 + function.free.len() as u64;
    let mut joins = 0u64;
    for term in &function.terms {
        match term {
            Term::Let { .. } => vars += 1,
            Term::Join { params, .. } => {
                joins += 1;
                vars += params.len() as u64;
            }
            _ => {}
        }
    }
    if u64::from(function.vars) > vars {
        return Err(format!(
            "it declares {} variables but binds {vars}",
            function.vars
        ));
    }
    if u64::from(function.joins) > joins {
        return Err(format!(
            "it declares {} join points but binds {joins}",
            function.joins
        ));
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
            Constant::Labelled(_, inner) => pending.push(inner),
            _ => {}
        }
    }
    Ok(())
}

/// Checks a constant's labels, iteratively, in the order a pre-order walk
/// meets them: they are numbered from 0 as defined, a reference names a
/// label already defined (possibly one it sits inside), and a label's
/// datum is not nothing but a reference to a label of the same chain, as
/// there would be no object to refer to.
fn check_labels(constant: &Constant) -> Result<(), String> {
    let mut defined = 0u32;
    let mut pending = vec![constant];
    while let Some(constant) = pending.pop() {
        match constant {
            Constant::Labelled(..) => {
                let first = defined;
                let mut inner = constant;
                while let Constant::Labelled(n, next) = inner {
                    if *n != defined {
                        return Err(format!(
                            "label {n} is out of order: expected label {defined}"
                        ));
                    }
                    defined += 1;
                    inner = next;
                }
                if let Constant::LabelRef(m) = inner
                    && (first..defined).contains(m)
                {
                    return Err(format!("label {m} stands for nothing but itself"));
                }
                pending.push(inner);
            }
            Constant::LabelRef(n) if *n >= defined => {
                return Err(format!("label {n} is referred to before it is defined"));
            }
            Constant::Pair(car, cdr) => {
                pending.push(cdr);
                pending.push(car);
            }
            Constant::Vector(items) => pending.extend(items.iter().rev()),
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
    /// The walk has left a term's subtree.
    Leave(TermId),
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
    /// Each term's place in the walk's preorder, and the place after its
    /// subtree: `a` is an ancestor of `b` when `pre[a] < pre[b] < end[a]`.
    pre: Vec<u32>,
    end: Vec<u32>,
    clock: u32,
    /// Whether a `let` in this function binds each variable to a closure.
    made: Vec<bool>,
    /// The uses of closure variables other than their self-ties, as the
    /// variable and the term using it.
    uses: Vec<(Var, TermId)>,
    /// The self-ties `set-free c i c`, by closure: index and term.
    ties: BTreeMap<Var, Vec<(u32, TermId)>>,
    /// How many `set-free`s write each closure slot.
    writes: BTreeMap<(Var, u32), u32>,
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
            pre: vec![0; function.terms.len()],
            end: vec![0; function.terms.len()],
            clock: 0,
            made: vec![false; function.vars as usize],
            uses: Vec::new(),
            ties: BTreeMap::new(),
            writes: BTreeMap::new(),
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
        // Each copy was called from an earlier one, or the function's own
        // code, so following calls out always ends.
        for (i, copy) in function.inlined.iter().enumerate() {
            self.position(&copy.call)?;
            if copy.call.is_some_and(|call| call.inlined as usize > i) {
                return Err(format!("inlined copy i{i} is called from a later one"));
            }
        }
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
                Task::Leave(id) => self.end[id.0 as usize] = self.clock,
            }
        }
        if let Some(n) = self.seen.iter().position(|seen| !seen) {
            return Err(format!("term t{n} is not part of the body"));
        }
        self.check_ties()
    }

    /// The self-tie rules, once the whole tree has been walked.
    fn check_ties(&self) -> Result<(), String> {
        for (var, ties) in &self.ties {
            for &(index, _) in ties {
                if self.writes[&(*var, index)] != 1 {
                    return Err(format!(
                        "free variable {index} of v{}, which is tied to itself, is written again",
                        var.0
                    ));
                }
            }
        }
        let ancestor = |a: TermId, b: TermId| {
            let (a, b) = (a.0 as usize, b.0 as usize);
            self.pre[a] < self.pre[b] && self.pre[b] < self.end[a]
        };
        // Ancestors of a term form a chain, so every tie is an ancestor of
        // a use when the last tie in preorder is, and the others are its
        // ancestors, which is worked out once per closure.
        let mut last: BTreeMap<Var, (TermId, bool)> = BTreeMap::new();
        for (&var, ties) in &self.ties {
            let &(_, latest) = ties
                .iter()
                .max_by_key(|&&(_, t)| self.pre[t.0 as usize])
                .expect("a tie list is never empty");
            let chain = ties
                .iter()
                .all(|&(_, t)| t == latest || ancestor(t, latest));
            last.insert(var, (latest, chain));
        }
        for &(var, user) in &self.uses {
            let Some(&(latest, chain)) = last.get(&var) else {
                continue;
            };
            if !chain || !ancestor(latest, user) {
                return Err(format!(
                    "v{} is used at t{} where it is not yet tied to itself by every \
                     set-free at t{}; a closure's ties must come before its uses",
                    var.0, user.0, latest.0
                ));
            }
        }
        Ok(())
    }

    /// Records what `value`, bound at term `id`, does with closures.
    fn closures(&mut self, id: TermId, var: Var, value: &Value) -> Result<(), String> {
        let mut operands: Vec<&Operand> = Vec::new();
        match value {
            Value::Closure { free, .. } => {
                operands.extend(free);
                self.made[var.0 as usize] = true;
            }
            Value::SetFree {
                closure,
                index,
                value,
            } => {
                let c = match closure {
                    Operand::Var(c) if self.made[c.0 as usize] => *c,
                    Operand::Var(c) => {
                        return Err(format!(
                            "set-free of v{}, which is not a closure made in this function",
                            c.0
                        ));
                    }
                    Operand::Const(_) => {
                        return Err(
                            "set-free of a constant, not a closure made in this function".into(),
                        );
                    }
                };
                *self.writes.entry((c, *index)).or_insert(0) += 1;
                if *value == Operand::Var(c) {
                    self.ties.entry(c).or_default().push((*index, id));
                } else {
                    operands.extend([closure, value]);
                }
            }
            Value::Operand(x) | Value::Box(x) | Value::Unbox(x) | Value::SetGlobal(_, x) => {
                operands.push(x);
            }
            Value::Primitive { args, .. } | Value::CallDirect { args, .. } => {
                operands.extend(args);
            }
            Value::Call { callee, args } => {
                operands.push(callee);
                operands.extend(args);
            }
            Value::SetBox(a, b) => operands.extend([a, b]),
            Value::Global(_) => {}
        }
        self.record(id, operands);
        Ok(())
    }

    /// Notes each of `operands` that is a closure variable as used by `id`.
    fn record<'b>(&mut self, id: TermId, operands: impl IntoIterator<Item = &'b Operand>) {
        for operand in operands {
            if let Operand::Var(v) = operand
                && self.made[v.0 as usize]
            {
                self.uses.push((*v, id));
            }
        }
    }

    fn enter(&mut self, id: TermId, tasks: &mut Vec<Task<'a>>) -> Result<(), String> {
        let n = id.0 as usize;
        match self.seen.get(n) {
            None => return Err(format!("term t{n} does not exist")),
            Some(true) => return Err(format!("term t{n} is part of the body twice")),
            Some(false) => self.seen[n] = true,
        }
        self.pre[n] = self.clock;
        self.clock += 1;
        tasks.push(Task::Leave(id));
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
                let bound = *var;
                let var = std::slice::from_ref(var);
                self.define(var)?;
                self.closures(id, bound, value)?;
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
                self.record(id, args);
            }
            Term::If {
                test,
                then,
                otherwise,
            } => {
                self.operand(test)?;
                self.record(id, [test]);
                tasks.push(Task::Enter(*otherwise));
                tasks.push(Task::Enter(*then));
            }
            Term::Return(value) => {
                self.operand(value)?;
                self.record(id, [value]);
            }
            Term::TailCall {
                callee,
                args,
                position,
            } => {
                self.operand(callee)?;
                self.operands(args)?;
                self.position(position)?;
                self.record(id, [callee]);
                self.record(id, args);
            }
            Term::TailCallDirect {
                function,
                args,
                position,
            } => {
                self.direct(*function, args)?;
                self.operands(args)?;
                self.position(position)?;
                self.record(id, args);
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
            Some(position) if position.inlined as usize > self.function.inlined.len() => Err(
                format!("inlined copy i{} does not exist", position.inlined - 1),
            ),
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
