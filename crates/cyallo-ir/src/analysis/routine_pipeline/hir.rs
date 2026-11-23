use super::{Scalar, ScalarType, Variable};
use cyallo_ast::indexed::{
    ExprNode, ExprSyntax, FunctionNode, Id, IndexedAst, List, LiteralNode, NameId, StmtNode,
    StmtSyntax, TypeNode,
};
use cyallo_ast::{
    FunctionBodyKind, NumberKind, Operator, ParameterArity, ParameterPassing, RoutineKind, Span,
};

pub(super) struct Expression {
    pub(super) span: Span,
    pub(super) kind: ExpressionKind,
}

pub(super) enum ExpressionKind {
    Constant(Scalar),
    Read(Variable),
    Group(Box<Expression>),
    Binary(Operator, Box<Expression>, Box<Expression>),
    Unary(Operator, Box<Expression>),
    /// `left && right` or `left || right`: the right side runs only when the left
    /// does not decide. `slot` is a hidden local that carries the result across blocks.
    ShortCircuit {
        and: bool,
        left: Box<Expression>,
        right: Box<Expression>,
        slot: Variable,
    },
    /// A call whose callee path is bound later, by the module graph.
    Call(Box<str>, Vec<Expression>),
}

pub(super) struct Statement {
    pub(super) span: Span,
    pub(super) kind: StatementKind,
}

pub(super) enum StatementKind {
    If(Expression, Vec<Statement>, Vec<Statement>),
    Return(Expression),
    /// `let [mut] name [: type] = value`. The initializer runs before the name exists.
    Let {
        slot: Variable,
        value: Expression,
        annotation: Option<ScalarType>,
    },
    Assign {
        slot: Variable,
        value: Expression,
    },
    /// `name op= value`: read, operate, then store, with the statement's span.
    Compound {
        slot: Variable,
        operator: Operator,
        value: Expression,
    },
    While(Expression, Vec<Statement>),
    ForRange(Box<ForRange>),
    Break,
    Continue,
    /// `return` in a procedure.
    ReturnVoid,
    /// A call to a procedure, as a statement. The expression is a `Call`.
    CallStatement(Expression),
}

/// `for variable in start..finish` over `i64`, without `rev`. `end` and `bound`
/// are hidden locals that hold the finish value and the last value to visit.
pub(super) struct ForRange {
    pub(super) variable: Variable,
    pub(super) end: Variable,
    pub(super) bound: Variable,
    pub(super) start: Expression,
    pub(super) finish: Expression,
    pub(super) inclusive: bool,
    pub(super) iterable_span: Span,
    pub(super) body: Vec<Statement>,
}

pub(super) struct Local {
    pub(super) name: Box<str>,
    pub(super) span: Span,
    pub(super) constant: bool,
}

pub(super) struct Routine {
    pub(super) statements: Vec<Statement>,
    /// Declared and hidden locals, after the parameters, in slot order.
    pub(super) locals: Vec<Local>,
}

#[derive(Clone, Copy)]
struct Visible {
    name: NameId,
    slot: Variable,
    mutable: bool,
}

struct Context<'a> {
    ast: &'a IndexedAst,
    parameters: usize,
    /// Every parameter and declared name in the routine. A call to one of these
    /// names is a local callable, which this subset leaves to the existing route.
    every_name: Vec<NameId>,
    locals: Vec<Local>,
    scopes: Vec<Vec<Visible>>,
    /// Loops enclosing the statement being lowered.
    loops: usize,
    /// The routine is a procedure: it returns no value.
    procedure: bool,
}

/// Lower eligible routine bodies directly from the file's indexed tables. `None`
/// leaves the routine to the existing route, which also reports its diagnostics
/// for anything this subset does not accept (duplicate names, assignment to an
/// immutable name, a name used out of scope, and so on).
pub(super) fn from_indexed(ast: &IndexedAst, function: &FunctionNode) -> Option<Routine> {
    if function.body_kind != FunctionBodyKind::Source
        || function.generator
        || !ast.get_list(function.annotations)?.is_empty()
        || !ast.get_list(function.type_params)?.is_empty()
        || !ast.get_list(function.type_packs)?.is_empty()
    {
        return None;
    }
    // A function returns `i64` or `bool`; a procedure returns nothing.
    let procedure = match (function.kind, ast.get(function.result)?) {
        (RoutineKind::Function, TypeNode::Integer | TypeNode::Boolean) => false,
        (RoutineKind::Procedure, TypeNode::Void) => true,
        _ => return None,
    };
    let parameters = ast.get_list(function.params)?;
    if parameters.iter().any(|parameter| {
        parameter.arity != ParameterArity::One
            || parameter.passing != ParameterPassing::Value
            || parameter.by_ref
            || parameter.receiver
            || !matches!(
                ast.get(parameter.ty),
                Some(TypeNode::Integer | TypeNode::Boolean)
            )
    }) {
        return None;
    }
    let names: Vec<_> = parameters.iter().map(|parameter| parameter.name).collect();
    let mut every_name = names.clone();
    let mut counters = Vec::new();
    declared_names(ast, function.body, &mut every_name, &mut counters)?;
    // Locals are one flat namespace in the existing route; a repeated name is its
    // error. Only loop counters may share a name, across loops that do not nest.
    for (index, name) in every_name.iter().enumerate() {
        if every_name[..index].contains(name) || counters.contains(name) {
            return None;
        }
    }
    every_name.extend(counters.iter().copied());
    let mut context = Context {
        ast,
        parameters: names.len(),
        every_name,
        locals: Vec::new(),
        loops: 0,
        procedure,
        scopes: vec![
            names
                .iter()
                .enumerate()
                .map(|(index, &name)| Visible {
                    name,
                    slot: Variable(index),
                    mutable: false,
                })
                .collect(),
        ],
    };
    let statements = context.block(function.body)?;
    Some(Routine {
        statements,
        locals: context.locals,
    })
}

/// Collect every name a statement list declares. Statements this subset does not
/// lower end the walk, because the whole routine then stays on the existing route.
fn declared_names(
    ast: &IndexedAst,
    list: List<Id<StmtNode>>,
    out: &mut Vec<NameId>,
    counters: &mut Vec<NameId>,
) -> Option<()> {
    cyallo_ast::with_stack(|| {
        for &id in ast.get_list(list)? {
            match ast.get(id)?.kind {
                StmtSyntax::Binding(name, _, _, _, _) => out.push(name),
                StmtSyntax::ForIn(name, _, body) => {
                    if !counters.contains(&name) {
                        counters.push(name);
                    }
                    declared_names(ast, body, out, counters)?;
                }
                StmtSyntax::If(_, yes, no) => {
                    declared_names(ast, yes, out, counters)?;
                    declared_names(ast, no, out, counters)?;
                }
                StmtSyntax::While(_, body) => declared_names(ast, body, out, counters)?,
                StmtSyntax::Return(_)
                | StmtSyntax::ReturnVoid
                | StmtSyntax::Evaluate(_)
                | StmtSyntax::Assign(..)
                | StmtSyntax::Compound(..)
                | StmtSyntax::Break
                | StmtSyntax::Continue => {}
                _ => return None,
            }
        }
        Some(())
    })
}

impl Context<'_> {
    fn lookup(&self, name: NameId) -> Option<Visible> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.iter().rev().find(|entry| entry.name == name))
            .copied()
    }

    fn declare(&mut self, name: Box<str>, span: Span, constant: bool) -> Variable {
        let slot = Variable(self.parameters + self.locals.len());
        self.locals.push(Local {
            name,
            span,
            constant,
        });
        slot
    }

    fn name(&self, name: NameId) -> Option<Box<str>> {
        self.ast.name(name).map(Box::from)
    }

    fn block(&mut self, list: List<Id<StmtNode>>) -> Option<Vec<Statement>> {
        self.scopes.push(Vec::new());
        let statements = self.statements(list);
        self.scopes.pop();
        statements
    }

    fn statements(&mut self, list: List<Id<StmtNode>>) -> Option<Vec<Statement>> {
        cyallo_ast::with_stack(|| {
            let ast = self.ast;
            ast.get_list(list)?
                .iter()
                .map(|&id| {
                    let statement = ast.get(id)?;
                    let kind = match statement.kind {
                        StmtSyntax::If(condition, yes, no) => StatementKind::If(
                            self.expression(condition)?,
                            self.block(yes)?,
                            self.block(no)?,
                        ),
                        // The existing route reports a value returned from a procedure.
                        StmtSyntax::Return(value) if !self.procedure => {
                            StatementKind::Return(self.expression(value)?)
                        }
                        StmtSyntax::ReturnVoid if self.procedure => StatementKind::ReturnVoid,
                        StmtSyntax::Evaluate(value) => {
                            let call = self.value(value, false)?;
                            // Only a call to a procedure; the linker checks which.
                            if !matches!(call.kind, ExpressionKind::Call(..)) {
                                return None;
                            }
                            StatementKind::CallStatement(call)
                        }
                        StmtSyntax::Binding(name, annotation, constant, mutable, value) => {
                            if constant {
                                return None;
                            }
                            let annotation = match annotation {
                                None => None,
                                Some(ty) => Some(match ast.get(ty)? {
                                    TypeNode::Integer => ScalarType::Integer,
                                    TypeNode::Boolean => ScalarType::Boolean,
                                    _ => return None,
                                }),
                            };
                            // The initializer cannot see the name it declares.
                            let value = self.expression(value)?;
                            let slot = self.declare(self.name(name)?, statement.span, !mutable);
                            self.scopes.last_mut()?.push(Visible {
                                name,
                                slot,
                                mutable,
                            });
                            StatementKind::Let {
                                slot,
                                value,
                                annotation,
                            }
                        }
                        StmtSyntax::Assign(target, value) => StatementKind::Assign {
                            slot: self.assignable(target)?,
                            value: self.expression(value)?,
                        },
                        StmtSyntax::Compound(target, operator, value)
                            if operator.is_arithmetic() && operator != Operator::RealDivide =>
                        {
                            StatementKind::Compound {
                                slot: self.assignable(target)?,
                                operator,
                                value: self.expression(value)?,
                            }
                        }
                        StmtSyntax::While(condition, body) => {
                            let condition = self.expression(condition)?;
                            self.loops += 1;
                            let body = self.block(body);
                            self.loops -= 1;
                            StatementKind::While(condition, body?)
                        }
                        // Outside a loop the existing route reports E115.
                        StmtSyntax::Break if self.loops > 0 => StatementKind::Break,
                        StmtSyntax::Continue if self.loops > 0 => StatementKind::Continue,
                        StmtSyntax::ForIn(name, iterable, body) => {
                            self.range(name, iterable, body, statement.span)?
                        }
                        _ => return None,
                    };
                    Some(Statement {
                        span: statement.span,
                        kind,
                    })
                })
                .collect()
        })
    }

    /// The slot of a mutable local named by `target`. Parameters, immutable names,
    /// loop counters and anything else stay with the existing route, which owns
    /// those diagnostics.
    fn assignable(&self, target: Id<ExprNode>) -> Option<Variable> {
        let ExprSyntax::Name(name) = self.ast.get(target)?.kind else {
            return None;
        };
        let visible = self.lookup(name)?;
        visible.mutable.then_some(visible.slot)
    }

    fn range(
        &mut self,
        name: NameId,
        iterable: Id<ExprNode>,
        body: List<Id<StmtNode>>,
        span: Span,
    ) -> Option<StatementKind> {
        // A counter reusing the name of an enclosing counter would share its slot.
        if self.lookup(name).is_some() {
            return None;
        }
        let (start, finish, inclusive, iterable_span) = self.bounds(iterable)?;
        let variable = self.declare(self.name(name)?, span, false);
        let end = self.declare("$for_end".into(), span, false);
        let bound = self.declare("$for_bound".into(), span, false);
        // The counter is visible in the body only, not in the bounds.
        self.scopes.push(vec![Visible {
            name,
            slot: variable,
            mutable: false,
        }]);
        self.loops += 1;
        let body = self.block(body);
        self.loops -= 1;
        self.scopes.pop();
        Some(StatementKind::ForRange(Box::new(ForRange {
            variable,
            end,
            bound,
            start,
            finish,
            inclusive,
            iterable_span,
            body: body?,
        })))
    }

    fn bounds(&mut self, iterable: Id<ExprNode>) -> Option<(Expression, Expression, bool, Span)> {
        let node = self.ast.get(iterable)?;
        match node.kind {
            ExprSyntax::Parenthesized(inner) => self.bounds(inner),
            ExprSyntax::Binary(operator @ (Operator::Range | Operator::RangeInclusive), a, b) => {
                Some((
                    self.expression(a)?,
                    self.expression(b)?,
                    operator == Operator::RangeInclusive,
                    node.span,
                ))
            }
            _ => None,
        }
    }

    fn expression(&mut self, id: Id<ExprNode>) -> Option<Expression> {
        self.value(id, true)
    }

    /// `split` is true where a short-circuit operator may start new blocks: no
    /// earlier sibling value is waiting, because a value cannot cross a block.
    fn value(&mut self, id: Id<ExprNode>, split: bool) -> Option<Expression> {
        cyallo_ast::with_stack(|| {
            let ast = self.ast;
            let expression = ast.get(id)?;
            let kind = match expression.kind {
                ExprSyntax::Literal(value) => ExpressionKind::Constant(match ast.get(value)? {
                    LiteralNode::Number(number) if number.kind == NumberKind::I64 => {
                        Scalar::Integer(number.int() as i64)
                    }
                    LiteralNode::Integer(value) => Scalar::Integer(*value),
                    LiteralNode::Boolean(value) => Scalar::Boolean(*value),
                    _ => return None,
                }),
                ExprSyntax::Name(name) => ExpressionKind::Read(self.lookup(name)?.slot),
                ExprSyntax::Parenthesized(inner) => {
                    ExpressionKind::Group(Box::new(self.value(inner, split)?))
                }
                ExprSyntax::Binary(operator @ (Operator::And | Operator::Or), left, right)
                    if split =>
                {
                    let left = Box::new(self.value(left, true)?);
                    let right = Box::new(self.value(right, true)?);
                    let slot = self.declare("$short_circuit".into(), expression.span, false);
                    ExpressionKind::ShortCircuit {
                        and: operator == Operator::And,
                        left,
                        right,
                        slot,
                    }
                }
                ExprSyntax::Binary(operator, left, right) if scalar_operator(operator) => {
                    ExpressionKind::Binary(
                        operator,
                        Box::new(self.value(left, false)?),
                        Box::new(self.value(right, false)?),
                    )
                }
                ExprSyntax::Unary(operator @ (Operator::Not | Operator::Subtract), operand) => {
                    ExpressionKind::Unary(operator, Box::new(self.value(operand, split)?))
                }
                ExprSyntax::Call(name, arguments) => {
                    // A local of the same name shadows the declaration.
                    if self.every_name.contains(&name) {
                        return None;
                    }
                    ExpressionKind::Call(
                        ast.name(name)?.into(),
                        ast.get_list(arguments)?
                            .iter()
                            .map(|&argument| self.value(argument, false))
                            .collect::<Option<_>>()?,
                    )
                }
                _ => return None,
            };
            Some(Expression {
                span: expression.span,
                kind,
            })
        })
    }
}

fn scalar_operator(operator: Operator) -> bool {
    (operator.is_arithmetic() && operator != Operator::RealDivide) || operator.is_comparison()
}
