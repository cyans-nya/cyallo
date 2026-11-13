use crate::ir::RoutineShape;
use cyallo_ast::{Expr, ExprKind as E, Function, Stmt, StmtKind as S};
/// Syntax-only shape of a routine, used before its body is lowered.
#[derive(Debug, Clone, Default)]
pub struct Estimate {
    pub nodes: usize,
    pub calls: Vec<Box<str>>,
    pub shape: RoutineShape,
}
impl Estimate {
    pub fn eager_candidate(&self) -> bool {
        self.nodes <= 256 && (self.shape.loops || self.shape.calls)
    }
}
pub fn estimate(function: &Function) -> Estimate {
    let mut estimate = Estimate::default();
    estimate.block(&function.body);
    // A syntax-only edge must never mistake a local callable for a named routine.
    // Local names have routine scope, so a conservative whole-body scan is enough.
    fn bindings(body: &[Stmt], names: &mut std::collections::HashSet<String>) {
        for s in body {
            match &s.kind {
                S::Binding(n, ..) | S::Declare(n, _) | S::Constant(n, _) => {
                    names.insert(n.to_string());
                }
                S::For(n, _, _, _, b) => {
                    names.insert(n.to_string());
                    bindings(b, names);
                }
                S::If(_, a, b) => {
                    bindings(a, names);
                    bindings(b, names);
                }
                S::While(_, b) | S::Repeat(b, _) => bindings(b, names),
                S::Case(_, arms, b) => {
                    for (_, b) in arms {
                        bindings(b, names);
                    }
                    bindings(b, names);
                }
                _ => {}
            }
        }
    }
    let mut names = function.params.iter().map(|p| p.name.to_string()).collect();
    bindings(&function.body, &mut names);
    estimate.calls.retain(|n| !names.contains(&n.to_string()));
    estimate
}
impl Estimate {
    fn expr(&mut self, e: &Expr) {
        self.nodes += 1;
        match &e.kind {
            E::Call(name, args)
            | E::TypedCollection(name, _, args)
            | E::GenericCall(name, _, args) => {
                self.calls.push(name.clone());
                self.shape.calls = true;
                for a in args {
                    self.expr(a);
                }
            }
            E::IntrinsicCall(_, _, args) => {
                self.shape.calls = true;
                for a in args {
                    self.expr(a);
                }
            }
            E::ArrayLiteral(args) | E::Tuple(args) => {
                for a in args {
                    self.expr(a);
                }
            }
            E::Index(a, indices) => {
                self.expr(a);
                for i in indices {
                    self.expr(i);
                }
            }
            E::Yield(a, _)
            | E::Parenthesized(a)
            | E::Unary(_, a)
            | E::Field(a, _)
            | E::TupleField(a, _)
            | E::Cast(a, _)
            | E::ExtensionField(a, _, _) => self.expr(a),
            E::Record(_, fs) | E::GenericRecord(_, _, fs) => {
                for (_, e) in fs {
                    self.expr(e);
                }
            }
            E::Invoke(a, args) => {
                self.shape.calls = true;
                self.expr(a);
                for e in args {
                    self.expr(e);
                }
            }
            E::ClosureValue(_, captures, _) => {
                for e in captures {
                    self.expr(e);
                }
            }
            E::ManagedNew(_, allocator, value) => {
                if let Some(allocator) = allocator {
                    self.expr(allocator);
                }
                self.expr(value);
            }
            E::Closure(..) => {}
            E::Binary(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            E::Literal(_) | E::Frozen(..) | E::Name(_) | E::Invalid(_) => {}
        }
    }
    fn block(&mut self, body: &[Stmt]) {
        for s in body {
            self.nodes += 1;
            match &s.kind {
                S::While(c, b) | S::Repeat(b, c) => {
                    self.shape.loops = true;
                    self.expr(c);
                    self.block(b);
                }
                S::For(_, a, b, step, body) => {
                    self.shape.loops = true;
                    self.expr(a);
                    self.expr(b);
                    if let Some(s) = step {
                        self.expr(s)
                    };
                    self.block(body);
                }
                S::If(c, a, b) => {
                    self.expr(c);
                    self.block(a);
                    self.block(b);
                }
                S::Case(c, arms, other) => {
                    self.expr(c);
                    for (_, b) in arms {
                        self.block(b)
                    }
                    self.block(other);
                }
                S::Call(name, args) => {
                    self.calls.push(name.clone());
                    self.shape.calls = true;
                    for a in args {
                        self.expr(a);
                    }
                }
                S::Assign(a, b) | S::Compound(a, _, b) => {
                    self.expr(a);
                    self.expr(b);
                }
                S::Binding(_, _, _, _, e)
                | S::Constant(_, e)
                | S::Evaluate(e)
                | S::Return(e)
                | S::Input(e) => self.expr(e),
                S::Output(es) => {
                    for e in es {
                        self.expr(e);
                    }
                }
                S::Declare(..) | S::ReturnVoid | S::Break | S::Continue => {}
            }
        }
    }
}
