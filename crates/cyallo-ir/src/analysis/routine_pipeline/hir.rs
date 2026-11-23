use super::{LocalName, ReferenceSite, SiteId, Variable};
use cyallo_ast::indexed::{
    ExprNode, ExprSyntax, FunctionNode, Id, IndexedAst, List, LiteralNode, NameId, StmtNode,
    StmtSyntax, TypeNode,
};
use std::sync::OnceLock;

use cyallo_ast::{
    FunctionBodyKind, Literal, Operator, ParameterArity, ParameterPassing, RoutineKind, Span, Type,
};

#[derive(Clone, Debug)]
pub(super) struct Expression {
    pub(super) span: Span,
    pub(super) kind: ExpressionKind,
    /// The inferred type of a binary node, remembered once known so no node is typed
    /// twice (leaves are cheaper to type than to look up). It depends
    /// only on the node and on the routine's fixed local and callee types, and a
    /// retyped copy is a new node with nothing remembered.
    pub(super) inferred: OnceLock<Box<Type>>,
}

#[derive(Clone, Debug)]
pub(super) enum ExpressionKind {
    /// A literal as written: a plain integer or real literal takes the type its
    /// context asks for, a suffixed one has its own.
    Literal(Literal),
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
    /// A call; the site holds the callee path that the module graph binds later.
    Call(SiteId, Vec<Expression>),
    /// `Name { field: value, ... }`, the fields in the order written. The site holds the
    /// record's path, which the module graph binds to its declaration.
    Record {
        site: SiteId,
        fields: Vec<(NameId, Expression)>,
    },
    /// `p.a.b`: a field of a local or of a reference parameter, found by name.
    Project {
        root: Variable,
        path: Vec<NameId>,
    },
    /// A string literal: its text is interned in the body's unit.
    Text(NameId),
    /// `p.*`: the value a reference parameter refers to.
    Deref(Variable),
    /// `&x` or `&mut x`, as a call argument: a reference to a local or a by-value parameter.
    Borrow {
        variable: Variable,
        mutable: bool,
    },
    /// A reference parameter passed on, as a call argument.
    Reborrow(Variable),
}

impl Expression {
    pub(super) fn new(span: Span, kind: ExpressionKind) -> Self {
        Self {
            span,
            kind,
            inferred: OnceLock::new(),
        }
    }

    /// Number of expression nodes, for the admission estimate.
    pub(super) fn size(&self) -> usize {
        1 + match &self.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Read(_)
            | ExpressionKind::Deref(_)
            | ExpressionKind::Borrow { .. }
            | ExpressionKind::Reborrow(_) => 0,
            ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => inner.size(),
            ExpressionKind::Record { fields, .. } => {
                fields.iter().map(|(_, value)| value.size()).sum()
            }
            ExpressionKind::Project { path, .. } => path.len(),
            ExpressionKind::Text(_) => 0,
            ExpressionKind::Binary(_, left, right)
            | ExpressionKind::ShortCircuit { left, right, .. } => left.size() + right.size(),
            ExpressionKind::Call(_, arguments) => arguments.iter().map(Expression::size).sum(),
        }
    }
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
        annotation: Option<Type>,
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
    /// `p.a.b = value`.
    AssignProject {
        root: Variable,
        path: Vec<NameId>,
        value: Expression,
    },
    /// `p.a.b op= value`.
    CompoundProject {
        root: Variable,
        path: Vec<NameId>,
        operator: Operator,
        value: Expression,
    },
    /// `p.* = value`.
    AssignDeref {
        variable: Variable,
        value: Expression,
    },
    /// `p.* op= value`.
    CompoundDeref {
        variable: Variable,
        operator: Operator,
        value: Expression,
    },
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
    pub(super) name: LocalName,
    pub(super) span: Span,
    pub(super) constant: bool,
}

impl Routine {
    /// Statement and expression nodes, for the admission estimate.
    pub(super) fn size(&self) -> usize {
        fn block(statements: &[Statement]) -> usize {
            statements
                .iter()
                .map(|statement| {
                    // Each statement is also a logical marker.
                    2 + match &statement.kind {
                        StatementKind::If(condition, yes, no) => {
                            condition.size() + block(yes) + block(no)
                        }
                        StatementKind::While(condition, body) => condition.size() + block(body),
                        StatementKind::ForRange(range) => {
                            range.start.size() + range.finish.size() + block(&range.body)
                        }
                        StatementKind::Return(value) => value.size(),
                        StatementKind::Let { value, .. }
                        | StatementKind::Assign { value, .. }
                        | StatementKind::Compound { value, .. }
                        | StatementKind::AssignDeref { value, .. }
                        | StatementKind::CompoundDeref { value, .. }
                        | StatementKind::AssignProject { value, .. }
                        | StatementKind::CompoundProject { value, .. } => value.size(),
                        StatementKind::CallStatement(call) => call.size(),
                        StatementKind::Break
                        | StatementKind::Continue
                        | StatementKind::ReturnVoid => 0,
                    }
                })
                .sum()
        }
        block(&self.statements)
    }

    /// Whether any statement is a loop.
    pub(super) fn has_loops(&self) -> bool {
        fn block(statements: &[Statement]) -> bool {
            statements.iter().any(|statement| match &statement.kind {
                StatementKind::While(..) | StatementKind::ForRange(_) => true,
                StatementKind::If(_, yes, no) => block(yes) || block(no),
                _ => false,
            })
        }
        block(&self.statements)
    }
}

pub(super) struct Routine {
    pub(super) sites: Vec<ReferenceSite>,
    pub(super) statements: Vec<Statement>,
    /// Declared and hidden locals, after the parameters, in slot order.
    pub(super) locals: Vec<Local>,
}

#[derive(Clone, Copy)]
struct Visible {
    name: NameId,
    slot: Variable,
    mutable: bool,
    /// A reference parameter, and whether it is `&mut`.
    reference: Option<bool>,
}

struct Context<'a> {
    ast: &'a IndexedAst,
    parameters: usize,
    /// Every parameter and declared name in the routine. A call to one of these
    /// names is a local callable, which this subset leaves to the existing route.
    every_name: Vec<NameId>,
    locals: Vec<Local>,
    sites: Vec<ReferenceSite>,
    scopes: Vec<Vec<Visible>>,
    /// Loops enclosing the statement being lowered.
    loops: usize,
    /// The routine returns no value.
    procedure: bool,
}

/// Lower eligible routine bodies directly from the file's indexed tables. `None`
/// leaves the routine to the existing route, which also reports its diagnostics
/// for anything this subset does not accept (duplicate names, assignment to an
/// immutable name, a name used out of scope, and so on).
pub(super) fn from_indexed(ast: &IndexedAst, function: &FunctionNode) -> Option<Routine> {
    REASON.with(|reason| *reason.borrow_mut() = None);
    if function.body_kind != FunctionBodyKind::Source {
        return decline(|| "declared without a body".into());
    }
    if function.generator {
        return decline(|| "generator".into());
    }
    if !ast.get_list(function.annotations)?.is_empty() {
        return decline(|| "annotated routine".into());
    }
    if !ast.get_list(function.type_params)?.is_empty()
        || !ast.get_list(function.type_packs)?.is_empty()
    {
        return decline(|| "generic routine".into());
    }
    // A routine returns a scalar, or a procedure returns nothing.
    let procedure = match (function.kind, ast.get(function.result)?) {
        (_, node) if signature_type(ast, node).is_some() => false,
        (RoutineKind::Procedure, TypeNode::Void) => true,
        (_, node) => return decline(|| format!("result type {}", variant(node))),
    };
    let parameters = ast.get_list(function.params)?;
    for parameter in parameters {
        if parameter.arity != ParameterArity::One {
            return decline(|| "variadic parameter".into());
        }
        if parameter.passing != ParameterPassing::Value || parameter.by_ref {
            return decline(|| "parameter passed by reference".into());
        }
        if parameter.receiver {
            return decline(|| "method receiver".into());
        }
        match ast.get(parameter.ty) {
            Some(node) if signature_type(ast, node).is_some() => {}
            Some(node) if reference_type(ast, node).is_some() => {}
            node => {
                return decline(|| format!("parameter type {}", node.map_or("?".into(), variant)));
            }
        }
    }
    let names: Vec<_> = parameters.iter().map(|parameter| parameter.name).collect();
    let mut every_name = names.clone();
    let mut counters = Vec::new();
    declared_names(ast, function.body, &mut every_name, &mut counters)?;
    // Locals are one flat namespace in the existing route; a repeated name is its
    // error. Only loop counters may share a name, across loops that do not nest.
    for (index, name) in every_name.iter().enumerate() {
        if every_name[..index].contains(name) || counters.contains(name) {
            return decline(|| "a name is declared twice".into());
        }
    }
    every_name.extend(counters.iter().copied());
    let mut context = Context {
        ast,
        parameters: names.len(),
        every_name,
        locals: Vec::new(),
        sites: Vec::new(),
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
                    reference: parameters
                        .get(index)
                        .and_then(|parameter| ast.get(parameter.ty))
                        .and_then(|node| reference_type(ast, node))
                        .map(|(mutable, _)| mutable),
                })
                .collect(),
        ],
    };
    let statements = context.block(function.body)?;
    Some(Routine {
        sites: context.sites,
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
                ref other => return decline(|| format!("statement {}", variant(other))),
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

    fn declare(&mut self, name: LocalName, span: Span, constant: bool) -> Variable {
        let slot = Variable(self.parameters + self.locals.len());
        self.locals.push(Local {
            name,
            span,
            constant,
        });
        slot
    }

    /// A call argument: a plain value, or the one place a borrow may appear.
    fn argument(&mut self, id: Id<ExprNode>) -> Option<Expression> {
        let ast = self.ast;
        let node = ast.get(id)?;
        let kind = match node.kind {
            ExprSyntax::Unary(operator @ (Operator::Borrow | Operator::BorrowMut), inner) => {
                let ExprSyntax::Name(name) = ast.get(inner)?.kind else {
                    return decline(|| "borrow of something other than a name".into());
                };
                match self.lookup(name) {
                    Some(visible) if visible.reference.is_none() => ExpressionKind::Borrow {
                        variable: visible.slot,
                        mutable: operator == Operator::BorrowMut,
                    },
                    _ => return decline(|| "borrow of a reference or an unknown name".into()),
                }
            }
            ExprSyntax::Name(name) => match self.lookup(name) {
                Some(visible) if visible.reference.is_some() => {
                    ExpressionKind::Reborrow(visible.slot)
                }
                _ => return self.value(id, false),
            },
            _ => return self.value(id, false),
        };
        Some(Expression::new(node.span, kind))
    }

    fn site(
        &mut self,
        path: NameId,
        span: Span,
        arguments: usize,
        discard: bool,
        record: bool,
    ) -> SiteId {
        self.sites.push(ReferenceSite {
            path,
            span,
            arguments,
            discard,
            record,
        });
        SiteId(self.sites.len() - 1)
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
                            // The linker checks that the callee has a contract this subset knows.
                            let ExpressionKind::Call(site, _) = &call.kind else {
                                return decline(|| "statement that is not a call".into());
                            };
                            self.sites[site.0].discard = true;
                            StatementKind::CallStatement(call)
                        }
                        StmtSyntax::Binding(name, annotation, constant, mutable, value) => {
                            if constant {
                                return decline(|| "constant binding".into());
                            }
                            let annotation = match annotation {
                                None => None,
                                Some(ty) => match scalar_type(ast.get(ty)?) {
                                    Some(ty) => Some(ty),
                                    None => {
                                        let shape = ast.get(ty).map_or("?".into(), variant);
                                        return decline(|| format!("local annotated {shape}"));
                                    }
                                },
                            };
                            // The initializer cannot see the name it declares.
                            let value = self.expression(value)?;
                            let slot =
                                self.declare(LocalName::Source(name), statement.span, !mutable);
                            self.scopes.last_mut()?.push(Visible {
                                name,
                                slot,
                                mutable,
                                reference: None,
                            });
                            StatementKind::Let {
                                slot,
                                value,
                                annotation,
                            }
                        }
                        StmtSyntax::Assign(target, value) => {
                            if let Some((root, path)) = self.projected(target) {
                                StatementKind::AssignProject {
                                    root,
                                    path,
                                    value: self.expression(value)?,
                                }
                            } else if let Some(variable) = self.dereferenced(target) {
                                StatementKind::AssignDeref {
                                    variable,
                                    value: self.expression(value)?,
                                }
                            } else {
                                StatementKind::Assign {
                                    slot: self.assignable(target)?,
                                    value: self.expression(value)?,
                                }
                            }
                        }
                        StmtSyntax::Compound(target, operator, value)
                            if operator.is_arithmetic() && operator != Operator::RealDivide =>
                        {
                            if let Some((root, path)) = self.projected(target) {
                                StatementKind::CompoundProject {
                                    root,
                                    path,
                                    operator,
                                    value: self.value(value, false)?,
                                }
                            } else if let Some(variable) = self.dereferenced(target) {
                                StatementKind::CompoundDeref {
                                    variable,
                                    operator,
                                    value: self.value(value, false)?,
                                }
                            } else {
                                StatementKind::Compound {
                                    slot: self.assignable(target)?,
                                    operator,
                                    value: self.value(value, false)?,
                                }
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
                        ref other => return decline(|| format!("statement {}", variant(other))),
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
    /// The root variable and field names of a place such as `p.a.b`.
    fn projected(&self, target: Id<ExprNode>) -> Option<(Variable, Vec<NameId>)> {
        let ExprSyntax::Field(base, name) = self.ast.get(target)?.kind else {
            return None;
        };
        let mut path = vec![name];
        let mut current = base;
        loop {
            match self.ast.get(current)?.kind {
                ExprSyntax::Field(inner, name) => {
                    path.push(name);
                    current = inner;
                }
                ExprSyntax::Name(root) => {
                    let visible = self.lookup(root)?;
                    path.reverse();
                    return Some((visible.slot, path));
                }
                _ => return decline(|| "field of something other than a name".into()),
            }
        }
    }

    /// The reference parameter `p` when `target` is `p.*`.
    fn dereferenced(&self, target: Id<ExprNode>) -> Option<Variable> {
        let ExprSyntax::Unary(Operator::Dereference, inner) = self.ast.get(target)?.kind else {
            return None;
        };
        let ExprSyntax::Name(name) = self.ast.get(inner)?.kind else {
            return decline(|| "dereference of something other than a reference parameter".into());
        };
        let visible = self.lookup(name)?;
        visible.reference.is_some().then_some(visible.slot)
    }

    fn assignable(&self, target: Id<ExprNode>) -> Option<Variable> {
        let ExprSyntax::Name(name) = self.ast.get(target)?.kind else {
            return decline(|| "assignment to a field, index or dereference".into());
        };
        let Some(visible) = self.lookup(name) else {
            return decline(|| "assignment to a name that is not a local".into());
        };
        if !visible.mutable {
            return decline(|| "assignment to a parameter, an immutable name or a counter".into());
        }
        Some(visible.slot)
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
        let variable = self.declare(LocalName::Source(name), span, false);
        let end = self.declare(LocalName::Hidden("$for_end"), span, false);
        let bound = self.declare(LocalName::Hidden("$for_bound"), span, false);
        // The counter is visible in the body only, not in the bounds.
        self.scopes.push(vec![Visible {
            name,
            slot: variable,
            mutable: false,
            reference: None,
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
                    self.value(a, false)?,
                    self.value(b, false)?,
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
                // The text of a string literal is already interned: reference it.
                ExprSyntax::Literal(value)
                    if matches!(ast.get(value), Some(LiteralNode::String(_))) =>
                {
                    let Some(LiteralNode::String(text)) = ast.get(value) else {
                        return None;
                    };
                    ExpressionKind::Text(*text)
                }
                ExprSyntax::Literal(value) => ExpressionKind::Literal(match ast.get(value)? {
                    LiteralNode::Number(number) => Literal::Number(*number),
                    LiteralNode::Integer(value) => Literal::Integer(*value),
                    LiteralNode::Real(value) => Literal::Real(*value),
                    LiteralNode::Boolean(value) => Literal::Boolean(*value),
                    other => return decline(|| format!("literal {}", variant(other))),
                }),
                ExprSyntax::Name(name) => match self.lookup(name) {
                    Some(visible) if visible.reference.is_some() => {
                        return decline(|| "reference parameter used as a value".into());
                    }
                    Some(visible) => ExpressionKind::Read(visible.slot),
                    // A module variable, a constant, a function value, or a name out of scope.
                    None => return decline(|| "name that is not a local".into()),
                },
                ExprSyntax::Parenthesized(inner) => {
                    ExpressionKind::Group(Box::new(self.value(inner, split)?))
                }
                ExprSyntax::Binary(operator @ (Operator::And | Operator::Or), left, right)
                    if split =>
                {
                    let left = Box::new(self.value(left, true)?);
                    let right = Box::new(self.value(right, true)?);
                    let slot =
                        self.declare(LocalName::Hidden("$short_circuit"), expression.span, false);
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
                ExprSyntax::Field(..) => {
                    let Some((root, path)) = self.projected(id) else {
                        return None;
                    };
                    ExpressionKind::Project { root, path }
                }
                ExprSyntax::Record(name, fields) => {
                    let mut written = Vec::new();
                    for &(field, value) in ast.get_list(fields)? {
                        written.push((field, self.value(value, false)?));
                    }
                    let site = self.site(name, expression.span, written.len(), false, true);
                    ExpressionKind::Record {
                        site,
                        fields: written,
                    }
                }
                ExprSyntax::Unary(Operator::Dereference, inner) => {
                    let ExprSyntax::Name(name) = ast.get(inner)?.kind else {
                        return decline(|| {
                            "dereference of something other than a reference parameter".into()
                        });
                    };
                    match self.lookup(name) {
                        Some(visible) if visible.reference.is_some() => {
                            ExpressionKind::Deref(visible.slot)
                        }
                        _ => {
                            return decline(|| {
                                "dereference of a name that is not a reference parameter".into()
                            });
                        }
                    }
                }
                ExprSyntax::Call(name, arguments) => {
                    // A local of the same name shadows the declaration.
                    if self.every_name.contains(&name) {
                        return decline(|| "call of a local name".into());
                    }
                    let arguments: Vec<Expression> = ast
                        .get_list(arguments)?
                        .iter()
                        .map(|&argument| self.argument(argument))
                        .collect::<Option<_>>()?;
                    let site = self.site(name, expression.span, arguments.len(), false, false);
                    ExpressionKind::Call(site, arguments)
                }
                ref other => return decline(|| format!("expression {}", variant(other))),
            };
            Some(Expression::new(expression.span, kind))
        })
    }
}

fn scalar_operator(operator: Operator) -> bool {
    (operator.is_arithmetic() && operator != Operator::RealDivide) || operator.is_comparison()
}

thread_local! {
    /// Why the routine last handed to `from_indexed` was declined, for coverage reports.
    static REASON: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Decline the routine, remembering the first (innermost) reason.
fn decline<T>(reason: impl FnOnce() -> String) -> Option<T> {
    REASON.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(reason());
        }
    });
    None
}

/// The name of an enum value's variant, without its fields.
fn variant(value: &impl std::fmt::Debug) -> String {
    let text = format!("{value:?}");
    text.split(['(', ' ', '{'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The reason `from_indexed` last declined on this thread.
pub(super) fn last_decline() -> Option<String> {
    REASON.with(|slot| slot.borrow_mut().take())
}

/// `&T` or `&mut T` over a scalar `T`, without a named lifetime: whether it is `&mut`, and the type.
fn reference_type(ast: &IndexedAst, node: &TypeNode) -> Option<(bool, Type)> {
    let TypeNode::Reference {
        mutable,
        lifetime: None,
        pointee,
    } = node
    else {
        return None;
    };
    let pointee = signature_type(ast, ast.get(*pointee)?)?;
    Some((
        *mutable,
        Type::Reference {
            mutable: *mutable,
            lifetime: None,
            pointee: Box::new(pointee),
        },
    ))
}

/// A type a signature may use: a scalar, or a named type, which the module graph then
/// checks is a record whose fields are all supported (HIR cannot resolve names).
fn signature_type(_ast: &IndexedAst, node: &TypeNode) -> Option<Type> {
    match node {
        // Only the fact that it is acceptable is used; the type itself is resolved later.
        TypeNode::Named(_) => Some(Type::Void),
        other => scalar_type(other),
    }
}

/// The scalar types the subset accepts: `i64`, `bool`, `f64` and the sized numbers.
fn scalar_type(node: &TypeNode) -> Option<Type> {
    Some(match node {
        TypeNode::Integer => Type::Integer,
        TypeNode::Boolean => Type::Boolean,
        TypeNode::Real => Type::Real,
        TypeNode::String => Type::String,
        TypeNode::Numeric(kind) => Type::numeric(*kind),
        _ => return None,
    })
}
