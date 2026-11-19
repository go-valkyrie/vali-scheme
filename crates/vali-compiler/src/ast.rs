//! The compiler's tree: core forms with every variable resolved.

use vali_syntax::{Datum, Span};

/// A lexical variable, numbered across the whole program.
pub(crate) type VarId = usize;

/// A `lambda`, numbered across the whole program. Number 0 is the
/// program's body.
pub(crate) type LambdaId = usize;

/// What the compiler knows about a lexical variable.
#[derive(Debug)]
pub(crate) struct VarInfo {
    pub(crate) name: String,
    /// The `lambda` whose frame holds it.
    pub(crate) binder: LambdaId,
    /// Whether `set!` assigns it.
    pub(crate) assigned: bool,
    /// Whether a `lambda` other than its binder refers to it.
    pub(crate) captured: bool,
}

impl VarInfo {
    /// Whether it lives in a box: whenever it is assigned (ADR 0005 #4).
    /// A captured variable shares its box with closures; any other shares
    /// it with the copies of its frame that re-entering a continuation
    /// makes, so a variable stays one location.
    pub(crate) fn boxed(&self) -> bool {
        self.assigned
    }
}

/// An expression.
#[derive(Debug)]
pub(crate) enum Expr {
    /// The unspecified value.
    Unspecified,
    /// A constant.
    Const(Datum),
    /// A lexical variable.
    Local(VarId),
    /// A module variable the program defines.
    Global(String),
    /// A procedure implemented in Rust that `(vali primitive)` exports, by
    /// name. Only these are inlined, so a program's own definitions never
    /// are.
    Free(String),
    /// An isolate global: a REPL's variable, bound late and never inlined.
    ReplGlobal(String),
    /// A variable a library defines: the library's identity, and the
    /// variable's name there.
    Imported(String, String),
    SetLocal(VarId, Box<Expr>),
    SetGlobal(String, Box<Expr>),
    /// Sets the isolate's global of that name: a REPL's variable.
    SetFree(String, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// At least one expression.
    Begin(Vec<Expr>),
    Lambda(LambdaId),
    /// Each init is evaluated before any variable is bound.
    Let(Vec<(VarId, Expr)>, Box<Expr>),
    /// `letrec*`: each init sees every variable, and runs in order.
    Letrec(Vec<(VarId, Expr)>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>, Span),
}

/// A `lambda`.
#[derive(Debug)]
pub(crate) struct Lambda {
    pub(crate) name: Option<String>,
    pub(crate) required: Vec<VarId>,
    pub(crate) rest: Option<VarId>,
    pub(crate) body: Expr,
    /// The variables of enclosing `lambda`s it refers to, directly or from
    /// a `lambda` inside it, in a fixed order: its closure's free
    /// variables.
    pub(crate) free: Vec<VarId>,
}

impl Expr {
    /// Moves the expressions directly inside this one into `out`, leaving
    /// it with none.
    fn take_children(&mut self, out: &mut Vec<Expr>) {
        let mut take = |e: &mut Expr| out.push(std::mem::replace(e, Expr::Unspecified));
        match self {
            Expr::SetLocal(_, e) | Expr::SetGlobal(_, e) | Expr::SetFree(_, e) => take(e),
            Expr::If(test, then, otherwise) => {
                take(test);
                take(then);
                take(otherwise);
            }
            Expr::Begin(exprs) => exprs.iter_mut().for_each(take),
            Expr::Let(inits, body) | Expr::Letrec(inits, body) => {
                inits.iter_mut().for_each(|(_, init)| take(init));
                take(body);
            }
            Expr::Call(callee, args, _) => {
                take(callee);
                args.iter_mut().for_each(take);
            }
            Expr::Unspecified
            | Expr::Const(_)
            | Expr::Local(_)
            | Expr::Global(_)
            | Expr::Free(_)
            | Expr::ReplGlobal(_)
            | Expr::Imported(..)
            | Expr::Lambda(_) => {}
        }
    }
}

/// Frees the expressions inside one at a time, from a list of its own, so
/// that dropping an expression nested thousands deep cannot overflow the
/// stack.
impl Drop for Expr {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        self.take_children(&mut pending);
        while let Some(mut expr) = pending.pop() {
            // `expr` is left without children, so dropping it at the end of
            // this iteration recurses no further.
            expr.take_children(&mut pending);
        }
    }
}
