use super::{LocalName, ReferenceSite, SiteId, Variable};
use cyallo_ast::indexed::{
    ExprNode, ExprSyntax, FunctionNode, Id, IndexedAst, List, LiteralNode, MatchArmNode,
    MatchBodyNode, NameId, PatternNode, PatternSyntax, StmtNode, StmtSyntax, TypeNode,
};
use std::sync::OnceLock;

use cyallo_ast::{
    FunctionBodyKind, Literal, Operator, ParameterArity, ParameterPassing, RoutineKind, Span, Type,
};

/// One step of a place: a record field by name, or a tuple element by position.
#[derive(Clone, Copy, Debug)]
pub(super) enum Step {
    Field(NameId),
    Index(usize),
}

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
        path: Vec<Step>,
    },
    /// `(a, b, ...)`: the elements in order.
    Tuple(Vec<Expression>),
    /// A string literal: its text is interned in the body's unit.
    Text(NameId),
    /// `match value { Variant(binding) => expression, ... }`.
    Match(Box<Match>),
    /// `receiver.method(arguments)` on a parameter whose type is a type parameter.
    MethodCall(Box<MethodCall>),
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

/// A method called on a parameter of generic type. Which method it is depends on the
/// bounds of that type, so it is found when the routine is typed.
#[derive(Clone, Debug)]
pub(super) struct MethodCall {
    pub(super) receiver: Variable,
    pub(super) name: NameId,
    pub(super) arguments: Vec<Expression>,
}

/// A match over an enum. The scrutinee is held in a hidden local (unless it already is a
/// local) and the value of the arm that runs is gathered in another.
#[derive(Clone, Debug)]
pub(super) struct Match {
    pub(super) scrutinee: Expression,
    pub(super) held: Variable,
    pub(super) result: Variable,
    pub(super) arms: Vec<Arm>,
}

#[derive(Clone, Debug)]
pub(super) struct Arm {
    pub(super) pattern: Pattern,
    /// `if condition`: evaluated with the arm's bindings in scope.
    pub(super) guard: Option<Expression>,
    pub(super) body: Expression,
    pub(super) span: Span,
}

/// What an arm matches. A pattern that takes a value apart (a variant, a tuple, a record)
/// names the local that holds the value it tests: the matched value for the arm's own
/// pattern, a hidden local for each part that is itself taken apart.
#[derive(Clone, Debug)]
pub(super) enum Pattern {
    Wildcard,
    /// A name bound to the part it stands for.
    Binding(Variable),
    /// The variant's name as written, possibly qualified (`Shape::Circle`).
    Variant {
        name: NameId,
        payload: Option<Box<Pattern>>,
        at: Variable,
    },
    Tuple {
        items: Vec<Pattern>,
        at: Variable,
    },
    /// Fields by name, in the order written; `rest` is a trailing `..`.
    Record {
        fields: Vec<(NameId, Pattern)>,
        rest: bool,
        at: Variable,
    },
    /// A value written out, compared with the value `at` holds.
    Literal {
        value: Expression,
        at: Variable,
    },
}

impl Pattern {
    /// A wildcard, or a variant whose payload is absent, ignored or one binding: the shapes
    /// a `match` in a value position tests without statements.
    pub(super) fn is_flat(&self) -> bool {
        match self {
            Pattern::Wildcard => true,
            Pattern::Variant { payload, .. } => matches!(
                payload.as_deref(),
                None | Some(Pattern::Wildcard | Pattern::Binding(_))
            ),
            _ => false,
        }
    }

    /// The local that holds the value this pattern tests, when it takes the value apart.
    pub(super) fn at(&self) -> Option<Variable> {
        match self {
            Pattern::Wildcard | Pattern::Binding(_) => None,
            Pattern::Variant { at, .. }
            | Pattern::Tuple { at, .. }
            | Pattern::Record { at, .. }
            | Pattern::Literal { at, .. } => Some(*at),
        }
    }

    /// The names the pattern binds, in the order written.
    pub(super) fn bindings(&self, out: &mut Vec<Variable>) {
        match self {
            Pattern::Wildcard | Pattern::Literal { .. } => {}
            Pattern::Binding(slot) => out.push(*slot),
            Pattern::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    payload.bindings(out);
                }
            }
            Pattern::Tuple { items, .. } => items.iter().for_each(|item| item.bindings(out)),
            Pattern::Record { fields, .. } => {
                fields.iter().for_each(|(_, field)| field.bindings(out))
            }
        }
    }
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
            ExpressionKind::Tuple(elements) => elements.iter().map(Expression::size).sum(),
            ExpressionKind::Text(_) => 0,
            ExpressionKind::MethodCall(call) => {
                1 + call.arguments.iter().map(Expression::size).sum::<usize>()
            }
            ExpressionKind::Match(matched) => {
                matched.scrutinee.size()
                    + matched
                        .arms
                        .iter()
                        .map(|arm| {
                            1 + arm.body.size() + arm.guard.as_ref().map_or(0, Expression::size)
                        })
                        .sum::<usize>()
            }
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
        path: Vec<Step>,
        value: Expression,
    },
    /// `p.a.b op= value`.
    CompoundProject {
        root: Variable,
        path: Vec<Step>,
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
    /// `match` whose arms are statements. An arm that yields a value assigns it to a
    /// hidden local, which the statement after this one reads.
    Match(Box<MatchStatement>),
}

pub(super) struct MatchStatement {
    pub(super) scrutinee: Expression,
    /// The local that holds the value matched: the scrutinee itself when it is a local
    /// (matched in place), otherwise a hidden local the scrutinee is assigned to.
    pub(super) held: Variable,
    pub(super) in_place: bool,
    pub(super) arms: Vec<StatementArm>,
}

pub(super) struct StatementArm {
    /// The alternatives of an or-pattern (one for an ordinary pattern). They bind the same
    /// locals, so the guard and body are shared.
    pub(super) patterns: Vec<Pattern>,
    pub(super) guard: Option<Expression>,
    pub(super) body: Vec<Statement>,
    pub(super) span: Span,
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
                        StatementKind::Match(matched) => {
                            matched.scrutinee.size()
                                + matched
                                    .arms
                                    .iter()
                                    .map(|arm| {
                                        1 + arm.guard.as_ref().map_or(0, Expression::size)
                                            + block(&arm.body)
                                    })
                                    .sum::<usize>()
                        }
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
                StatementKind::Match(matched) => matched.arms.iter().any(|arm| block(&arm.body)),
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
    /// For each parameter: whether its type is a type parameter, or a reference to one,
    /// which is what a method may be called on.
    generic_receiver: Vec<bool>,
    /// Statements to run before the one being lowered (a `match` hoisted out of its value).
    pending: Vec<Statement>,
    /// The names bound so far by the pattern being lowered.
    pattern_names: Vec<NameId>,
    /// Matching through a reference: every name the patterns bind is a reference to its
    /// part, `&mut` when this is `Some(true)`.
    pattern_reference: Option<bool>,
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
    // Type parameters are only spelled in the signature, which the module graph checks;
    // each instance is typed from its own parameter types. Bounds and packs are not lowered.
    // Bounds are resolved when the body is bound: the module graph accepts the ones the
    // pipeline gives meaning to and rejects the rest as unimplemented.
    if !ast.get_list(function.type_packs)?.is_empty() {
        return decline(|| "generic routine with packs".into());
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
        pending: Vec::new(),
        pattern_names: Vec::new(),
        pattern_reference: None,
        generic_receiver: {
            let type_params = ast.get_list(function.type_params)?;
            parameters
                .iter()
                .map(|parameter| {
                    let node = ast.get(parameter.ty);
                    let named = |id: Id<TypeNode>| {
                        matches!(ast.get(id), Some(TypeNode::Named(name)) if type_params.contains(name))
                    };
                    match node {
                        Some(TypeNode::Named(name)) => type_params.contains(name),
                        Some(TypeNode::Reference { pointee, lifetime: None, .. }) => named(*pointee),
                        _ => false,
                    }
                })
                .collect()
        },
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
fn capitalized(name: &str) -> bool {
    name.rsplit("::").next().is_some_and(|last| last.starts_with(char::is_uppercase))
}

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

    /// Whether a pattern is one `match` in a value position can test without statements.
    fn flat_syntax(&self, id: Id<PatternNode>) -> Option<bool> {
        let ast = self.ast;
        Some(match ast.get(id)?.kind {
            PatternSyntax::Wildcard => true,
            PatternSyntax::Binding(name) => capitalized(ast.name(name)?),
            PatternSyntax::Variant(_, None) => true,
            PatternSyntax::Variant(_, Some(inner)) => match ast.get(inner)?.kind {
                PatternSyntax::Wildcard => true,
                PatternSyntax::Binding(name) => !capitalized(ast.name(name)?),
                _ => false,
            },
            _ => false,
        })
    }

    /// The local a name in a pattern binds. Alternatives of one pattern bind one local
    /// between them; a name twice in one alternative is left to the existing route.
    fn binding_slot(
        &mut self,
        binding: NameId,
        span: Span,
        mut shared: Option<&mut Vec<(NameId, Variable)>>,
    ) -> Option<Variable> {
        if self.pattern_names.contains(&binding) {
            return decline(|| "name bound twice in a pattern".into());
        }
        self.pattern_names.push(binding);
        let earlier = shared.as_deref().and_then(|shared| {
            shared
                .iter()
                .find(|(name, _)| *name == binding)
                .map(|(_, slot)| *slot)
        });
        Some(match earlier {
            Some(slot) => slot,
            None => {
                let slot = self.declare(LocalName::Source(binding), span, true);
                self.scopes.last_mut()?.push(Visible {
                    name: binding,
                    slot,
                    mutable: false,
                    reference: self.pattern_reference,
                });
                if let Some(shared) = shared.as_deref_mut() {
                    shared.push((binding, slot));
                }
                slot
            }
        })
    }

    /// A pattern for the whole matched value, held in `at`. A bare lowercase name would bind
    /// the whole value, which is left to the existing route.
    fn pattern(
        &mut self,
        id: Id<PatternNode>,
        at: Variable,
        shared: Option<&mut Vec<(NameId, Variable)>>,
    ) -> Option<Pattern> {
        self.pattern_names.clear();
        self.pattern_at(id, at, shared, true)
    }

    /// A part of a pattern: a name binds it, `_` ignores it, anything else takes it apart
    /// in a hidden local that holds the part.
    fn part(
        &mut self,
        id: Id<PatternNode>,
        shared: Option<&mut Vec<(NameId, Variable)>>,
    ) -> Option<Pattern> {
        let ast = self.ast;
        let node = ast.get(id)?;
        match node.kind {
            PatternSyntax::Wildcard => Some(Pattern::Wildcard),
            PatternSyntax::Binding(name) if !capitalized(ast.name(name)?) => {
                Some(Pattern::Binding(self.binding_slot(name, node.span, shared)?))
            }
            _ => {
                let at = self.declare(LocalName::Hidden("$part"), node.span, false);
                self.pattern_at(id, at, shared, false)
            }
        }
    }

    fn pattern_at(
        &mut self,
        id: Id<PatternNode>,
        at: Variable,
        mut shared: Option<&mut Vec<(NameId, Variable)>>,
        root: bool,
    ) -> Option<Pattern> {
        let ast = self.ast;
        let node = ast.get(id)?;
        match node.kind {
            PatternSyntax::Wildcard => Some(Pattern::Wildcard),
            PatternSyntax::Binding(name) => {
                if capitalized(ast.name(name)?) {
                    Some(Pattern::Variant {
                        name,
                        payload: None,
                        at,
                    })
                } else if root {
                    decline(|| "pattern that binds the whole value".into())
                } else {
                    Some(Pattern::Binding(self.binding_slot(
                        name,
                        node.span,
                        shared.as_deref_mut(),
                    )?))
                }
            }
            PatternSyntax::Variant(name, payload) => {
                let payload = match payload {
                    None => None,
                    Some(inner) => Some(Box::new(self.part(inner, shared.as_deref_mut())?)),
                };
                Some(Pattern::Variant { name, payload, at })
            }
            PatternSyntax::Tuple(items) => {
                let mut parts = Vec::new();
                for &item in ast.get_list(items)? {
                    parts.push(self.part(item, shared.as_deref_mut())?);
                }
                Some(Pattern::Tuple { items: parts, at })
            }
            PatternSyntax::Record(fields, rest) => {
                let mut parts = Vec::new();
                for &(field, item) in ast.get_list(fields)? {
                    parts.push((field, self.part(item, shared.as_deref_mut())?));
                }
                Some(Pattern::Record {
                    fields: parts,
                    rest,
                    at,
                })
            }
            PatternSyntax::Literal(value) => {
                let kind = match ast.get(value)? {
                    LiteralNode::String(text) => ExpressionKind::Text(*text),
                    LiteralNode::Number(number) => ExpressionKind::Literal(Literal::Number(*number)),
                    LiteralNode::Integer(value) => ExpressionKind::Literal(Literal::Integer(*value)),
                    LiteralNode::Real(value) => ExpressionKind::Literal(Literal::Real(*value)),
                    LiteralNode::Boolean(value) => {
                        ExpressionKind::Literal(Literal::Boolean(*value))
                    }
                    _ => return decline(|| "literal pattern of this kind".into()),
                };
                Some(Pattern::Literal {
                    value: Expression::new(node.span, kind),
                    at,
                })
            }
            ref other => decline(|| format!("pattern {}", variant(other))),
        }
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
        name_use: bool,
    ) -> SiteId {
        self.sites.push(ReferenceSite {
            path,
            span,
            arguments,
            discard,
            record,
            name_use,
            contextual: false,
        });
        SiteId(self.sites.len() - 1)
    }

    /// Note that the expression stands where its type is fixed by the context: a returned
    /// value or a call argument. Only a generic variant constructor needs to know.
    fn contextual(&mut self, expression: &Expression) {
        match &expression.kind {
            ExpressionKind::Call(site, _) => self.sites[site.0].contextual = true,
            ExpressionKind::Group(inner) => self.contextual(inner),
            _ => {}
        }
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
            let lowered: Vec<Vec<Statement>> = ast
                .get_list(list)?
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
                            let value = match self.hoisted(value, statement.span)? {
                                Some(read) => read,
                                None => self.expression(value)?,
                            };
                            self.contextual(&value);
                            StatementKind::Return(value)
                        }
                        StmtSyntax::ReturnVoid if self.procedure => StatementKind::ReturnVoid,
                        StmtSyntax::Evaluate(value)
                            if matches!(ast.get(value)?.kind, ExprSyntax::Match(..)) =>
                        {
                            let ExprSyntax::Match(scrutinee, arms) = ast.get(value)?.kind else {
                                return None;
                            };
                            self.match_statement(scrutinee, arms, statement.span, None)?
                        }
                        StmtSyntax::Evaluate(value) => {
                            let call = self.value(value, false)?;
                            // The linker checks that the callee has a contract this subset knows.
                            match &call.kind {
                                ExpressionKind::Call(site, _) => {
                                    self.sites[site.0].discard = true;
                                }
                                ExpressionKind::MethodCall(_) => {}
                                _ => {
                                    return decline(|| "statement that is not a call".into());
                                }
                            }
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
                            let value = match self.hoisted(value, statement.span)? {
                                Some(read) => read,
                                None => self.expression(value)?,
                            };
                            let slot =
                                self.declare(LocalName::Source(name), statement.span, !mutable);
                            // A method's result may be the receiver's own type (`Self`), and
                            // then has the methods of the same bounds.
                            if matches!(value.kind, ExpressionKind::MethodCall(_)) {
                                self.generic_receiver.resize(slot.0 + 1, false);
                                self.generic_receiver[slot.0] = true;
                            }
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
                                let slot = self.assignable(target)?;
                                let value = match self.hoisted(value, statement.span)? {
                                    Some(read) => read,
                                    None => self.expression(value)?,
                                };
                                StatementKind::Assign { slot, value }
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
                    // A hoisted `match` runs first.
                    let mut produced = std::mem::take(&mut self.pending);
                    produced.push(Statement {
                        span: statement.span,
                        kind,
                    });
                    Some(produced)
                })
                .collect::<Option<_>>()?;
            Some(lowered.into_iter().flatten().collect())
        })
    }

    /// A `match` in a value position whose arms include a block: it becomes a statement
    /// that assigns the arm's value to a hidden local, and the value is a read of it.
    fn hoisted(&mut self, id: Id<ExprNode>, span: Span) -> Option<Option<Expression>> {
        let ast = self.ast;
        let ExprSyntax::Match(scrutinee, arms) = ast.get(id)?.kind else {
            return Some(None);
        };
        let mut needs_statements = false;
        for arm in ast.get_list(arms)? {
            needs_statements |= matches!(arm.body, MatchBodyNode::Block(..))
                || matches!(ast.get(arm.pattern)?.kind, PatternSyntax::Or(_))
                || !self.flat_syntax(arm.pattern)?;
        }
        needs_statements |= self.reference_scrutinee(scrutinee).is_some();
        if !needs_statements {
            return Some(None);
        }
        let result = self.declare(LocalName::Hidden("$match"), span, false);
        let kind = self.match_statement(scrutinee, arms, span, Some(result))?;
        // A value is read where every arm that finishes has assigned it. When none finishes
        // the value has no type to give, and the existing route owns that case.
        if let StatementKind::Match(matched) = &kind
            && matched.arms.iter().all(|arm| {
                matches!(
                    arm.body.last().map(|last| &last.kind),
                    Some(
                        StatementKind::Return(_)
                            | StatementKind::ReturnVoid
                            | StatementKind::Break
                            | StatementKind::Continue
                    )
                )
            })
        {
            return decline(|| "match whose arms all leave the routine or the loop".into());
        }
        self.pending.push(Statement { span, kind });
        Some(Some(Expression::new(span, ExpressionKind::Read(result))))
    }

    /// A scrutinee that is a reference: `&x`, `&mut x`, or a reference parameter. Returns
    /// whether it is `&mut`.
    fn reference_scrutinee(&self, id: Id<ExprNode>) -> Option<bool> {
        match self.ast.get(id)?.kind {
            ExprSyntax::Unary(operator @ (Operator::Borrow | Operator::BorrowMut), inner) => {
                let ExprSyntax::Name(name) = self.ast.get(inner)?.kind else {
                    return None;
                };
                let visible = self.lookup(name)?;
                visible
                    .reference
                    .is_none()
                    .then_some(operator == Operator::BorrowMut)
            }
            ExprSyntax::Name(name) => self.lookup(name)?.reference,
            _ => None,
        }
    }

    /// The expression for a reference scrutinee, which the value lowering declines.
    fn reference_value(&mut self, id: Id<ExprNode>, span: Span) -> Option<Expression> {
        let kind = match self.ast.get(id)?.kind {
            ExprSyntax::Unary(operator, inner) => {
                let ExprSyntax::Name(name) = self.ast.get(inner)?.kind else {
                    return None;
                };
                ExpressionKind::Borrow {
                    variable: self.lookup(name)?.slot,
                    mutable: operator == Operator::BorrowMut,
                }
            }
            ExprSyntax::Name(name) => ExpressionKind::Reborrow(self.lookup(name)?.slot),
            _ => return None,
        };
        Some(Expression::new(span, kind))
    }

    /// `match` as a statement, with `result` set when its value is wanted.
    fn match_statement(
        &mut self,
        scrutinee: Id<ExprNode>,
        arms: List<MatchArmNode>,
        span: Span,
        result: Option<Variable>,
    ) -> Option<StatementKind> {
        let ast = self.ast;
        let reference = self.reference_scrutinee(scrutinee);
        let scrutinee = match reference {
            Some(_) => self.reference_value(scrutinee, span)?,
            None => self.value(scrutinee, false)?,
        };
        let (held, in_place) = match &scrutinee.kind {
            ExpressionKind::Read(variable) => (*variable, true),
            _ => (
                self.declare(LocalName::Hidden("$scrutinee"), span, false),
                false,
            ),
        };
        let nodes = ast.get_list(arms)?;
        if nodes.is_empty() {
            return decline(|| "match without arms".into());
        }
        let mut lowered = Vec::new();
        let outer = std::mem::replace(&mut self.pattern_reference, reference);
        for arm in nodes {
            // The arm's bindings are visible in its guard and body only.
            self.scopes.push(Vec::new());
            let arm = self.statement_arm(arm, held, result);
            self.scopes.pop();
            match arm {
                Some(arm) => lowered.push(arm),
                None => {
                    self.pattern_reference = outer;
                    return None;
                }
            }
        }
        self.pattern_reference = outer;
        Some(StatementKind::Match(Box::new(MatchStatement {
            scrutinee,
            held,
            in_place,
            arms: lowered,
        })))
    }

    fn statement_arm(
        &mut self,
        arm: &MatchArmNode,
        held: Variable,
        result: Option<Variable>,
    ) -> Option<StatementArm> {
        let ast = self.ast;
        let patterns = match ast.get(arm.pattern)?.kind {
            PatternSyntax::Or(alternatives) => {
                let mut shared = Vec::new();
                let mut patterns = Vec::new();
                for &alternative in ast.get_list(alternatives)? {
                    patterns.push(self.pattern(alternative, held, Some(&mut shared))?);
                }
                // Every alternative must bind the same locals, and a guard could not be
                // given the payload of whichever alternative matched.
                let bound = |pattern: &Pattern| {
                    let mut slots = Vec::new();
                    pattern.bindings(&mut slots);
                    slots.sort_by_key(|slot| slot.0);
                    slots
                };
                let first = bound(patterns.first()?);
                if patterns.iter().any(|pattern| bound(pattern) != first) {
                    return decline(|| "or-pattern alternatives bind different names".into());
                }
                if !first.is_empty() && arm.guard.is_some() {
                    return decline(|| "guarded or-pattern with a binding".into());
                }
                patterns
            }
            _ => vec![self.pattern(arm.pattern, held, None)?],
        };
        let guard = match arm.guard {
            Some(guard) => Some(self.value(guard, true)?),
            None => None,
        };
        let body = match arm.body {
            MatchBodyNode::Expression(value) => self.arm_value(value, result, arm.span)?,
            MatchBodyNode::Block(list, tail) => {
                // The tail sees the block's own locals.
                self.scopes.push(Vec::new());
                let lowered = self.statements(list);
                let tail = match (&lowered, tail) {
                    (Some(_), Some(tail)) => Some(self.arm_value(tail, result, arm.span)),
                    _ => None,
                };
                self.scopes.pop();
                let mut statements = lowered?;
                match tail {
                    Some(tail) => statements.extend(tail?),
                    None => {
                        // A block that yields no value must not be asked for one, unless it
                        // never finishes.
                        let ends = matches!(
                            statements.last().map(|last| &last.kind),
                            Some(
                                StatementKind::Return(_)
                                    | StatementKind::ReturnVoid
                                    | StatementKind::Break
                                    | StatementKind::Continue
                            )
                        );
                        if result.is_some() && !ends {
                            return decline(|| "match arm block without a value".into());
                        }
                    }
                }
                statements
            }
        };
        Some(StatementArm {
            patterns,
            guard,
            body,
            span: arm.span,
        })
    }

    /// An arm's value as statements: assigned to the match's result, or, when the match is
    /// a statement, a call made for its effects.
    fn arm_value(
        &mut self,
        value: Id<ExprNode>,
        result: Option<Variable>,
        span: Span,
    ) -> Option<Vec<Statement>> {
        let kind = match result {
            Some(slot) => StatementKind::Assign {
                slot,
                value: self.expression(value)?,
            },
            None => {
                let call = self.value(value, false)?;
                let ExpressionKind::Call(site, _) = &call.kind else {
                    return decline(|| "match arm statement that is not a call".into());
                };
                self.sites[site.0].discard = true;
                StatementKind::CallStatement(call)
            }
        };
        Some(vec![Statement { span, kind }])
    }

    /// The slot of a mutable local named by `target`. Parameters, immutable names,
    /// loop counters and anything else stay with the existing route, which owns
    /// those diagnostics.
    /// The base and step of one field or tuple-element access.
    fn step_of(&self, target: Id<ExprNode>) -> Option<(Id<ExprNode>, Step)> {
        match self.ast.get(target)?.kind {
            ExprSyntax::Field(base, name) => Some((base, Step::Field(name))),
            ExprSyntax::TupleField(base, index) => Some((base, Step::Index(index))),
            _ => None,
        }
    }

    /// The root variable and steps of a place such as `p.a.0.b`.
    fn projected(&self, target: Id<ExprNode>) -> Option<(Variable, Vec<Step>)> {
        let (mut current, first) = self.step_of(target)?;
        let mut path = vec![first];
        loop {
            if let Some((inner, step)) = self.step_of(current) {
                path.push(step);
                current = inner;
                continue;
            }
            match self.ast.get(current)?.kind {
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
                    // Not a local: a unit variant, a module variable, a constant, or a function
                    // value. The module graph accepts it only as a unit variant.
                    None => {
                        let site = self.site(name, expression.span, 0, false, false, true);
                        ExpressionKind::Call(site, Vec::new())
                    }
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
                ExprSyntax::Field(..) | ExprSyntax::TupleField(..) => {
                    let Some((root, path)) = self.projected(id) else {
                        return None;
                    };
                    ExpressionKind::Project { root, path }
                }
                ExprSyntax::Tuple(elements) => {
                    let mut written = Vec::new();
                    for &element in ast.get_list(elements)? {
                        written.push(self.value(element, false)?);
                    }
                    ExpressionKind::Tuple(written)
                }
                ExprSyntax::Record(name, fields) => {
                    let mut written = Vec::new();
                    for &(field, value) in ast.get_list(fields)? {
                        written.push((field, self.value(value, false)?));
                    }
                    let site = self.site(name, expression.span, written.len(), false, true, false);
                    ExpressionKind::Record {
                        site,
                        fields: written,
                    }
                }
                ExprSyntax::Match(scrutinee, arms) => {
                    // A match starts new blocks, so like `&&` it may not sit under an operator.
                    if !split {
                        return decline(|| "match under an operator".into());
                    }
                    let scrutinee = self.value(scrutinee, false)?;
                    // A local is tested in place; a call's result is first held in a local.
                    let held = match &scrutinee.kind {
                        ExpressionKind::Read(variable) => *variable,
                        _ => self.declare(LocalName::Hidden("$scrutinee"), expression.span, false),
                    };
                    if ast.get_list(arms)?.is_empty() {
                        return decline(|| "match without arms".into());
                    }
                    let result = self.declare(LocalName::Hidden("$match"), expression.span, false);
                    let mut lowered = Vec::new();
                    for arm in ast.get_list(arms)? {
                        let MatchBodyNode::Expression(body) = arm.body else {
                            return decline(|| "match arm with a block".into());
                        };
                        // The arm's bindings are visible in its body only.
                        self.scopes.push(Vec::new());
                        let pattern = self
                            .pattern(arm.pattern, held, None)
                            .filter(Pattern::is_flat)
                            .or_else(|| decline(|| "nested pattern under an operator".into()));
                        let guard = match (pattern.as_ref(), arm.guard) {
                            (Some(_), Some(guard)) => Some(self.value(guard, true)),
                            _ => None,
                        };
                        let body = pattern.as_ref().and_then(|_| self.value(body, true));
                        self.scopes.pop();
                        let guard = match guard {
                            Some(guard) => Some(guard?),
                            None => None,
                        };
                        lowered.push(Arm {
                            pattern: pattern?,
                            guard,
                            body: body?,
                            span: arm.span,
                        });
                    }
                    ExpressionKind::Match(Box::new(Match {
                        scrutinee,
                        held,
                        result,
                        arms: lowered,
                    }))
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
                ExprSyntax::Invoke(callee, arguments) => {
                    let ExprSyntax::Field(base, method) = ast.get(callee)?.kind else {
                        return decline(|| "call of a value".into());
                    };
                    let ExprSyntax::Name(receiver) = ast.get(base)?.kind else {
                        return decline(|| "method call on something other than a name".into());
                    };
                    let visible = self.lookup(receiver)?;
                    if !self
                            .generic_receiver
                            .get(visible.slot.0)
                            .copied()
                            .unwrap_or(false)
                    {
                        return decline(|| {
                            "method call on something other than a generic parameter or a method result".into()
                        });
                    }
                    let arguments: Vec<Expression> = ast
                        .get_list(arguments)?
                        .iter()
                        .map(|&argument| self.argument(argument))
                        .collect::<Option<_>>()?;
                    ExpressionKind::MethodCall(Box::new(MethodCall {
                        receiver: visible.slot,
                        name: method,
                        arguments,
                    }))
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
                    for argument in &arguments {
                        self.contextual(argument);
                    }
                    let site =
                        self.site(name, expression.span, arguments.len(), false, false, false);
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
fn signature_type(ast: &IndexedAst, node: &TypeNode) -> Option<Type> {
    match node {
        // Only the fact that it is acceptable is used; the type itself is resolved later.
        TypeNode::Named(_) | TypeNode::Applied(..) => Some(Type::Void),
        TypeNode::Tuple(elements) => {
            let all = ast.get_list(*elements)?.iter().all(|&element| {
                ast.get(element)
                    .is_some_and(|node| signature_type(ast, node).is_some())
            });
            all.then_some(Type::Void)
        }
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
