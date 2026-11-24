//! Typed analysis. AIR walks the expression trees that MIR kept and types them the way
//! the existing route does: it infers a type upward from the operands, and pushes an
//! expected type downward into the literals of an arithmetic tree (`contextualize`)
//! before the operation is typed. The order of those two passes decides which
//! diagnostic a mixed-type expression gets, so it is kept. While walking, AIR flattens
//! each statement to typed operations and builds the blocks, because `&&` and `||`
//! end a block in the middle of an expression.
use super::{LocalName, SiteId, Variable, hir, mir};
use crate::patterns;
use cyallo_ast::indexed::{IndexedAst, NameId};
use cyallo_ast::{Diagnostic, Literal, Number, NumberKind, Operator, Param, Span, Type};
use hir::{Expression, ExpressionKind, Step};
use mir::{Located, Statement};
use std::{borrow::Cow, collections::HashMap, fmt, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub(super) struct Value(pub(super) usize);

/// Half-open range into `Body::operands`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Operands {
    pub(super) start: usize,
    pub(super) len: usize,
}

pub(super) struct Body {
    pub(super) blocks: Vec<Block>,
    pub(super) types: Vec<Type>,
    pub(super) operands: Vec<Value>,
    /// Final types of the locals after the parameters, in slot order.
    pub(super) locals: Vec<Type>,
}

/// What a reference site was bound to: a callee, or the record type a literal names.
pub(super) enum Site {
    Call(Callee),
    /// The linked name of the record, whose definition the type environment holds.
    Record(Box<str>),
    /// The built-in `print`, which writes its arguments to the output.
    Print,
    /// A native member of the receiver's type, resolved from the types when it is lowered.
    Member,
    /// A mutable script variable, at its slot in the program's globals.
    Global { index: usize, ty: Type },
    /// A named function used as a value, or called through a local that holds it.
    Function {
        callee: Callee,
        kind: cyallo_ast::RoutineKind,
    },
    /// A utility resolved from its operand types: string and numeric helpers.
    Builtin(crate::intrinsic::Intrinsic),
    /// A variant of a generic enum, whose instance is the type expected where it stands.
    GenericVariant {
        owner: cyallo_ast::DeclarationId,
        name: Box<str>,
        payload: bool,
    },
    /// A variant constructor, built in place rather than by calling a function.
    Variant {
        variant: cyallo_ast::EnumVariant,
        payload: Option<Type>,
        result: Type,
    },
}

/// A native member resolved against the types of its receiver and arguments.
struct MemberPlan {
    resolved: crate::member::Resolved,
    arguments: Vec<Expression>,
    receiver_type: Type,
}

/// A field place: the root local, whether it is a reference to the record, and the field
/// index and type at each step.
pub(super) struct Place {
    pub(super) root_type: Type,
    pub(super) via_reference: bool,
    pub(super) steps: Vec<(usize, Type)>,
}

/// A callee contract bound to one reference site: the target's routine
/// identity and its signature. The callee's body is never consulted.
pub(super) struct Callee {
    pub(super) id: usize,
    pub(super) parameters: Vec<Type>,
    pub(super) result: Type,
}

pub(super) struct Block {
    pub(super) operations: Vec<Located<Operation>>,
    pub(super) exit: Located<Exit>,
}

pub(super) enum Operation {
    Statement,
    Constant(Value, Literal),
    /// A string literal, sharing the text interned in the body's unit.
    Text(Value, Arc<str>),
    ReadVariable(Value, Variable),
    /// Store into a declared local (its first store is its definition).
    Store(Variable, Value),
    Binary(Value, Operator, Value, Value),
    Unary(Value, Operator, Value),
    Call(Value, usize, Operands),
    CallStatement(usize, Operands),
    /// Write the values to the output.
    Print(Operands),
    Builtin(Value, crate::intrinsic::Intrinsic, Operands),
    /// Build a record from its fields, in declaration order.
    Aggregate(Value, Vec<Value>),
    /// Read a field of a local or of the record a reference parameter refers to.
    FieldRead(Value, Variable, Box<Place>),
    /// `[a, b, c]`: a fixed-size array of the operands.
    ArrayLiteral(Value, Operands),
    /// A named function as a value.
    FunctionValue(Value, usize),
    /// A storage or service operation a native member lowers to; the first operand is the
    /// receiver.
    Lookup(Value, crate::lookup::Operation, Operands),
    Service(Value, crate::services::Operation, Operands),
    /// A script variable read, and written.
    ReadGlobal(Value, usize),
    WriteGlobal(usize, Value),
    /// A call through a function value held in a local.
    Invoke(Option<Value>, Value, Operands),
    /// A reference to an array, viewed as a reference to a slice of its elements.
    Slice(Value, Value, bool),
    /// The element of a local array at an index value.
    IndexRead(Value, Variable, Value, bool),
    /// Store a value into the element of a local array at an index value.
    IndexWrite(Variable, Value, Value, bool),
    /// Write a field.
    FieldWrite(Variable, Box<Place>, Value),
    /// Move a value out of a local, which is then empty until it is written again.
    TakeVariable(Value, Variable),
    /// Read through a reference value.
    Load(Value, Value),
    /// Write through a reference value.
    StoreIndirect(Value, Value),
    /// A reference to a local: its address, moved into a reference value.
    Borrow(Value, Variable),
    /// Build an enum value of a variant, with its payload if it has one.
    EnumConstruct(Value, cyallo_ast::EnumVariant, Option<Value>),
    /// Whether the enum held in a local is this variant.
    EnumTest(Value, Variable, cyallo_ast::EnumVariant),
    /// The payload of the variant the local holds, read as a value.
    EnumPayload(Value, Variable, cyallo_ast::EnumVariant, Type),
    /// The payload of a non-`Copy` variant, moved out of the local that holds it.
    EnumTake(Value, Variable, cyallo_ast::EnumVariant, Type),
    /// The local's value has been (partly) moved out: it is empty on this path.
    Consume(Variable),
}

/// Targets are indexes into `Body::blocks`.
pub(super) enum Exit {
    Branch {
        condition: Value,
        yes: usize,
        no: usize,
    },
    Jump(usize),
    /// Fall into the next block: no instruction.
    Next,
    Return(Value),
    ReturnVoid,
    End,
    /// No arm of a match applied, which exhaustiveness rules out.
    Unreachable,
}

#[derive(Debug)]
enum CheckError {
    TypeMismatch { expected: Type, actual: Type },
    MissingReturn,
    InvalidMir,
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, actual } => {
                write!(
                    f,
                    "mismatched types: expected `{expected}`, found `{actual}`"
                )
            }
            Self::MissingReturn => {
                f.write_str("non-void routine must return a value on every path")
            }
            Self::InvalidMir => f.write_str("invalid routine MIR"),
        }
    }
}

impl std::error::Error for CheckError {}

fn routine_diagnostic(error: CheckError, span: Span) -> Diagnostic {
    let code = match &error {
        CheckError::TypeMismatch { .. } => "E109",
        CheckError::MissingReturn => "E103",
        CheckError::InvalidMir => "E100",
    };
    Diagnostic::new(code, error.to_string(), span)
}

fn invalid(span: Span) -> Diagnostic {
    routine_diagnostic(CheckError::InvalidMir, span)
}

fn require(expected: &Type, actual: &Type, span: Span) -> Result<(), Diagnostic> {
    if expected == actual {
        return Ok(());
    }
    let mut diagnostic = routine_diagnostic(
        CheckError::TypeMismatch {
            expected: expected.clone(),
            actual: actual.clone(),
        },
        span,
    );
    // The existing route explains the one conversion people expect to be implicit.
    if expected.number_kind().is_some_and(|kind| kind.float())
        && actual.number_kind().is_some_and(|kind| !kind.float())
    {
        diagnostic.message.push_str(
            "; integers never convert to floats implicitly: write a float literal such as `2.0` or cast with `as`",
        );
    }
    Err(diagnostic)
}

/// How the value of an expression is used.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// Looked at: the source keeps it.
    Read,
    /// Handed on: a value that is not `Copy` leaves its source.
    Move,
}

/// The type a reference refers to.
fn pointee(ty: &Type, span: Span) -> Result<Type, Diagnostic> {
    match ty {
        Type::Reference { pointee, .. } => Ok((**pointee).clone()),
        _ => Err(Diagnostic::new(
            "E109",
            "dereference requires a reference or managed owner",
            span,
        )),
    }
}

/// The type behind a reference that may be written through.
fn writable(ty: &Type, span: Span) -> Result<Type, Diagnostic> {
    let target = pointee(ty, span)?;
    match ty {
        Type::Reference { mutable: true, .. } => Ok(target),
        _ => Err(Diagnostic::new(
            "E107",
            "cannot write through a read-only reference",
            span,
        )),
    }
}

/// An argument matches its parameter, and a mutable reference may be passed where a
/// shared one of the same pointee is wanted.
/// The type of a named function used as a value.
fn function_type(kind: cyallo_ast::RoutineKind, callee: &Callee) -> Type {
    Type::Function(
        kind,
        callee
            .parameters
            .iter()
            .map(|parameter| (parameter.clone(), false))
            .collect(),
        Box::new(callee.result.clone()),
    )
}

fn require_argument(parameter: &Type, actual: &Type, span: Span) -> Result<(), Diagnostic> {
    if let (
        Type::Reference {
            mutable: wanted,
            pointee: wanted_pointee,
            ..
        },
        Type::Reference {
            mutable: given,
            pointee: given_pointee,
            ..
        },
    ) = (parameter, actual)
        && (!*wanted || *given)
        && wanted_pointee == given_pointee
    {
        return Ok(());
    }
    require(parameter, actual, span)
}

fn numeric(ty: &Type) -> bool {
    ty.number_kind().is_some()
}

/// A literal, or arithmetic over literals only: its type comes from its context.
fn contextual_number(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::Literal(Literal::Integer(_) | Literal::Real(_)) => true,
        ExpressionKind::Group(inner) => contextual_number(inner),
        ExpressionKind::Unary(Operator::Add | Operator::Subtract, inner) => {
            contextual_number(inner)
        }
        ExpressionKind::Binary(operator, left, right) if operator.is_arithmetic() => {
            contextual_number(left) && contextual_number(right)
        }
        _ => false,
    }
}

/// The type both operands of a binary operation are read in.
fn numeric_context(a: &Expression, ta: &Type, b: &Expression, tb: &Type) -> Type {
    match (contextual_number(a), contextual_number(b)) {
        (true, false) => tb.clone(),
        (false, _) => ta.clone(),
        (true, true) => {
            if *ta == Type::Real || *tb == Type::Real {
                Type::Real
            } else {
                ta.clone()
            }
        }
    }
}

/// Push a wanted numeric type into the literals of an arithmetic tree. The expression
/// is borrowed back unchanged when no literal changes, which is the usual case (an
/// `i64` literal in an `i64` context), so typing allocates nothing there.
fn contextualize<'e>(
    expression: &'e Expression,
    wanted: &Type,
) -> Result<Cow<'e, Expression>, Diagnostic> {
    if let (Type::Tuple(items), ExpressionKind::Tuple(elements)) = (wanted, &expression.kind)
        && items.len() == elements.len()
    {
        let fitted = elements
            .iter()
            .zip(items)
            .map(|(element, item)| contextualize(element, item))
            .collect::<Result<Vec<_>, _>>()?;
        if fitted.iter().all(|element| matches!(element, Cow::Borrowed(_))) {
            return Ok(Cow::Borrowed(expression));
        }
        return Ok(Cow::Owned(Expression::new(
            expression.span,
            ExpressionKind::Tuple(fitted.into_iter().map(Cow::into_owned).collect()),
        )));
    }
    let Some(kind) = wanted.number_kind() else {
        return Ok(Cow::Borrowed(expression));
    };
    cyallo_ast::with_stack(|| contextualize_number(expression, wanted, kind))
}

fn contextualize_number<'e>(
    expression: &'e Expression,
    wanted: &Type,
    kind: NumberKind,
) -> Result<Cow<'e, Expression>, Diagnostic> {
    let span = expression.span;
    match &expression.kind {
        ExpressionKind::Literal(literal @ (Literal::Integer(_) | Literal::Real(_))) => {
            let number = match literal {
                Literal::Integer(value) => Number {
                    kind: NumberKind::I64,
                    bits: *value as u64,
                },
                Literal::Real(value) => Number {
                    kind: NumberKind::F64,
                    bits: value.to_bits(),
                },
                _ => unreachable!("matched an integer or real literal"),
            };
            if number.kind.float() && !kind.float() {
                return Err(Diagnostic::new(
                    "E109",
                    "floating literal requires a floating type or an explicit cast",
                    span,
                ));
            }
            // An integer literal never becomes a float implicitly; it fails later.
            if kind.float() && !number.kind.float() {
                return Ok(Cow::Borrowed(expression));
            }
            let typed =
                Literal::number(number.cast(kind).map_err(|error| {
                    cyallo_ast::diagnostic::numeric_conversion_failed(error, span)
                })?);
            if typed == *literal {
                return Ok(Cow::Borrowed(expression));
            }
            Ok(Cow::Owned(Expression::new(
                span,
                ExpressionKind::Literal(typed),
            )))
        }
        ExpressionKind::Binary(
            operator @ (Operator::Add
            | Operator::Subtract
            | Operator::Multiply
            | Operator::Divide
            | Operator::RealDivide
            | Operator::IntegerDivide
            | Operator::Remainder),
            left,
            right,
        ) => {
            let (new_left, new_right) =
                (contextualize(left, wanted)?, contextualize(right, wanted)?);
            if matches!(
                (&new_left, &new_right),
                (Cow::Borrowed(_), Cow::Borrowed(_))
            ) {
                return Ok(Cow::Borrowed(expression));
            }
            Ok(Cow::Owned(Expression::new(
                span,
                ExpressionKind::Binary(
                    *operator,
                    Box::new(new_left.into_owned()),
                    Box::new(new_right.into_owned()),
                ),
            )))
        }
        ExpressionKind::Group(inner) => Ok(match contextualize(inner, wanted)? {
            Cow::Borrowed(_) => Cow::Borrowed(expression),
            Cow::Owned(inner) => Cow::Owned(Expression::new(
                span,
                ExpressionKind::Group(Box::new(inner)),
            )),
        }),
        ExpressionKind::Unary(operator @ (Operator::Add | Operator::Subtract), inner) => {
            Ok(match contextualize(inner, wanted)? {
                Cow::Borrowed(_) => Cow::Borrowed(expression),
                Cow::Owned(inner) => Cow::Owned(Expression::new(
                    span,
                    ExpressionKind::Unary(*operator, Box::new(inner)),
                )),
            })
        }
        _ => Ok(Cow::Borrowed(expression)),
    }
}

/// Check the routine's statements in source order, then decide from reachability
/// whether control can fall off the end of a routine that returns a value.
pub(super) fn analyze(
    body: &mir::Body,
    parameters: &[Param],
    result: &Type,
    callees: &[Site],
    definitions: &HashMap<String, Type>,
    syntax: &IndexedAst,
    routine_span: Span,
    bounds: &[(Box<str>, Vec<crate::generics::Bound>)],
    generic: Option<&super::GenericHeader>,
    services: Option<&crate::services::Metadata>,
    env: Option<&crate::declarations::TypeEnvironment>,
    offset: u32,
) -> Result<Body, Diagnostic> {
    // A short-circuit operator ends a block in the middle of an expression, so a
    // MIR block may become several. The count is syntactic, so targets are known
    // before any block is built.
    let mut start = Vec::with_capacity(body.blocks.len());
    let mut total = 0;
    for block in &body.blocks {
        start.push(total);
        total += 1 + extra_blocks(block);
    }
    let mut analysis = Analysis {
        parameters,
        locals: &body.locals,
        result,
        callees,
        definitions,
        syntax,
        bounds,
        generic,
        services,
        env,
        offset,
        start,
        types: Vec::new(),
        operands: Vec::new(),
        local_types: vec![None; body.locals.len()],
        blocks: Vec::with_capacity(total),
        operations: Vec::new(),
        guard_aliases: Vec::new(),
    };
    // The existing route fixes every local's type before it lowers any statement.
    analysis.infer_locals(body)?;
    for (index, block) in body.blocks.iter().enumerate() {
        debug_assert_eq!(analysis.blocks.len(), analysis.start[index]);
        for statement in &block.statements {
            analysis.statement(statement)?;
        }
        analysis.exit(&block.exit, index)?;
    }
    check_moves(&analysis.blocks, parameters.len() + body.locals.len())?;
    // Reachability over the finished blocks. Forward and back edges alike only
    // mark their targets; a loop header is reached by its forward edge first.
    let mut reachable = vec![false; analysis.blocks.len()];
    *reachable.first_mut().ok_or_else(|| invalid(routine_span))? = true;
    for index in 0..analysis.blocks.len() {
        if !reachable[index] {
            continue;
        }
        match analysis.blocks[index].exit.kind {
            Exit::Branch { yes, no, .. } => {
                reachable[yes] = true;
                reachable[no] = true;
            }
            Exit::Jump(target) => reachable[target] = true,
            Exit::Next => reachable[index + 1] = true,
            Exit::End if *result != Type::Void => {
                return Err(routine_diagnostic(CheckError::MissingReturn, routine_span));
            }
            Exit::Return(_) | Exit::ReturnVoid | Exit::End | Exit::Unreachable => {}
        }
    }
    let locals = analysis
        .local_types
        .into_iter()
        .map(|ty| ty.ok_or_else(|| invalid(routine_span)))
        .collect::<Result<_, _>>()?;
    Ok(Body {
        blocks: analysis.blocks,
        types: analysis.types,
        operands: analysis.operands,
        locals,
    })
}

/// A value moved out of a local may not be used again until the local is written. A move
/// on any path into a block counts, so a branch or a loop that moves a local and then
/// reaches a later use is reported at that use.
fn check_moves(blocks: &[Block], variables: usize) -> Result<(), Diagnostic> {
    fn walk(block: &Block, state: &mut [Option<Span>], report: bool) -> Result<(), Diagnostic> {
        for operation in &block.operations {
            let span = operation.span;
            match operation.kind {
                Operation::ReadVariable(_, variable)
                | Operation::Borrow(_, variable)
                | Operation::FieldRead(_, variable, _)
                | Operation::IndexRead(_, variable, _, _)
                | Operation::IndexWrite(variable, _, _, _)
                | Operation::EnumTest(_, variable, _)
                | Operation::EnumPayload(_, variable, _, _)
                | Operation::EnumTake(_, variable, _, _)
                | Operation::FieldWrite(variable, _, _)
                | Operation::TakeVariable(_, variable) => {
                    if report && let Some(moved) = state[variable.0] {
                        return Err(Diagnostic::new("E118", "use of a moved value", span)
                            .with_related(moved, "ownership transferred here"));
                    }
                    if matches!(operation.kind, Operation::TakeVariable(..)) {
                        state[variable.0] = Some(span);
                    }
                }
                Operation::Consume(variable) => state[variable.0] = Some(span),
                Operation::Store(variable, _) => state[variable.0] = None,
                _ => {}
            }
        }
        Ok(())
    }
    if !blocks.iter().any(|block| {
        block.operations.iter().any(|operation| {
            matches!(
                operation.kind,
                Operation::TakeVariable(..) | Operation::Consume(..)
            )
        })
    }) {
        return Ok(());
    }
    let successors = |index: usize| -> Vec<usize> {
        match blocks[index].exit.kind {
            Exit::Branch { yes, no, .. } => vec![yes, no],
            Exit::Jump(target) => vec![target],
            Exit::Next => vec![index + 1],
            Exit::Return(_) | Exit::ReturnVoid | Exit::End | Exit::Unreachable => vec![],
        }
    };
    let mut entry: Vec<Vec<Option<Span>>> = vec![vec![None; variables]; blocks.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..blocks.len() {
            let mut state = entry[index].clone();
            walk(&blocks[index], &mut state, false)?;
            for target in successors(index) {
                for (slot, moved) in state.iter().enumerate() {
                    if moved.is_some() && entry[target][slot].is_none() {
                        entry[target][slot] = *moved;
                        changed = true;
                    }
                }
            }
        }
    }
    for (index, block) in blocks.iter().enumerate() {
        let mut state = entry[index].clone();
        walk(block, &mut state, true)?;
    }
    Ok(())
}

/// How many blocks beyond its own a MIR block becomes: two for each short-circuit
/// operator, and for a match one test block and one jump block per arm plus the block
/// that holds the unreachable exit.
fn extra_blocks(block: &mir::Block) -> usize {
    fn count(expression: &Expression) -> usize {
        match &expression.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Text(_)
            | ExpressionKind::Read(_)
            | ExpressionKind::Deref(_)
            | ExpressionKind::Borrow { .. }
            | ExpressionKind::Reborrow(_) => 0,
            ExpressionKind::Project { .. } => 0,
            ExpressionKind::ArrayLiteral(values) => values.iter().map(count).sum(),
            ExpressionKind::Index { index, .. } => count(index),
            ExpressionKind::Cast(inner, _) => count(inner),
            ExpressionKind::Tuple(elements) => elements.iter().map(count).sum(),
            ExpressionKind::Record { fields, .. } => {
                fields.iter().map(|(_, value)| count(value)).sum()
            }
            ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => count(inner),
            ExpressionKind::Binary(_, left, right) => count(left) + count(right),
            ExpressionKind::ShortCircuit { left, right, .. } => 2 + count(left) + count(right),
            ExpressionKind::Match(matched) => {
                2 * matched.arms.len()
                    + 1
                    + count(&matched.scrutinee)
                    + matched
                        .arms
                        .iter()
                        .map(|arm| {
                            count(&arm.body)
                                + arm.guard.as_ref().map_or(0, |guard| 1 + count(guard))
                        })
                        .sum::<usize>()
            }
            ExpressionKind::Call(_, arguments)
            | ExpressionKind::Invoke { arguments, .. }
            | ExpressionKind::Member { arguments, .. } => arguments.iter().map(count).sum(),
            ExpressionKind::MethodCall(call) => call.arguments.iter().map(count).sum(),
        }
    }
    let statements: usize = block
        .statements
        .iter()
        .map(|statement| match &statement.kind {
            Statement::Define { value, .. }
            | Statement::Assign { value, .. }
            | Statement::Compound { value, .. }
            | Statement::AssignDeref { value, .. }
            | Statement::CompoundDeref { value, .. }
            | Statement::AssignProject { value, .. }
            | Statement::CompoundProject { value, .. }
            | Statement::AssignGlobal { value, .. }
            | Statement::CompoundGlobal { value, .. } => count(value),
            Statement::AssignIndex { index, value, .. }
            | Statement::CompoundIndex { index, value, .. } => count(index) + count(value),
            Statement::CallStatement(call) => count(call),
            Statement::RangeStart { start, finish, .. } => count(start) + count(finish),
            Statement::Marker
            | Statement::RangeBound { .. }
            | Statement::RangeStep { .. }
            | Statement::MatchCover { .. }
            | Statement::MatchBind { .. } => 0,
        })
        .sum();
    statements
        + match &block.exit.kind {
            mir::Exit::Branch { condition, .. } | mir::Exit::Guard { condition, .. } => {
                count(condition)
            }
            mir::Exit::Return(value) => count(value),
            _ => 0,
        }
}

/// A method found for a call: the routine that runs it (none while a template is checked),
/// its parameter and result types, and how it takes its receiver.
struct MethodTarget {
    id: usize,
    params: Vec<(Type, bool)>,
    result: Type,
    receiver: crate::services::Receiver,
}

struct Analysis<'a> {
    parameters: &'a [Param],
    locals: &'a [mir::LocalInfo],
    result: &'a Type,
    callees: &'a [Site],
    /// Record definitions, to look a field up by name.
    definitions: &'a HashMap<String, Type>,
    /// The body's unit, which holds the text of every name the body mentions.
    syntax: &'a IndexedAst,
    /// What each type parameter may be used for, when a generic template is being checked.
    bounds: &'a [(Box<str>, Vec<crate::generics::Bound>)],
    /// The routine's declared generic signature, when it is a template or an instance.
    generic: Option<&'a super::GenericHeader>,
    /// The program's traits and implementations, to find a method.
    services: Option<&'a crate::services::Metadata>,
    /// Native declarations, to resolve a member of a library type.
    env: Option<&'a crate::declarations::TypeEnvironment>,
    /// The archive offset of this file's spans, which native declaration access is keyed by.
    offset: u32,
    /// The first block built for each MIR block.
    start: Vec<usize>,
    types: Vec<Type>,
    operands: Vec<Value>,
    local_types: Vec<Option<Type>>,
    blocks: Vec<Block>,
    operations: Vec<Located<Operation>>,
    /// Bindings a guard is reading before its arm is chosen: they alias the payload.
    guard_aliases: Vec<Variable>,
}

impl Analysis<'_> {
    /// The bounds of a type parameter, or none for any other type.
    fn bounds_of(&self, ty: &Type) -> &[crate::generics::Bound] {
        match ty {
            Type::Named(name) => self
                .bounds
                .iter()
                .find(|(parameter, _)| **parameter == **name)
                .map_or(&[], |(_, bounds)| bounds),
            _ => &[],
        }
    }

    /// Whether the bounds of a type parameter allow `capability`.
    fn allows(&self, ty: &Type, capability: crate::generics::Bound) -> bool {
        self.bounds_of(ty)
            .iter()
            .any(|bound| bound.grants(capability))
    }

    /// A value of this type can be copied: it is, or a bound says every instance is.
    fn is_copy(&self, ty: &Type) -> bool {
        crate::ownership::copy(ty) || self.allows(ty, crate::generics::Bound::Copy)
    }

    /// An operator applied to a type parameter: each operand must be that parameter, and
    /// one of its bounds must give the operator. Arithmetic gives the parameter back and a
    /// comparison a boolean. `None` when neither operand is a type parameter with bounds.
    fn opaque_binary(
        &self,
        operator: Operator,
        a: &Type,
        b: &Type,
        span: Span,
    ) -> Result<Option<Type>, Diagnostic> {
        use crate::generics::Bound;
        if self.bounds_of(a).is_empty() && self.bounds_of(b).is_empty() {
            return Ok(None);
        }
        let needed = match operator {
            Operator::Add => Some((Bound::Add, false)),
            Operator::Subtract => Some((Bound::Sub, false)),
            Operator::Multiply => Some((Bound::Mul, false)),
            Operator::Divide | Operator::IntegerDivide => Some((Bound::Div, false)),
            Operator::Remainder => Some((Bound::Rem, false)),
            Operator::Equal | Operator::NotEqual => Some((Bound::Eq, true)),
            Operator::Less | Operator::LessEqual | Operator::Greater | Operator::GreaterEqual => {
                Some((Bound::Ord, true))
            }
            _ => None,
        };
        if a == b
            && let Some((capability, comparison)) = needed
            && self.allows(a, capability)
        {
            return Ok(Some(if comparison { Type::Boolean } else { a.clone() }));
        }
        Err(Diagnostic::new(
            "E105",
            format!("operator `{operator}` cannot be applied to `{a}` and `{b}`"),
            span,
        ))
    }

    fn emit(&mut self, kind: Operation, span: Span) {
        self.operations.push(Located { kind, span });
    }

    fn value(&mut self, ty: Type) -> Value {
        self.types.push(ty);
        Value(self.types.len() - 1)
    }

    /// Close the block being built and return its index.
    fn close(&mut self, exit: Exit, span: Span) -> usize {
        let operations = std::mem::take(&mut self.operations);
        self.blocks.push(Block {
            operations,
            exit: Located { kind: exit, span },
        });
        self.blocks.len() - 1
    }

    /// The interned text of a name, shared rather than copied.
    fn name(&self, name: NameId, span: Span) -> Result<Arc<str>, Diagnostic> {
        self.syntax.get(name).cloned().ok_or_else(|| invalid(span))
    }

    /// The text of a name, for comparing and for messages.
    fn text(&self, name: NameId) -> &str {
        self.syntax.name(name).unwrap_or_default()
    }

    /// A type with a record's name replaced by its definition, so fields can be looked up.
    fn view(&self, ty: &Type) -> Type {
        match ty {
            Type::Named(name) | Type::RecordRef(name) | Type::EnumRef(_, name) => self
                .definitions
                .get(&**name)
                .cloned()
                .unwrap_or_else(|| ty.clone()),
            Type::Reference {
                mutable,
                lifetime,
                pointee,
            } => Type::Reference {
                mutable: *mutable,
                lifetime: lifetime.clone(),
                pointee: Box::new(self.view(pointee)),
            },
            other => other.clone(),
        }
    }

    /// The record type a literal's site names, and its declared fields.
    fn record_type(
        &self,
        site: SiteId,
        span: Span,
    ) -> Result<(Type, Vec<(Box<str>, Type)>), Diagnostic> {
        let Some(Site::Record(target)) = self.callees.get(site.0) else {
            return Err(invalid(span));
        };
        match self.definitions.get(&**target) {
            Some(ty @ Type::Record(_, fields)) => Ok((ty.clone(), fields.clone())),
            _ => Err(Diagnostic::new("E116", "unknown record type", span)),
        }
    }

    /// Resolve `root.a.b` to field indexes and types. A reference root is read through
    /// first; writing needs a `&mut` reference, or a local declared with `mut`.
    fn place(
        &self,
        root: Variable,
        path: &[Step],
        write: bool,
        span: Span,
    ) -> Result<Place, Diagnostic> {
        let root_type = self.typed(root, span)?;
        let (mut current, via_reference) = match &root_type {
            Type::Reference {
                mutable, pointee, ..
            } => {
                if write && !*mutable {
                    return Err(Diagnostic::new(
                        "E107",
                        "cannot write through a read-only reference",
                        span,
                    ));
                }
                (self.view(pointee), true)
            }
            other => {
                if write && self.constant(root) {
                    return Err(Diagnostic::new("E107", "cannot modify a constant", span));
                }
                (self.view(other), false)
            }
        };
        let mut steps = Vec::with_capacity(path.len());
        for step in path {
            let (index, field) = match (&current, step) {
                (Type::Record(_, fields), Step::Field(name)) => {
                    let name = self.text(*name);
                    fields
                        .iter()
                        .enumerate()
                        .find(|(_, (field, _))| &**field == name)
                        .map(|(index, (_, field))| (index, field.clone()))
                }
                (Type::Tuple(items), Step::Index(index)) => {
                    items.get(*index).map(|item| (*index, item.clone()))
                }
                _ => None,
            }
            .ok_or_else(|| Diagnostic::new("E116", "unknown field or tuple index", span))?;
            let field = self.view(&field);
            steps.push((index, field.clone()));
            current = field;
        }
        Ok(Place {
            root_type,
            via_reference,
            steps,
        })
    }

    /// A record literal: the fields are evaluated in the order written, each against its
    /// declared type, then gathered in declaration order.
    fn record(
        &mut self,
        site: SiteId,
        fields: &[(NameId, Expression)],
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        let (ty, declared) = self.record_type(site, span)?;
        let mut gathered: Vec<Option<Value>> = vec![None; declared.len()];
        for (name, value) in fields {
            let name = self.text(*name);
            let Some(index) = declared.iter().position(|(field, _)| &**field == name) else {
                return Err(Diagnostic::new(
                    "E116",
                    format!("unknown field `{}`", cyallo_ast::source_name(name)),
                    value.span,
                ));
            };
            if gathered[index].is_some() {
                return Err(Diagnostic::new(
                    "E101",
                    "duplicate record field",
                    value.span,
                ));
            }
            let wanted = self.view(&declared[index].1);
            let (stored, actual) = self.expected_as(value, &wanted, Use::Move)?;
            require(&wanted, &actual, value.span)?;
            gathered[index] = Some(stored);
        }
        let Some(values) = gathered.into_iter().collect::<Option<Vec<_>>>() else {
            return Err(Diagnostic::new(
                "E116",
                "record literal must initialize every field",
                span,
            ));
        };
        let value = self.value(ty.clone());
        self.emit(Operation::Aggregate(value, values), span);
        Ok((value, ty))
    }

    /// Whether a local was declared without `mut` (parameters were not).
    /// The element type of a local array, with the number of its dimensions checked.
    fn element_of(
        &self,
        root: Variable,
        write: bool,
        span: Span,
    ) -> Result<(Type, Type), Diagnostic> {
        let array = self.typed(root, span)?;
        match &array {
            // A slice parameter: writing needs `&mut`.
            Type::Reference {
                mutable, pointee, ..
            } if matches!(**pointee, Type::Slice(_)) => {
                if write && !*mutable {
                    return Err(Diagnostic::new(
                        "E107",
                        "cannot write through a read-only reference",
                        span,
                    ));
                }
                let Type::Slice(element) = &**pointee else {
                    unreachable!()
                };
                Ok((array.clone(), self.view(element)))
            }
            Type::Array(bounds, element) if bounds.len() == 1 => {
                Ok((array.clone(), self.view(element)))
            }
            Type::Array(..) => Err(Diagnostic::new(
                "E108",
                "incorrect number of array indices",
                span,
            )),
            _ => Err(Diagnostic::new("E108", "indexing requires an array", span)),
        }
    }

    /// An index operand: an integer value.
    fn index_value(&mut self, index: &Expression) -> Result<Value, Diagnostic> {
        let (value, ty) = self.expression(index)?;
        if ty.number_kind().is_none_or(|kind| kind.float()) {
            return Err(Diagnostic::new(
                "E109",
                "index requires an integer",
                index.span,
            ));
        }
        Ok(value)
    }

    fn constant(&self, variable: Variable) -> bool {
        variable
            .0
            .checked_sub(self.parameters.len())
            .and_then(|local| self.locals.get(local))
            .is_some_and(|local| local.constant)
    }

    fn variable_type(&self, variable: Variable) -> Option<Type> {
        match variable.0.checked_sub(self.parameters.len()) {
            None => Some(self.view(&self.parameters[variable.0].ty)),
            Some(local) => self.local_types.get(local).cloned().flatten(),
        }
    }

    /// Give a local its type, or check that it already has this one.
    fn declare(&mut self, variable: Variable, ty: Type, span: Span) -> Result<(), Diagnostic> {
        let local = variable
            .0
            .checked_sub(self.parameters.len())
            .ok_or_else(|| invalid(span))?;
        match self.local_types.get_mut(local) {
            Some(slot @ None) => {
                *slot = Some(ty);
                Ok(())
            }
            Some(Some(existing)) if *existing == ty => Ok(()),
            _ => Err(invalid(span)),
        }
    }

    fn typed(&self, variable: Variable, span: Span) -> Result<Type, Diagnostic> {
        self.variable_type(variable).ok_or_else(|| invalid(span))
    }

    fn read(&mut self, variable: Variable, span: Span) -> Result<(Value, Type), Diagnostic> {
        let ty = self.typed(variable, span)?;
        let value = self.value(ty.clone());
        self.emit(Operation::ReadVariable(value, variable), span);
        Ok((value, ty))
    }

    fn store(&mut self, variable: Variable, value: Value, span: Span) {
        self.emit(Operation::Store(variable, value), span);
    }

    /// The type of an expression without lowering it. Errors are the existing
    /// route's `expression_type` errors; lowering ignores them where it does.
    fn infer(&self, expression: &Expression) -> Result<Type, Diagnostic> {
        // Only a binary operation recurses into operands that are typed again by its
        // ancestors, so only it is remembered.
        if !matches!(expression.kind, ExpressionKind::Binary(..)) {
            return self.infer_uncached(expression);
        }
        if let Some(known) = expression.inferred.get() {
            return Ok((**known).clone());
        }
        let ty = self.infer_uncached(expression)?;
        // Only a success is remembered: a failure may be fixed by a later local.
        let _ = expression.inferred.set(Box::new(ty.clone()));
        Ok(ty)
    }

    fn infer_uncached(&self, expression: &Expression) -> Result<Type, Diagnostic> {
        cyallo_ast::with_stack(|| match &expression.kind {
            ExpressionKind::Literal(literal) => Ok(super::super::literal_type(literal)),
            ExpressionKind::Text(_) => Ok(Type::String),
            ExpressionKind::Read(variable) => self
                .variable_type(*variable)
                .ok_or_else(|| Diagnostic::new("E114", "type is not known yet", expression.span)),
            ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => self.infer(inner),
            ExpressionKind::Cast(_, wanted) => Ok(wanted.clone()),
            ExpressionKind::Deref(variable) => {
                let ty = self.variable_type(*variable).ok_or_else(|| {
                    Diagnostic::new("E114", "type is not known yet", expression.span)
                })?;
                pointee(&ty, expression.span)
            }
            ExpressionKind::Project { root, path } => {
                let place = self.place(*root, path, false, expression.span)?;
                Ok(place
                    .steps
                    .last()
                    .map_or(place.root_type, |(_, ty)| ty.clone()))
            }
            ExpressionKind::Tuple(elements) => elements
                .iter()
                .map(|element| self.infer(element))
                .collect::<Result<Vec<_>, _>>()
                .map(Type::Tuple),
            ExpressionKind::Record { site, .. } => {
                self.record_type(*site, expression.span).map(|(ty, _)| ty)
            }
            ExpressionKind::ArrayLiteral(values) => {
                let element = self.infer(&values[0])?;
                Ok(Type::Array(
                    vec![(0, values.len() as i64 - 1)],
                    Box::new(element),
                ))
            }
            ExpressionKind::Index { root, .. } => {
                self.element_of(*root, false, expression.span).map(|(_, element)| element)
            }
            ExpressionKind::Reborrow(variable) => self
                .variable_type(*variable)
                .ok_or_else(|| Diagnostic::new("E114", "type is not known yet", expression.span)),
            ExpressionKind::Borrow { variable, mutable } => {
                let ty = self.variable_type(*variable).ok_or_else(|| {
                    Diagnostic::new("E114", "type is not known yet", expression.span)
                })?;
                Ok(Type::Reference {
                    mutable: *mutable,
                    lifetime: None,
                    pointee: Box::new(ty),
                })
            }
            ExpressionKind::Binary(operator, a, b) => {
                let ta = self.infer(a)?;
                let tb = self.infer(b)?;
                if let Some(ty) = self.opaque_binary(*operator, &ta, &tb, expression.span)? {
                    return Ok(ty);
                }
                if numeric(&ta) && numeric(&tb) {
                    let context = numeric_context(a, &ta, b, &tb);
                    // An operand the context left alone has the type it was just given.
                    let a = contextualize(a, &context)?;
                    let b = contextualize(b, &context)?;
                    let ta = match a {
                        Cow::Borrowed(_) => ta,
                        Cow::Owned(ref retyped) => self.infer(retyped)?,
                    };
                    let tb = match b {
                        Cow::Borrowed(_) => tb,
                        Cow::Owned(ref retyped) => self.infer(retyped)?,
                    };
                    super::super::binary_type(*operator, &ta, &tb, expression.span)
                } else {
                    super::super::binary_type(*operator, &ta, &tb, expression.span)
                }
            }
            ExpressionKind::ShortCircuit { .. } => Ok(Type::Boolean),
            ExpressionKind::Match(matched) => match matched.arms.first() {
                Some(arm) => self.infer(&arm.body),
                None => Err(invalid(expression.span)),
            },
            ExpressionKind::MethodCall(call) => {
                Ok(self.resolve_method(call, expression.span)?.result)
            }
            ExpressionKind::Invoke { callee, .. } => {
                Ok(self.function_signature(*callee, expression.span)?.1)
            }
            ExpressionKind::Member {
                receiver,
                name,
                arguments,
                ..
            } => Ok(self
                .resolve_member(*receiver, *name, arguments, expression.span)?
                .resolved
                .result),
            ExpressionKind::Call(site, arguments) => match self.callees.get(site.0) {
                Some(Site::Builtin(id)) => {
                    let (_, types) = self.builtin_arguments(*id, arguments)?;
                    self.builtin_result(*id, &types, expression.span)
                }
                Some(Site::Variant { result, .. }) => Ok(self.view(result)),
                Some(Site::Function { callee, kind }) => Ok(function_type(*kind, callee)),
                Some(Site::Global { ty, .. }) => Ok(ty.clone()),
                Some(Site::GenericVariant { .. }) => Err(Diagnostic::new(
                    "E114",
                    "the enum instance of this variant is not known here; annotate the type",
                    expression.span,
                )),
                _ => Ok(self.callee(*site, expression.span)?.result.clone()),
            },
        })
    }

    /// The parameter and result types of the function value a local holds.
    fn function_signature(
        &self,
        callee: Variable,
        span: Span,
    ) -> Result<(Vec<(Type, bool)>, Type), Diagnostic> {
        let ty = self.typed(callee, span)?;
        // A function value behind a reference parameter is called through it.
        let ty = match self.view(&ty) {
            Type::Reference { pointee, .. } => self.view(&pointee),
            other => other,
        };
        match ty {
            Type::Function(_, parameters, result) => Ok((parameters, self.view(&result))),
            _ => Err(Diagnostic::new("E110", "value is not callable", span)),
        }
    }

    /// Resolve a native member of the receiver's type from the types of the receiver and the
    /// arguments, as the existing route does: each argument first takes the member's
    /// parameter type.
    fn resolve_member(
        &self,
        receiver: Variable,
        name: NameId,
        arguments: &[Expression],
        span: Span,
    ) -> Result<MemberPlan, Diagnostic> {
        let Some(env) = self.env else {
            return Err(Diagnostic::new("E106", "unresolved method", span));
        };
        let receiver_type = self.typed(receiver, span)?;
        let text = self.text(name);
        // Native access is checked against the span's place in the program's archive.
        let placed = cyallo_ast::spans::shift_span_checked(span, self.offset).unwrap_or(span);
        let parameters =
            crate::native_contract::parameter_types(env, &receiver_type, text, placed);
        let mut fitted = Vec::with_capacity(arguments.len());
        let mut types = Vec::with_capacity(arguments.len());
        let mut stand_ins = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            let argument = match parameters.as_ref().and_then(|types| types.get(index)) {
                Some(wanted) => contextualize(argument, wanted)?.into_owned(),
                None => argument.clone(),
            };
            types.push(self.infer(&argument)?);
            // The declaration lookup reads a literal string argument; every other argument
            // is known by its type.
            let kind = match &argument.kind {
                ExpressionKind::Text(text) => cyallo_ast::ExprKind::Literal(Literal::String(
                    self.text(*text).into(),
                )),
                _ => cyallo_ast::ExprKind::Name(format!("$argument{index}").into()),
            };
            stand_ins.push(cyallo_ast::Expr {
                kind,
                span: argument.span,
            });
            fitted.push(argument);
        }
        // A diagnostic from the resolver carries a placed span; the caller places it again.
        let unplaced = |mut error: Diagnostic| {
            error.span = span;
            error
        };
        let resolved =
            crate::member::resolve(env, &receiver_type, text, &stand_ins, &types, placed)
                .map_err(unplaced)?
                .ok_or_else(|| Diagnostic::new("E116", "unknown field or tuple index", span))?;
        Ok(MemberPlan {
            resolved,
            arguments: fitted,
            receiver_type,
        })
    }

    /// `receiver.member(arguments)` on a parameter: the receiver is passed as the member's
    /// mode says, the arguments are evaluated in order, and the member's operation is one
    /// instruction.
    fn member_call(
        &mut self,
        receiver: Variable,
        name: NameId,
        arguments: &[Expression],
        span: Span,
        statement: bool,
    ) -> Result<(Value, Type), Diagnostic> {
        use crate::lookup::ReceiverMode;
        let plan = self.resolve_member(receiver, name, arguments, span)?;
        let MemberPlan {
            resolved,
            arguments,
            receiver_type,
        } = plan;
        self.member_obligations(&resolved.obligations, span)?;
        if resolved.result == Type::Void && !statement {
            return Err(Diagnostic::new(
                "E110",
                "procedure cannot be used as an expression",
                span,
            ));
        }
        let read = Expression::new(span, ExpressionKind::Read(receiver));
        let first = match resolved.receiver {
            ReceiverMode::Owner | ReceiverMode::Move => self.expression_as(&read, Use::Move)?.0,
            ReceiverMode::Value => {
                let (value, ty) = self.read(receiver, span)?;
                match self.view(&ty) {
                    Type::Reference { pointee, .. } | Type::Managed { pointee, .. } => {
                        let loaded = self.value(self.view(&pointee));
                        self.emit(Operation::Load(loaded, value), span);
                        loaded
                    }
                    _ => value,
                }
            }
            mode => {
                let mutable = mode == ReceiverMode::Mutable;
                match &receiver_type {
                    // A reference parameter already is the address of the receiver.
                    Type::Reference {
                        mutable: writable, ..
                    } => {
                        if mutable && !*writable {
                            return Err(Diagnostic::new(
                                "E107",
                                "cannot modify through a shared reference",
                                span,
                            ));
                        }
                        self.read(receiver, span)?.0
                    }
                    ty => {
                        let reference = Type::Reference {
                            mutable,
                            lifetime: None,
                            pointee: Box::new(ty.clone()),
                        };
                        let value = self.value(reference);
                        self.emit(Operation::Borrow(value, receiver), span);
                        value
                    }
                }
            }
        };
        let mut operands = vec![first];
        for (argument, parameter) in arguments.iter().zip(&resolved.params) {
            let wanted = self.view(&parameter.ty);
            let usage = if matches!(wanted, Type::Reference { .. }) {
                Use::Read
            } else {
                Use::Move
            };
            operands.push(self.expression_as(argument, usage)?.0);
        }
        if operands
            .iter()
            .any(|operand| crate::composites::borrowed(&self.types[operand.0]))
        {
            return Err(Diagnostic::new(
                "E117",
                "borrowed values cannot enter managed storage or lookup plans",
                span,
            ));
        }
        let range = Operands {
            start: self.operands.len(),
            len: operands.len(),
        };
        self.operands.extend(operands);
        let result = self.view(&resolved.result);
        let value = self.value(result.clone());
        match resolved.body {
            crate::member::Body::Storage(operation) => {
                self.emit(Operation::Lookup(value, operation, range), span)
            }
            crate::member::Body::Service(operation) => {
                self.emit(Operation::Service(value, operation, range), span)
            }
        }
        Ok((value, result))
    }

    /// The evidence a native member asks of its type arguments: an allocator, or a trait the
    /// program implements for the type.
    fn member_obligations(
        &self,
        obligations: &[crate::native_contract::TraitObligation],
        span: Span,
    ) -> Result<(), Diagnostic> {
        use crate::native_contract::TraitObligationKind;
        let Some(services) = self.services else {
            return if obligations.is_empty() {
                Ok(())
            } else {
                Err(invalid(span))
            };
        };
        for obligation in obligations {
            let ty = obligation.ty.pointee_or_self();
            let proven = match obligation.kind {
                TraitObligationKind::Allocator => services.allocator(ty).is_some(),
                TraitObligationKind::UserTrait => {
                    let name = match &obligation.bound {
                        Type::Named(name) | Type::Applied(name, _) | Type::TraitObject(name) => {
                            Some(name.as_ref())
                        }
                        _ => None,
                    };
                    name.and_then(|name| services.trait_id(name)).is_some_and(|trait_id| {
                        services.implementations.iter().any(|implementation| {
                            implementation.trait_id == trait_id
                                && crate::declarations::equivalent_types(
                                    &implementation.receiver,
                                    ty,
                                )
                        })
                    })
                }
            };
            if !proven {
                return Err(crate::declarations::invalid_declaration(
                    crate::declarations::DeclarationError::UnsatisfiedTrait {
                        ty: obligation.ty.clone(),
                        bound: obligation.bound.clone(),
                    },
                    span,
                ));
            }
        }
        Ok(())
    }

    /// The function value a local holds, loaded through the reference when it is one.
    fn function_value(&mut self, callee: Variable, span: Span) -> Result<Value, Diagnostic> {
        let (value, ty) = self.read(callee, span)?;
        match self.view(&ty) {
            Type::Reference { pointee, .. } => {
                let loaded = self.value(self.view(&pointee));
                self.emit(Operation::Load(loaded, value), span);
                Ok(loaded)
            }
            _ => Ok(value),
        }
    }

    /// The arguments of a call through a function value, against its parameter types.
    fn invoke(
        &mut self,
        callee: Variable,
        arguments: &[Expression],
        span: Span,
    ) -> Result<(Operands, Type), Diagnostic> {
        let (parameters, result) = self.function_signature(callee, span)?;
        if arguments.len() != parameters.len() {
            return Err(Diagnostic::new("E110", "incorrect argument count", span));
        }
        let mut values = Vec::with_capacity(arguments.len());
        for (argument, (parameter, _)) in arguments.iter().zip(&parameters) {
            let parameter = self.view(parameter);
            let usage = if matches!(parameter, Type::Reference { .. }) {
                Use::Read
            } else {
                Use::Move
            };
            let (value, actual) = self.expected_as(argument, &parameter, usage)?;
            require_argument(&parameter, &actual, argument.span)?;
            values.push(value);
        }
        let range = Operands {
            start: self.operands.len(),
            len: values.len(),
        };
        self.operands.extend(values);
        Ok((range, result))
    }

    /// The slot and type of the script variable a site names.
    fn global(&self, site: SiteId, span: Span) -> Result<(usize, Type), Diagnostic> {
        match self.callees.get(site.0) {
            Some(Site::Global { index, ty }) => Ok((*index, ty.clone())),
            _ => Err(invalid(span)),
        }
    }

    fn callee(&self, site: SiteId, span: Span) -> Result<&Callee, Diagnostic> {
        match self.callees.get(site.0) {
            Some(Site::Call(callee)) => Ok(callee),
            _ => Err(invalid(span)),
        }
    }

    /// `expr_expected`: contextualize to the wanted type, then lower.
    fn expected(
        &mut self,
        expression: &Expression,
        wanted: &Type,
    ) -> Result<(Value, Type), Diagnostic> {
        self.expected_as(expression, wanted, Use::Read)
    }

    fn expected_as(
        &mut self,
        expression: &Expression,
        wanted: &Type,
        usage: Use,
    ) -> Result<(Value, Type), Diagnostic> {
        if let ExpressionKind::Call(site, arguments) = &expression.kind
            && matches!(self.callees.get(site.0), Some(Site::GenericVariant { .. }))
        {
            return self.construct_generic(*site, arguments, wanted, expression.span);
        }
        let expression = contextualize(expression, wanted)?;
        let (value, actual) = self.expression_as(&expression, usage)?;
        // A reference to an array stands where a reference to a slice of its elements is wanted.
        if let Type::Reference {
            mutable: wanted_mutable,
            pointee: wanted_pointee,
            ..
        } = wanted
            && let Type::Slice(element) = &**wanted_pointee
            && let Type::Reference {
                mutable: given_mutable,
                pointee: given_pointee,
                ..
            } = &actual
            && (!*wanted_mutable || *given_mutable)
            && matches!(&**given_pointee, Type::Array(_, held) if **held == **element)
        {
            let value_type = wanted.clone();
            let slice = self.value(value_type.clone());
            self.emit(
                Operation::Slice(slice, value, *wanted_mutable),
                expression.span,
            );
            return Ok((slice, value_type));
        }
        Ok((value, actual))
    }

    fn expression(&mut self, expression: &Expression) -> Result<(Value, Type), Diagnostic> {
        self.expression_as(expression, Use::Read)
    }

    /// Lower an expression. A value that is moved out of a local (`Use::Move`) empties the
    /// local when its type is not `Copy`; only the root of a moved expression is moved, so
    /// operands of an operation are always read.
    fn expression_as(
        &mut self,
        expression: &Expression,
        usage: Use,
    ) -> Result<(Value, Type), Diagnostic> {
        cyallo_ast::with_stack(|| {
            let span = expression.span;
            match &expression.kind {
                ExpressionKind::Literal(literal) => {
                    let ty = super::super::literal_type(literal);
                    let value = self.value(ty.clone());
                    self.emit(Operation::Constant(value, literal.clone()), span);
                    Ok((value, ty))
                }
                ExpressionKind::Text(text) => {
                    let text = self.name(*text, span)?;
                    let value = self.value(Type::String);
                    self.emit(Operation::Text(value, text), span);
                    Ok((value, Type::String))
                }
                ExpressionKind::Read(variable) => {
                    let ty = self.typed(*variable, span)?;
                    if usage == Use::Move && !self.is_copy(&ty) {
                        if self.guard_aliases.iter().any(|alias| alias.0 == variable.0) {
                            return Err(patterns::invalid_pattern(
                                patterns::PatternError::GuardMove,
                                span,
                            ));
                        }
                        let value = self.value(ty.clone());
                        self.emit(Operation::TakeVariable(value, *variable), span);
                        Ok((value, ty))
                    } else {
                        self.read(*variable, span)
                    }
                }
                ExpressionKind::Reborrow(variable) => self.read(*variable, span),
                ExpressionKind::Project { root, path } => {
                    let place = self.place(*root, path, false, span)?;
                    let ty = place
                        .steps
                        .last()
                        .map_or_else(|| place.root_type.clone(), |(_, ty)| ty.clone());
                    if usage == Use::Move && !self.is_copy(&ty) {
                        return Err(Diagnostic::new(
                            "E118",
                            "cannot move a field out of a record; move the whole record or read the field in place",
                            span,
                        ));
                    }
                    let value = self.value(ty.clone());
                    self.emit(Operation::FieldRead(value, *root, Box::new(place)), span);
                    Ok((value, ty))
                }
                ExpressionKind::Tuple(elements) => {
                    let mut values = Vec::with_capacity(elements.len());
                    let mut types = Vec::with_capacity(elements.len());
                    for element in elements {
                        let (value, ty) = self.expression_as(element, Use::Move)?;
                        values.push(value);
                        types.push(ty);
                    }
                    let ty = Type::Tuple(types);
                    let value = self.value(ty.clone());
                    self.emit(Operation::Aggregate(value, values), span);
                    Ok((value, ty))
                }
                ExpressionKind::Record { site, fields } => self.record(*site, fields, span),
                ExpressionKind::Cast(inner, wanted) => {
                    let (value, got) = self.expression(inner)?;
                    if got.number_kind().is_none() {
                        return Err(cyallo_ast::diagnostic::numeric_cast_requires_number(
                            got, span,
                        ));
                    }
                    let kind = wanted.number_kind().expect("a numeric cast target");
                    let operands = Operands {
                        start: self.operands.len(),
                        len: 1,
                    };
                    self.operands.push(value);
                    let result = self.value(wanted.clone());
                    self.emit(
                        Operation::Builtin(result, crate::intrinsic::Intrinsic::cast(kind), operands),
                        span,
                    );
                    Ok((result, wanted.clone()))
                }
                ExpressionKind::ArrayLiteral(values) => {
                    let mut element: Option<Type> = None;
                    let mut operands = Vec::with_capacity(values.len());
                    for value in values {
                        let (operand, ty) = self.expression_as(value, Use::Move)?;
                        match &element {
                            Some(first) if !crate::memory::same_regions(first, &ty) => {
                                return Err(Diagnostic::new(
                                    "E109",
                                    "array literal elements must have the same type",
                                    value.span,
                                ));
                            }
                            Some(_) => {}
                            None => element = Some(ty),
                        }
                        operands.push(operand);
                    }
                    let ty = Type::Array(
                        vec![(0, values.len() as i64 - 1)],
                        Box::new(element.expect("an array literal has an element")),
                    );
                    let range = Operands {
                        start: self.operands.len(),
                        len: operands.len(),
                    };
                    self.operands.extend(operands);
                    let value = self.value(ty.clone());
                    self.emit(Operation::ArrayLiteral(value, range), span);
                    Ok((value, ty))
                }
                ExpressionKind::Index { root, index } => {
                    let (_, element) = self.element_of(*root, false, span)?;
                    if usage == Use::Move && !self.is_copy(&element) {
                        return Err(Diagnostic::new(
                            "E118",
                            "cannot move an element out of an array; read it in place or clone it",
                            span,
                        ));
                    }
                    let index = self.index_value(index)?;
                    let value = self.value(element.clone());
                    let through = matches!(self.typed(*root, span)?, Type::Reference { .. });
                    self.emit(Operation::IndexRead(value, *root, index, through), span);
                    Ok((value, element))
                }
                ExpressionKind::Deref(variable) => {
                    let (reference, ty) = self.read(*variable, span)?;
                    let target = pointee(&ty, span)?;
                    if usage == Use::Move && !self.is_copy(&target) {
                        return Err(Diagnostic::new(
                            "E118",
                            "cannot move an owned value out of borrowed or shared storage; clone it explicitly",
                            span,
                        ));
                    }
                    let value = self.value(target.clone());
                    self.emit(Operation::Load(value, reference), span);
                    Ok((value, target))
                }
                ExpressionKind::Borrow { variable, mutable } => {
                    let ty = self.typed(*variable, span)?;
                    if matches!(ty, Type::Reference { .. }) {
                        return Err(Diagnostic::new(
                            "E109",
                            "a reference to a reference is not supported",
                            span,
                        ));
                    }
                    if *mutable && self.constant(*variable) {
                        return Err(Diagnostic::new("E107", "cannot modify a constant", span));
                    }
                    let reference = Type::Reference {
                        mutable: *mutable,
                        lifetime: None,
                        pointee: Box::new(ty),
                    };
                    let value = self.value(reference.clone());
                    self.emit(Operation::Borrow(value, *variable), span);
                    Ok((value, reference))
                }
                ExpressionKind::Group(inner) => self.expression_as(inner, usage),
                ExpressionKind::Unary(operator, operand) => {
                    let (operand, ty) = self.expression(operand)?;
                    let valid = if *operator == Operator::Not {
                        ty == Type::Boolean
                    } else {
                        numeric(&ty) || self.allows(&ty, crate::generics::Bound::Neg)
                    };
                    if !valid {
                        return Err(Diagnostic::new("E105", "invalid unary operand", span));
                    }
                    let value = self.value(ty.clone());
                    self.emit(Operation::Unary(value, *operator, operand), span);
                    Ok((value, ty))
                }
                ExpressionKind::Binary(operator, a, b) => {
                    // Both operands are read in one numeric context, which is chosen
                    // from their inferred types before either is lowered.
                    // The existing route types both operands first (to find out whether
                    // either is dynamic), and a typing error in either stops it here.
                    let ta = self.infer(a)?;
                    let tb = self.infer(b)?;
                    let context =
                        (numeric(&ta) && numeric(&tb)).then(|| numeric_context(a, &ta, b, &tb));
                    let (a, b) = match &context {
                        Some(context) => (contextualize(a, context)?, contextualize(b, context)?),
                        None => (Cow::Borrowed(&**a), Cow::Borrowed(&**b)),
                    };
                    let (left, ta) = self.expression(&a)?;
                    let (right, tb) = self.expression(&b)?;
                    if let Some(ty) = self.opaque_binary(*operator, &ta, &tb, span)? {
                        let value = self.value(ty.clone());
                        self.emit(Operation::Binary(value, *operator, left, right), span);
                        return Ok((value, ty));
                    }
                    if numeric(&ta) && numeric(&tb) && ta != tb {
                        return Err(Diagnostic::new(
                            "E109",
                            "numeric operands require identical types; use a checked cast",
                            span,
                        ));
                    }
                    let ty = super::super::binary_type(*operator, &ta, &tb, span)?;
                    let value = self.value(ty.clone());
                    let operator = super::super::specialized_operator(*operator, &ty);
                    self.emit(Operation::Binary(value, operator, left, right), span);
                    Ok((value, ty))
                }
                ExpressionKind::ShortCircuit {
                    and,
                    left,
                    right,
                    slot,
                } => self.short_circuit(*and, left, right, *slot, span),
                ExpressionKind::Match(matched) => self.matched(matched, span),
                ExpressionKind::MethodCall(call) => self.method_call(call, span, false),
                ExpressionKind::Call(site, arguments)
                    if matches!(self.callees.get(site.0), Some(Site::Builtin(_))) =>
                {
                    let Some(Site::Builtin(id)) = self.callees.get(site.0) else {
                        return Err(invalid(span));
                    };
                    self.builtin(*id, arguments, span)
                }
                ExpressionKind::Call(site, arguments)
                    if matches!(self.callees.get(site.0), Some(Site::Variant { .. })) =>
                {
                    self.construct(*site, arguments, span)
                }
                ExpressionKind::Call(site, _)
                    if matches!(self.callees.get(site.0), Some(Site::Global { .. })) =>
                {
                    let Some(Site::Global { index, ty }) = self.callees.get(site.0) else {
                        return Err(invalid(span));
                    };
                    let (index, ty) = (*index, ty.clone());
                    let value = self.value(ty.clone());
                    self.emit(Operation::ReadGlobal(value, index), span);
                    Ok((value, ty))
                }
                ExpressionKind::Call(site, _)
                    if matches!(self.callees.get(site.0), Some(Site::Function { .. })) =>
                {
                    let Some(Site::Function { callee, kind }) = self.callees.get(site.0) else {
                        return Err(invalid(span));
                    };
                    let (id, ty) = (callee.id, function_type(*kind, callee));
                    let value = self.value(ty.clone());
                    self.emit(Operation::FunctionValue(value, id), span);
                    Ok((value, ty))
                }
                ExpressionKind::Member {
                    receiver,
                    name,
                    arguments,
                    ..
                } => self.member_call(*receiver, *name, arguments, span, false),
                ExpressionKind::Invoke { callee, arguments } => {
                    let (operands, result) = self.invoke(*callee, arguments, span)?;
                    if result == Type::Void {
                        return Err(Diagnostic::new(
                            "E110",
                            "procedure cannot be used as an expression",
                            span,
                        ));
                    }
                    let function = self.function_value(*callee, span)?;
                    let value = self.value(result.clone());
                    self.emit(Operation::Invoke(Some(value), function, operands), span);
                    Ok((value, result))
                }
                ExpressionKind::Call(site, arguments) => {
                    let (target, result, operands) = self.arguments(*site, arguments, span)?;
                    let value = self.value(result.clone());
                    self.emit(Operation::Call(value, target, operands), span);
                    Ok((value, result))
                }
            }
        })
    }

    /// The variant a pattern names in the enum being matched, and its index.
    fn variant_of(
        &self,
        scrutinee: &Type,
        name: NameId,
        span: Span,
    ) -> Result<(Arc<cyallo_ast::EnumType>, usize), Diagnostic> {
        let Type::Enum(contract) = self.view(scrutinee) else {
            return Err(patterns::invalid_pattern(
                patterns::PatternError::ExpectedEnum {
                    actual: scrutinee.clone(),
                },
                span,
            ));
        };
        let written = self.text(name);
        let last = |text: &str| text.rsplit("::").next().unwrap_or(text).to_owned();
        let found = contract
            .variants
            .iter()
            .position(|variant| last(&variant.name) == last(written));
        let qualifier = written.rsplit_once("::").map(|(owner, _)| last(owner));
        match found {
            None => Err(patterns::invalid_pattern(
                patterns::PatternError::UnknownVariant {
                    name: written.into(),
                },
                span,
            )),
            Some(_) if qualifier.is_some_and(|owner| owner != last(&contract.name)) => {
                Err(patterns::invalid_pattern(
                    patterns::PatternError::WrongVariant {
                        enumeration: contract.name.clone(),
                    },
                    span,
                ))
            }
            Some(index) => Ok((contract, index)),
        }
    }

    /// Give a match's hidden locals and arm bindings their types, once the type of the
    /// value matched is known. Returns whether anything was declared.
    fn declare_match(&mut self, matched: &hir::Match, span: Span) -> Result<bool, Diagnostic> {
        let mut changed = false;
        if self.variable_type(matched.held).is_none()
            && let Ok(ty) = self.infer(&matched.scrutinee)
        {
            self.declare(matched.held, ty, span)?;
            changed = true;
        }
        let Some(held) = self.variable_type(matched.held) else {
            return Ok(changed);
        };
        for arm in &matched.arms {
            let hir::Pattern::Variant { name, payload } = &arm.pattern else {
                continue;
            };
            let (contract, index) = self.variant_of(&held, *name, arm.span)?;
            let declared = contract.variants[index].payload.as_ref();
            match (declared, payload) {
                (Some(_), hir::Payload::Binding(slot)) if self.variable_type(*slot).is_none() => {
                    let ty = self.view(declared.expect("matched a payload"));
                    self.declare(*slot, ty, arm.span)?;
                    changed = true;
                }
                (Some(_), hir::Payload::Binding(_) | hir::Payload::Wildcard)
                | (None, hir::Payload::Absent) => {}
                _ => {
                    return Err(patterns::invalid_pattern(
                        patterns::PatternError::PayloadArity,
                        arm.span,
                    ));
                }
            }
        }
        if self.variable_type(matched.result).is_none()
            && let Some(first) = matched.arms.first()
            && let Ok(ty) = self.infer(&first.body)
        {
            self.declare(matched.result, ty, span)?;
            changed = true;
        }
        Ok(changed)
    }

    /// `match`: a test block and a body block per arm, joined where the value is read.
    /// Control that falls off the last arm reaches `Unreachable`, which the
    /// exhaustiveness check makes impossible.
    fn matched(&mut self, matched: &hir::Match, span: Span) -> Result<(Value, Type), Diagnostic> {
        let in_place = matches!(&matched.scrutinee.kind, ExpressionKind::Read(variable) if *variable == matched.held);
        let held = if in_place {
            self.typed(matched.held, span)?
        } else {
            let (value, ty) = self.expression_as(&matched.scrutinee, Use::Move)?;
            self.store(matched.held, value, span);
            ty
        };
        let result = self.typed(matched.result, span)?;
        let mut covered = Vec::new();
        let mut total = 0;
        let mut wildcard = false;
        let (mut tests, mut jumps) = (Vec::new(), Vec::new());
        for arm in &matched.arms {
            let test_block = match &arm.pattern {
                hir::Pattern::Wildcard => {
                    wildcard |= arm.guard.is_none();
                    self.close(Exit::Next, arm.span)
                }
                hir::Pattern::Variant { name, .. } => {
                    let (contract, index) = self.variant_of(&held, *name, arm.span)?;
                    total = contract.variants.len();
                    if arm.guard.is_none() {
                        covered.push(index);
                    }
                    let operation = contract.variants[index].operation();
                    let found = self.value(Type::Boolean);
                    self.emit(
                        Operation::EnumTest(found, matched.held, operation),
                        arm.span,
                    );
                    let next = self.blocks.len() + 1;
                    self.close(
                        Exit::Branch {
                            condition: found,
                            yes: next,
                            no: usize::MAX,
                        },
                        arm.span,
                    )
                }
            };
            let mut fails = Vec::new();
            if matches!(self.blocks[test_block].exit.kind, Exit::Branch { .. }) {
                fails.push(test_block);
            }
            let binding = match &arm.pattern {
                hir::Pattern::Variant {
                    name,
                    payload: hir::Payload::Binding(slot),
                } => {
                    let (contract, index) = self.variant_of(&held, *name, arm.span)?;
                    let operation = contract.variants[index].operation();
                    Some((*slot, self.typed(*slot, arm.span)?, operation))
                }
                _ => None,
            };
            if let Some(guard) = &arm.guard {
                // The guard reads the payload where it lies; a payload that is not `Copy`
                // is only moved into its binding once the guard has chosen the arm.
                let mut owned = false;
                if let Some((slot, ty, operation)) = &binding {
                    let value = self.value(ty.clone());
                    self.emit(
                        Operation::EnumPayload(value, matched.held, *operation, ty.clone()),
                        arm.span,
                    );
                    self.store(*slot, value, arm.span);
                    if !self.is_copy(ty) {
                        owned = true;
                        self.guard_aliases.push(*slot);
                    }
                }
                let (condition, ty) = self.expected(guard, &Type::Boolean)?;
                self.guard_aliases.clear();
                if ty != Type::Boolean {
                    return Err(patterns::invalid_pattern(
                        patterns::PatternError::GuardType { actual: ty },
                        guard.span,
                    ));
                }
                let next = self.blocks.len() + 1;
                fails.push(self.close(
                    Exit::Branch {
                        condition,
                        yes: next,
                        no: usize::MAX,
                    },
                    guard.span,
                ));
                if owned && let Some((slot, ty, operation)) = &binding {
                    let value = self.value(ty.clone());
                    self.emit(
                        Operation::EnumTake(value, matched.held, *operation, ty.clone()),
                        arm.span,
                    );
                    self.emit(Operation::Consume(matched.held), arm.span);
                    self.store(*slot, value, arm.span);
                }
            } else if let Some((slot, ty, operation)) = &binding {
                let value = self.value(ty.clone());
                if self.is_copy(ty) {
                    self.emit(
                        Operation::EnumPayload(value, matched.held, *operation, ty.clone()),
                        arm.span,
                    );
                } else {
                    // The binding owns the payload, so the matched value is spent on this path.
                    self.emit(
                        Operation::EnumTake(value, matched.held, *operation, ty.clone()),
                        arm.span,
                    );
                    self.emit(Operation::Consume(matched.held), arm.span);
                }
                self.store(*slot, value, arm.span);
            }
            let (value, ty) = self.expected_as(&arm.body, &result, Use::Move)?;
            if ty != result {
                return Err(patterns::invalid_pattern(
                    patterns::PatternError::ArmType {
                        expected: result.clone(),
                        actual: ty,
                    },
                    arm.body.span,
                ));
            }
            self.store(matched.result, value, arm.span);
            let jump = self.close(Exit::Jump(usize::MAX), arm.span);
            tests.push(test_block);
            jumps.push(jump);
            for fail in fails {
                if let Exit::Branch { condition, yes, .. } = self.blocks[fail].exit.kind {
                    self.blocks[fail].exit.kind = Exit::Branch {
                        condition,
                        yes,
                        no: jump + 1,
                    };
                }
            }
        }
        if !wildcard {
            covered.sort_unstable();
            covered.dedup();
            if covered.len() != total || total == 0 {
                let missing = match self.view(&held) {
                    Type::Enum(contract) => contract
                        .variants
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !covered.contains(index))
                        .map(|(_, variant)| variant.name.clone())
                        .collect(),
                    _ => Vec::new(),
                };
                return Err(patterns::invalid_pattern(
                    patterns::PatternError::NonExhaustive { missing },
                    span,
                ));
            }
        }
        let end = self.close(Exit::Unreachable, span) + 1;
        for jump in jumps {
            self.blocks[jump].exit.kind = Exit::Jump(end);
        }
        let _ = tests;
        self.read(matched.result, span)
    }

    /// The arguments of a utility and their types, with literals taking the type the other
    /// numeric arguments share, as the existing route contextualizes them.
    fn builtin_arguments<'e>(
        &self,
        id: crate::intrinsic::Intrinsic,
        arguments: &'e [Expression],
    ) -> Result<(Vec<Cow<'e, Expression>>, Vec<Type>), Diagnostic> {
        let context = if crate::builtins::shares_numeric_type(id) {
            arguments
                .iter()
                .find(|argument| !contextual_number(argument))
                .or_else(|| {
                    arguments.iter().find(|argument| {
                        matches!(argument.kind, ExpressionKind::Literal(Literal::Real(_)))
                    })
                })
                .or(arguments.first())
                .map(|argument| self.infer(argument))
                .transpose()?
        } else {
            None
        };
        let mut retyped = Vec::with_capacity(arguments.len());
        let mut types = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let argument = match &context {
                Some(context) => contextualize(argument, context)?,
                None => Cow::Borrowed(argument),
            };
            types.push(self.infer(&argument)?);
            retyped.push(argument);
        }
        Ok((retyped, types))
    }

    fn builtin_result(
        &self,
        id: crate::intrinsic::Intrinsic,
        types: &[Type],
        span: Span,
    ) -> Result<Type, Diagnostic> {
        crate::builtins::result_type(id, types).ok_or_else(|| {
            let found = types
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Diagnostic::new("E112", format!("`{id}` does not accept ({found})"), span)
        })
    }

    /// A utility call: arguments are read, and the result is one operation.
    fn builtin(
        &mut self,
        id: crate::intrinsic::Intrinsic,
        arguments: &[Expression],
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        let (arguments, _) = self.builtin_arguments(id, arguments)?;
        let mut values = Vec::with_capacity(arguments.len());
        let mut types = Vec::with_capacity(arguments.len());
        for argument in &arguments {
            let (value, ty) = self.expression(argument)?;
            values.push(value);
            types.push(ty);
        }
        let result = self.builtin_result(id, &types, span)?;
        let operands = Operands {
            start: self.operands.len(),
            len: values.len(),
        };
        self.operands.extend(values);
        let value = self.value(result.clone());
        self.emit(Operation::Builtin(value, id, operands), span);
        Ok((value, result))
    }

    /// `print(a, b, ...)`: every argument is evaluated, then all must be printable.
    fn print(&mut self, arguments: &[Expression], span: Span) -> Result<(), Diagnostic> {
        let mut values = Vec::with_capacity(arguments.len());
        let mut types = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let (value, ty) = self.expression(argument)?;
            values.push(value);
            types.push(ty);
        }
        if let Some(_) = types.iter().find(|ty| {
            ty.number_kind().is_none()
                && !matches!(ty, Type::String | Type::Char | Type::Boolean)
                && !self
                    .bounds_of(ty)
                    .iter()
                    .any(|bound| bound.grants(crate::generics::Bound::Format))
        }) {
            let found = types
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Diagnostic::new(
                "E112",
                format!(
                    "`{}` does not accept ({found})",
                    crate::intrinsic::Intrinsic::Print
                ),
                span,
            ));
        }
        let operands = Operands {
            start: self.operands.len(),
            len: values.len(),
        };
        self.operands.extend(values);
        self.emit(Operation::Print(operands), span);
        Ok(())
    }

    /// Find the method a call names in the traits that bound the receiver's type parameter,
    /// and, for an instance, the `impl` method of the concrete type that stands for it.
    fn resolve_method(
        &self,
        call: &hir::MethodCall,
        span: Span,
    ) -> Result<MethodTarget, Diagnostic> {
        let unresolved = |message: String| Diagnostic::new("E106", message, span);
        let name = self.text(call.name);
        let (Some(header), Some(services)) = (self.generic, self.services) else {
            return Err(unresolved(format!("unresolved method `{name}`")));
        };
        let peel = |ty: &Type| match ty {
            Type::Reference { pointee, .. } => (**pointee).clone(),
            other => other.clone(),
        };
        // What the receiver is: a type parameter while a template is checked, the concrete
        // type in an instance (whatever the receiver is: a parameter, or a local holding a
        // value of the parameter's type, such as the result of a method returning `Self`).
        let receiver_type = self.typed(call.receiver, span)?;
        let concrete = self.view(&peel(&receiver_type));
        let bounds_of = |parameter: &str| {
            header
                .bounds
                .iter()
                .find(|(candidate, _)| **candidate == *parameter)
                .map_or(&[][..], |(_, bounds)| &bounds[..])
        };
        let is_parameter = matches!(&concrete,
            Type::Named(name) if header.bounds.iter().any(|(candidate, _)| **candidate == **name)
                || header.params.iter().any(|p| matches!(&peel(&p.ty), Type::Named(n) if n == name)));
        use cyallo_ast::IdentifierEq as _;
        // The trait methods of a parameter's bounds named `name`.
        let in_bounds = |parameter: &str| -> Result<Vec<(usize, usize)>, Diagnostic> {
            let mut found = Vec::new();
            for bound in bounds_of(parameter) {
                let crate::generics::Bound::User(index) = bound else {
                    continue;
                };
                let Some(trait_id) = header
                    .user_traits
                    .get(*index as usize)
                    .and_then(|trait_name| services.trait_id(trait_name))
                else {
                    continue;
                };
                if let Some(method) = services.traits[trait_id]
                    .methods
                    .iter()
                    .position(|method| method.name.eq_identifier(name))
                    && !found.contains(&(trait_id, method))
                {
                    found.push((trait_id, method));
                }
            }
            Ok(found)
        };
        let template = is_parameter;
        let found = if let Type::Named(parameter) = &concrete
            && template
        {
            let found = in_bounds(parameter)?;
            if found.len() > 1 {
                return Err(unresolved(format!(
                    "method `{name}` is in more than one bound of `{parameter}`"
                )));
            }
            found
        } else {
            // An instance: the methods the bounds of every parameter allow, which the
            // concrete type implements.
            let mut found = Vec::new();
            for (parameter, _) in &header.bounds {
                for (trait_id, method) in in_bounds(parameter)? {
                    let implemented = services.implementations.iter().any(|implementation| {
                        implementation.trait_id == trait_id
                            && self.view(&implementation.receiver) == concrete
                    });
                    if implemented && !found.contains(&(trait_id, method)) {
                        found.push((trait_id, method));
                    }
                }
            }
            if found.len() > 1 {
                return Err(unresolved(format!(
                    "method `{name}` is in more than one bound of `{concrete}`"
                )));
            }
            found
        };
        let Some(&(trait_id, method)) = found.first() else {
            return Err(unresolved(format!(
                "`{}` has no method `{}` in its bounds",
                concrete,
                cyallo_ast::source_name(name)
            )));
        };
        let contract = &services.traits[trait_id].methods[method];
        // A template is checked against the trait; an instance calls the `impl`'s method.
        let declared = concrete.clone();
        let id = if template {
            usize::MAX
        } else {
            let implementation = services
                .implementations
                .iter()
                .find(|implementation| {
                    implementation.trait_id == trait_id
                        && self.view(&implementation.receiver) == concrete
                })
                .ok_or_else(|| {
                    unresolved(format!(
                        "`{concrete}` does not implement the trait of `{name}`"
                    ))
                })?;
            implementation.methods[method]
        };
        // `Self` in the trait's signature is the receiver's type: the parameter itself in a
        // template, the concrete type in an instance.
        let mut params = contract.params.clone();
        let mut result = contract.result.clone();
        let stand_in = if id == usize::MAX {
            declared.clone()
        } else {
            concrete.clone()
        };
        let replace = |ty: &mut Type| {
            cyallo_ast::visit::visit_type(ty, &mut |ty| {
                if *ty == Type::SelfType {
                    *ty = stand_in.clone();
                }
            });
        };
        for (parameter, _) in &mut params {
            replace(parameter);
        }
        replace(&mut result);
        Ok(MethodTarget {
            id,
            params,
            result,
            receiver: contract.receiver,
        })
    }

    /// `receiver.method(arguments)`: the receiver is passed by reference as the method
    /// declares, then the arguments against the method's parameter types.
    fn method_call(
        &mut self,
        call: &hir::MethodCall,
        span: Span,
        statement: bool,
    ) -> Result<(Value, Type), Diagnostic> {
        use crate::services::Receiver;
        let target = self.resolve_method(call, span)?;
        if target.result == Type::Void && !statement {
            return Err(Diagnostic::new(
                "E110",
                "procedure cannot be used as an expression",
                span,
            ));
        }
        let ty = self.typed(call.receiver, span)?;
        let receiver = match (&ty, target.receiver) {
            (Type::Reference { mutable: false, .. }, Receiver::Mutable) => {
                return Err(Diagnostic::new(
                    "E107",
                    "cannot modify through a shared reference",
                    span,
                ));
            }
            (Type::Reference { .. }, Receiver::Value) => {
                return Err(Diagnostic::new(
                    "E118",
                    "cannot move an owned value out of borrowed or shared storage",
                    span,
                ));
            }
            (Type::Reference { .. }, _) => self.read(call.receiver, span)?.0,
            (_, Receiver::Value) => {
                let read = Expression::new(span, ExpressionKind::Read(call.receiver));
                self.expression_as(&read, Use::Move)?.0
            }
            (_, mode) => {
                let mutable = mode == Receiver::Mutable;
                if mutable && self.constant(call.receiver) {
                    return Err(Diagnostic::new("E107", "cannot modify a constant", span));
                }
                let reference = Type::Reference {
                    mutable,
                    lifetime: None,
                    pointee: Box::new(ty.clone()),
                };
                let value = self.value(reference);
                self.emit(Operation::Borrow(value, call.receiver), span);
                value
            }
        };
        if call.arguments.len() != target.params.len() {
            return Err(invalid(span));
        }
        let mut values = vec![receiver];
        for (argument, (parameter, _)) in call.arguments.iter().zip(&target.params) {
            let parameter = self.view(parameter);
            let usage = if matches!(parameter, Type::Reference { .. }) {
                Use::Read
            } else {
                Use::Move
            };
            let (value, actual) = self.expected_as(argument, &parameter, usage)?;
            require_argument(&parameter, &actual, argument.span)?;
            values.push(value);
        }
        let operands = Operands {
            start: self.operands.len(),
            len: values.len(),
        };
        self.operands.extend(values);
        let result = self.view(&target.result);
        let value = self.value(result.clone());
        if target.result == Type::Void {
            self.emit(Operation::CallStatement(target.id, operands), span);
        } else {
            self.emit(Operation::Call(value, target.id, operands), span);
        }
        Ok((value, result))
    }

    /// A variant of a generic enum, built as the instance the context asks for: the expected
    /// type names the instance, and fixes the payload's type.
    fn construct_generic(
        &mut self,
        site: SiteId,
        arguments: &[Expression],
        wanted: &Type,
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        let callees = self.callees;
        let Some(Site::GenericVariant {
            owner,
            name,
            payload,
        }) = callees.get(site.0)
        else {
            return Err(invalid(span));
        };
        let result = self.view(wanted);
        let found = match &result {
            Type::Enum(contract) if contract.identity.declaration.0 == *owner => contract
                .variants
                .iter()
                .find(|variant| *variant.name == **name),
            _ => None,
        };
        let Some(variant) = found else {
            return Err(Diagnostic::new(
                "E109",
                format!("a variant of another enum cannot be used where `{wanted}` is expected"),
                span,
            ));
        };
        let operation = variant.operation();
        let operand = match (&variant.payload, payload, arguments) {
            (None, false, []) => None,
            (Some(declared), true, [argument]) => {
                let declared = self.view(declared);
                let (value, ty) = self.expected_as(argument, &declared, Use::Move)?;
                require_argument(&declared, &ty, argument.span)?;
                Some(value)
            }
            _ => return Err(invalid(span)),
        };
        let value = self.value(result.clone());
        self.emit(Operation::EnumConstruct(value, operation, operand), span);
        Ok((value, result))
    }

    /// A variant constructor: the variant's tag and its payload, built in place.
    fn construct(
        &mut self,
        site: SiteId,
        arguments: &[Expression],
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        let callees = self.callees;
        let Some(Site::Variant {
            variant,
            payload,
            result,
        }) = callees.get(site.0)
        else {
            return Err(invalid(span));
        };
        let result = self.view(result);
        let operand = match (payload, arguments) {
            (None, []) => None,
            (Some(parameter), [argument]) => {
                let parameter = self.view(parameter);
                let (value, ty) = self.expected_as(argument, &parameter, Use::Move)?;
                require_argument(&parameter, &ty, argument.span)?;
                Some(value)
            }
            _ => return Err(invalid(span)),
        };
        let value = self.value(result.clone());
        self.emit(Operation::EnumConstruct(value, *variant, operand), span);
        Ok((value, result))
    }

    /// Lower call arguments against the callee's parameter types, in order.
    fn arguments(
        &mut self,
        site: SiteId,
        arguments: &[Expression],
        span: Span,
    ) -> Result<(usize, Type, Operands), Diagnostic> {
        // The callee table outlives the analysis, so its parameters need no copy.
        let callees = self.callees;
        let Some(Site::Call(callee)) = callees.get(site.0) else {
            return Err(invalid(span));
        };
        let result = self.view(&callee.result);
        let parameters: Vec<Type> = callee
            .parameters
            .iter()
            .map(|parameter| self.view(parameter))
            .collect();
        let (target, parameters) = (callee.id, &parameters);
        if arguments.len() != parameters.len() {
            return Err(invalid(span));
        }
        let mut values = Vec::with_capacity(arguments.len());
        let mut loans: Vec<(Variable, bool)> = Vec::new();
        for (argument, parameter) in arguments.iter().zip(parameters) {
            let usage = if matches!(parameter, Type::Reference { .. }) {
                Use::Read
            } else {
                Use::Move
            };
            let (value, ty) = self.expected_as(argument, parameter, usage)?;
            require_argument(parameter, &ty, argument.span)?;
            match &argument.kind {
                ExpressionKind::Borrow { variable, mutable } => loans.push((*variable, *mutable)),
                ExpressionKind::Reborrow(variable) => {
                    let mutable = matches!(ty, Type::Reference { mutable: true, .. });
                    loans.push((*variable, mutable));
                }
                _ => {}
            }
            values.push(value);
        }
        // Two loans of one place in one call may not include a mutable one.
        for (index, (place, mutable)) in loans.iter().enumerate() {
            if loans[index + 1..]
                .iter()
                .any(|(other, other_mutable)| other.0 == place.0 && (*mutable || *other_mutable))
            {
                return Err(Diagnostic::new(
                    "E118",
                    "call arguments overlap and include a mutable reference",
                    span,
                ));
            }
        }
        let operands = Operands {
            start: self.operands.len(),
            len: values.len(),
        };
        self.operands.extend(values);
        Ok((target, result, operands))
    }

    /// `left && right` / `left || right`. The result lives in a hidden local,
    /// because the right side runs in its own block and values do not cross blocks.
    fn short_circuit(
        &mut self,
        and: bool,
        left: &Expression,
        right: &Expression,
        slot: Variable,
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        // The existing route never checked these operands (it moved them into a
        // boolean register); this is the check its unreachable second path intended.
        let (decided, ty) = self.expression(left)?;
        require(&Type::Boolean, &ty, span)?;
        self.declare(slot, Type::Boolean, span)?;
        self.store(slot, decided, span);
        let test = self.close(Exit::Next, span);
        let rhs = test + 1;
        let (other, ty) = self.expression(right)?;
        require(&Type::Boolean, &ty, span)?;
        self.store(slot, other, span);
        let rhs_end = self.close(Exit::Next, span);
        let end = rhs_end + 1;
        let (yes, no) = if and { (rhs, end) } else { (end, rhs) };
        self.blocks[test].exit.kind = Exit::Branch {
            condition: decided,
            yes,
            no,
        };
        self.read(slot, span)
    }

    /// The existing route fixes every local's type before it lowers anything. An
    /// annotated local has its annotation. An unannotated one takes the first type that
    /// any of its initializer or later assignments can be given, and a loop counter the
    /// type of its range. The passes repeat until nothing more is known, then the first
    /// local still unknown reports why, so a typing error here comes before any
    /// lowering error. Lowering reads these types and never changes them.
    fn infer_locals(&mut self, body: &mir::Body) -> Result<(), Diagnostic> {
        struct Candidate<'b> {
            slot: Variable,
            value: &'b Expression,
            span: Span,
        }
        let mut candidates = Vec::new();
        let mut ranges = Vec::new();
        let mut bounds = Vec::new();
        let mut matches: Vec<(&hir::Match, Span)> = Vec::new();
        let mut binds: Vec<(Variable, NameId, Variable, Span)> = Vec::new();
        for block in &body.blocks {
            match &block.exit.kind {
                mir::Exit::Branch { condition, .. } => gather_matches(condition, &mut matches),
                mir::Exit::Return(value) => gather_matches(value, &mut matches),
                _ => {}
            }
            for statement in &block.statements {
                let span = statement.span;
                match &statement.kind {
                    Statement::Define { value, .. }
                    | Statement::Assign { value, .. }
                    | Statement::Compound { value, .. }
                    | Statement::AssignDeref { value, .. }
                    | Statement::CompoundDeref { value, .. }
                    | Statement::AssignProject { value, .. }
                    | Statement::CompoundProject { value, .. }
                    | Statement::AssignGlobal { value, .. }
                    | Statement::CompoundGlobal { value, .. }
                    | Statement::CallStatement(value) => gather_matches(value, &mut matches),
                    Statement::AssignIndex { index, value, .. }
                    | Statement::CompoundIndex { index, value, .. } => {
                        gather_matches(index, &mut matches);
                        gather_matches(value, &mut matches);
                    }
                    Statement::RangeStart { start, finish, .. } => {
                        gather_matches(start, &mut matches);
                        gather_matches(finish, &mut matches);
                    }
                    _ => {}
                }
                match &statement.kind {
                    Statement::Define {
                        slot,
                        annotation: Some(ty),
                        ..
                    } => self.declare(*slot, ty.clone(), span)?,
                    Statement::Define { slot, value, .. } | Statement::Assign { slot, value } => {
                        candidates.push(Candidate {
                            slot: *slot,
                            value,
                            span,
                        });
                    }
                    Statement::MatchBind {
                        held, name, slot, ..
                    } => binds.push((*held, *name, *slot, span)),
                    Statement::RangeStart {
                        variable,
                        end,
                        start,
                        ..
                    } => ranges.push((*variable, *end, start)),
                    Statement::RangeBound { end, bound, .. } => bounds.push((*end, *bound)),
                    _ => {}
                }
            }
        }
        loop {
            let mut changed = false;
            for (variable, end, start) in &ranges {
                if self.variable_type(*variable).is_none()
                    && let Ok(ty) = self.infer(start)
                {
                    self.declare(*variable, ty.clone(), start.span)?;
                    self.declare(*end, ty, start.span)?;
                    changed = true;
                }
            }
            for (end, bound) in &bounds {
                if self.variable_type(*bound).is_none()
                    && let Some(ty) = self.variable_type(*end)
                {
                    self.declare(*bound, ty, Span::default())?;
                    changed = true;
                }
            }
            for candidate in &candidates {
                if self.variable_type(candidate.slot).is_none()
                    && let Ok(ty) = self.infer(candidate.value)
                {
                    self.declare(candidate.slot, ty, candidate.span)?;
                    changed = true;
                }
            }
            for (matched, span) in &matches {
                changed |= self.declare_match(matched, *span)?;
            }
            for (held, name, slot, span) in &binds {
                if let Some(held) = self.variable_type(*held) {
                    let (contract, index) = self.variant_of(&held, *name, *span)?;
                    if let Some(payload) = &contract.variants[index].payload {
                        let ty = self.view(payload);
                        match self.variable_type(*slot) {
                            None => {
                                self.declare(*slot, ty, *span)?;
                                changed = true;
                            }
                            // Alternatives of an or-pattern share a binding.
                            Some(existing) if existing != ty => {
                                return Err(patterns::invalid_pattern(
                                    patterns::PatternError::AlternativeBindings,
                                    *span,
                                ));
                            }
                            Some(_) => {}
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (matched, span) in &matches {
            if self.variable_type(matched.result).is_none() {
                return Err(
                    match matched.arms.first().map(|arm| self.infer(&arm.body)) {
                        Some(Err(error)) => error,
                        _ => Diagnostic::new("E114", "type of the match is not known", *span),
                    },
                );
            }
        }
        let unresolved: Vec<Variable> = candidates
            .iter()
            .map(|candidate| candidate.slot)
            .filter(|slot| self.variable_type(*slot).is_none())
            .collect();
        for candidate in &candidates {
            if self.variable_type(candidate.slot).is_some() {
                continue;
            }
            // An initializer that does not wait on another unknown local failed on its own:
            // report that error rather than hiding it.
            if !mentions(candidate.value, &unresolved)
                && let Err(error) = self.infer(candidate.value)
            {
                return Err(error);
            }
            let name = candidate
                .slot
                .0
                .checked_sub(self.parameters.len())
                .and_then(|local| body.locals.get(local))
                .map_or("", |local| match local.name {
                    LocalName::Source(id) => self.text(id),
                    LocalName::Hidden(text) => text,
                });
            return Err(Diagnostic::new(
                "E114",
                format!(
                    "cannot infer the type of `{}`; initialize it with a known value or declare its type",
                    cyallo_ast::source_name(name)
                ),
                candidate.span,
            ));
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Located<Statement>) -> Result<(), Diagnostic> {
        let span = statement.span;
        match &statement.kind {
            Statement::Marker => self.emit(Operation::Statement, span),
            Statement::MatchCover { held, arms } => {
                let held_type = self.typed(*held, span)?;
                let (mut covered, mut total, mut wildcard) = (Vec::new(), 0, false);
                for arm in arms {
                    let Some(name) = arm.name else {
                        wildcard |= !arm.guarded;
                        continue;
                    };
                    let (contract, index) = self.variant_of(&held_type, name, arm.span)?;
                    total = contract.variants.len();
                    let declared = contract.variants[index].payload.is_some();
                    if declared != !matches!(arm.payload, hir::Payload::Absent) {
                        return Err(patterns::invalid_pattern(
                            patterns::PatternError::PayloadArity,
                            arm.span,
                        ));
                    }
                    if !arm.guarded {
                        covered.push(index);
                    }
                }
                covered.sort_unstable();
                covered.dedup();
                if !wildcard && (covered.len() != total || total == 0) {
                    let missing = match self.view(&held_type) {
                        Type::Enum(contract) => contract
                            .variants
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| !covered.contains(index))
                            .map(|(_, variant)| variant.name.clone())
                            .collect(),
                        _ => Vec::new(),
                    };
                    return Err(patterns::invalid_pattern(
                        patterns::PatternError::NonExhaustive { missing },
                        span,
                    ));
                }
            }
            Statement::MatchBind {
                held,
                name,
                slot,
                stage,
            } => {
                let held_type = self.typed(*held, span)?;
                let (contract, index) = self.variant_of(&held_type, *name, span)?;
                let operation = contract.variants[index].operation();
                let ty = self.typed(*slot, span)?;
                let copy = self.is_copy(&ty);
                if *stage == mir::Stage::Take && copy {
                    return Ok(());
                }
                let value = self.value(ty.clone());
                if copy || *stage == mir::Stage::Alias {
                    self.emit(Operation::EnumPayload(value, *held, operation, ty), span);
                } else {
                    // The binding owns the payload: the matched value is spent on this path.
                    self.emit(Operation::EnumTake(value, *held, operation, ty), span);
                    self.emit(Operation::Consume(*held), span);
                }
                self.store(*slot, value, span);
            }
            Statement::Define {
                slot,
                value,
                annotation,
            } => {
                // The local's type was fixed before lowering began.
                let ty = self.typed(*slot, span)?;
                debug_assert!(
                    annotation
                        .as_ref()
                        .is_none_or(|annotation| *annotation == ty)
                );
                let (stored, actual) = self.expected_as(value, &ty, Use::Move)?;
                require(&ty, &actual, value.span)?;
                self.store(*slot, stored, span);
            }
            Statement::Assign { slot, value } => {
                let ty = self.typed(*slot, span)?;
                let (stored, actual) = self.expected_as(value, &ty, Use::Move)?;
                require(&ty, &actual, value.span)?;
                self.store(*slot, stored, span);
            }
            Statement::Compound {
                slot,
                operator,
                value,
            } => {
                let ty = self.typed(*slot, span)?;
                let (old, _) = self.read(*slot, span)?;
                let contextual = contextualize(value, &ty)?;
                let (operand, operand_type) = self.expression(&contextual)?;
                let result = super::super::binary_type(*operator, &ty, &operand_type, span)?;
                require(&ty, &result, span)?;
                let combined = self.value(result.clone());
                let operator = super::super::specialized_operator(*operator, &result);
                self.emit(Operation::Binary(combined, operator, old, operand), span);
                self.store(*slot, combined, span);
            }
            Statement::AssignGlobal { site, value } => {
                let (index, ty) = self.global(*site, span)?;
                let (stored, actual) = self.expected_as(value, &ty, Use::Move)?;
                require(&ty, &actual, value.span)?;
                self.emit(Operation::WriteGlobal(index, stored), span);
            }
            Statement::CompoundGlobal {
                site,
                operator,
                value,
            } => {
                let (index, ty) = self.global(*site, span)?;
                let old = self.value(ty.clone());
                self.emit(Operation::ReadGlobal(old, index), span);
                let contextual = contextualize(value, &ty)?;
                let (operand, operand_type) = self.expression(&contextual)?;
                let result = super::super::binary_type(*operator, &ty, &operand_type, span)?;
                require(&ty, &result, span)?;
                let combined = self.value(result.clone());
                let operator = super::super::specialized_operator(*operator, &result);
                self.emit(Operation::Binary(combined, operator, old, operand), span);
                self.emit(Operation::WriteGlobal(index, combined), span);
            }
            Statement::AssignIndex { root, index, value } => {
                let (_, element) = self.element_of(*root, true, span)?;
                if self.constant(*root) {
                    return Err(Diagnostic::new("E107", "cannot modify a constant", span));
                }
                let index = self.index_value(index)?;
                let (stored, actual) = self.expected_as(value, &element, Use::Move)?;
                require(&element, &actual, value.span)?;
                let through = matches!(self.typed(*root, span)?, Type::Reference { .. });
                self.emit(Operation::IndexWrite(*root, index, stored, through), span);
            }
            Statement::CompoundIndex {
                root,
                index,
                operator,
                value,
            } => {
                let (_, element) = self.element_of(*root, true, span)?;
                if self.constant(*root) {
                    return Err(Diagnostic::new("E107", "cannot modify a constant", span));
                }
                let index = self.index_value(index)?;
                let old = self.value(element.clone());
                let through = matches!(self.typed(*root, span)?, Type::Reference { .. });
                self.emit(Operation::IndexRead(old, *root, index, through), span);
                let contextual = contextualize(value, &element)?;
                let (operand, operand_type) = self.expression(&contextual)?;
                let result = super::super::binary_type(*operator, &element, &operand_type, span)?;
                require(&element, &result, span)?;
                let combined = self.value(result.clone());
                let operator = super::super::specialized_operator(*operator, &result);
                self.emit(Operation::Binary(combined, operator, old, operand), span);
                self.emit(Operation::IndexWrite(*root, index, combined, through), span);
            }
            Statement::AssignProject { root, path, value } => {
                let place = self.place(*root, path, true, span)?;
                let target = place
                    .steps
                    .last()
                    .map_or_else(|| place.root_type.clone(), |(_, ty)| ty.clone());
                let (stored, actual) = self.expected_as(value, &target, Use::Move)?;
                require(&target, &actual, value.span)?;
                self.emit(Operation::FieldWrite(*root, Box::new(place), stored), span);
            }
            Statement::CompoundProject {
                root,
                path,
                operator,
                value,
            } => {
                let place = self.place(*root, path, true, span)?;
                let target = place
                    .steps
                    .last()
                    .map_or_else(|| place.root_type.clone(), |(_, ty)| ty.clone());
                let old = self.value(target.clone());
                self.emit(
                    Operation::FieldRead(
                        old,
                        *root,
                        Box::new(self.place(*root, path, false, span)?),
                    ),
                    span,
                );
                let contextual = contextualize(value, &target)?;
                let (operand, operand_type) = self.expression(&contextual)?;
                let result = super::super::binary_type(*operator, &target, &operand_type, span)?;
                require(&target, &result, span)?;
                let combined = self.value(result.clone());
                let operator = super::super::specialized_operator(*operator, &result);
                self.emit(Operation::Binary(combined, operator, old, operand), span);
                self.emit(
                    Operation::FieldWrite(*root, Box::new(place), combined),
                    span,
                );
            }
            Statement::AssignDeref { variable, value } => {
                let reference = self.typed(*variable, span)?;
                let target = writable(&reference, span)?;
                let (stored, actual) = self.expected_as(value, &target, Use::Move)?;
                require(&target, &actual, value.span)?;
                let (pointer, _) = self.read(*variable, span)?;
                self.emit(Operation::StoreIndirect(pointer, stored), span);
            }
            Statement::CompoundDeref {
                variable,
                operator,
                value,
            } => {
                let reference = self.typed(*variable, span)?;
                let target = writable(&reference, span)?;
                let (pointer, _) = self.read(*variable, span)?;
                let old = self.value(target.clone());
                self.emit(Operation::Load(old, pointer), span);
                let contextual = contextualize(value, &target)?;
                let (operand, operand_type) = self.expression(&contextual)?;
                let result = super::super::binary_type(*operator, &target, &operand_type, span)?;
                require(&target, &result, span)?;
                let combined = self.value(result.clone());
                let operator = super::super::specialized_operator(*operator, &result);
                self.emit(Operation::Binary(combined, operator, old, operand), span);
                self.emit(Operation::StoreIndirect(pointer, combined), span);
            }
            Statement::CallStatement(call) => {
                if let ExpressionKind::MethodCall(method) = &call.kind {
                    self.method_call(method, call.span, true)?;
                    return Ok(());
                }
                if let ExpressionKind::Member {
                    receiver,
                    name,
                    arguments,
                    ..
                } = &call.kind
                {
                    self.member_call(*receiver, *name, arguments, call.span, true)?;
                    return Ok(());
                }
                if let ExpressionKind::Invoke { callee, arguments } = &call.kind {
                    let (operands, result) = self.invoke(*callee, arguments, call.span)?;
                    let function = self.function_value(*callee, call.span)?;
                    let value = (result != Type::Void).then(|| self.value(result));
                    self.emit(Operation::Invoke(value, function, operands), call.span);
                    return Ok(());
                }
                let ExpressionKind::Call(site, arguments) = &call.kind else {
                    return Err(invalid(span));
                };
                if matches!(self.callees.get(site.0), Some(Site::Print)) {
                    return self.print(arguments, call.span);
                }
                if matches!(self.callees.get(site.0), Some(Site::Builtin(_))) {
                    // The result is computed and dropped, as for any call statement.
                    self.expression(call)?;
                    return Ok(());
                }
                let (target, result, operands) = self.arguments(*site, arguments, call.span)?;
                if result == Type::Void {
                    self.emit(Operation::CallStatement(target, operands), call.span);
                } else {
                    // The result is computed and dropped, as the existing route does.
                    let value = self.value(result);
                    self.emit(Operation::Call(value, target, operands), call.span);
                }
            }
            Statement::RangeStart {
                variable,
                end,
                start,
                finish,
                iterable_span,
            } => {
                let (first, ty) = self.expression(start)?;
                let (last, end_type) = self.expression(finish)?;
                require(&ty, &end_type, finish.span)?;
                if ty.number_kind().is_none_or(|kind| kind.float()) {
                    return Err(Diagnostic::new(
                        "E109",
                        "for-in ranges require integer bounds",
                        *iterable_span,
                    ));
                }
                let counter = self.typed(*variable, span)?;
                require(&ty, &counter, span)?;
                self.declare(*end, ty, span)?;
                self.store(*variable, first, span);
                self.store(*end, last, span);
            }
            Statement::RangeBound {
                end,
                bound,
                inclusive,
            } => {
                let (last, ty) = self.read(*end, span)?;
                let value = if *inclusive {
                    last
                } else {
                    let one = self.one(&ty, span)?;
                    let value = self.value(ty.clone());
                    self.emit(
                        Operation::Binary(value, Operator::Subtract, last, one),
                        span,
                    );
                    value
                };
                self.declare(*bound, ty, span)?;
                self.store(*bound, value, span);
            }
            Statement::RangeStep { variable } => {
                let (current, ty) = self.read(*variable, span)?;
                let one = self.one(&ty, span)?;
                let next = self.value(ty);
                self.emit(Operation::Binary(next, Operator::Add, current, one), span);
                self.store(*variable, next, span);
            }
        }
        Ok(())
    }

    /// The literal `1` of the counter's integer type.
    fn one(&mut self, ty: &Type, span: Span) -> Result<Value, Diagnostic> {
        let kind = ty.number_kind().ok_or_else(|| invalid(span))?;
        let unit = Number::integer(kind, 1)
            .map_err(|error| cyallo_ast::diagnostic::invalid_numeric_literal(error, span))?;
        let value = self.value(ty.clone());
        self.emit(Operation::Constant(value, Literal::number(unit)), span);
        Ok(value)
    }

    fn exit(&mut self, exit: &Located<mir::Exit>, index: usize) -> Result<(), Diagnostic> {
        let span = exit.span;
        let target = |analysis: &Self, block: mir::BlockId| -> Result<usize, Diagnostic> {
            analysis
                .start
                .get(block.0)
                .copied()
                .ok_or_else(|| invalid(span))
        };
        let kind = match &exit.kind {
            mir::Exit::Branch { condition, yes, no } => {
                let (value, ty) = self.expected(condition, &Type::Boolean)?;
                require(&Type::Boolean, &ty, condition.span)?;
                Exit::Branch {
                    condition: value,
                    yes: target(self, *yes)?,
                    no: target(self, *no)?,
                }
            }
            mir::Exit::MatchTest {
                held,
                name,
                yes,
                no,
            } => {
                let held_type = self.typed(*held, span)?;
                let (contract, index) = self.variant_of(&held_type, *name, span)?;
                let found = self.value(Type::Boolean);
                let operation = contract.variants[index].operation();
                self.emit(Operation::EnumTest(found, *held, operation), span);
                Exit::Branch {
                    condition: found,
                    yes: target(self, *yes)?,
                    no: target(self, *no)?,
                }
            }
            mir::Exit::Guard {
                condition,
                alias,
                yes,
                no,
            } => {
                if let Some(slot) = alias
                    && !self.is_copy(&self.typed(*slot, span)?)
                {
                    self.guard_aliases.push(*slot);
                }
                let result = self.expected(condition, &Type::Boolean);
                self.guard_aliases.clear();
                let (value, ty) = result?;
                if ty != Type::Boolean {
                    return Err(patterns::invalid_pattern(
                        patterns::PatternError::GuardType { actual: ty },
                        condition.span,
                    ));
                }
                Exit::Branch {
                    condition: value,
                    yes: target(self, *yes)?,
                    no: target(self, *no)?,
                }
            }
            mir::Exit::Unreachable => Exit::Unreachable,
            mir::Exit::Jump(block) => Exit::Jump(target(self, *block)?),
            mir::Exit::Next(block) => {
                if target(self, *block)? != self.blocks.len() + 1 {
                    return Err(invalid(span));
                }
                Exit::Next
            }
            mir::Exit::Return(value) => {
                if *self.result == Type::Void {
                    return Err(invalid(span));
                }
                let (stored, ty) = self.expected_as(value, self.result, Use::Move)?;
                require(self.result, &ty, value.span)?;
                Exit::Return(stored)
            }
            mir::Exit::ReturnVoid => {
                if *self.result != Type::Void {
                    return Err(invalid(span));
                }
                Exit::ReturnVoid
            }
            mir::Exit::End => Exit::End,
            mir::Exit::RangeEmpty {
                variable,
                end,
                inclusive,
                yes,
                no,
            } => {
                let (first, _) = self.read(*variable, span)?;
                let (last, _) = self.read(*end, span)?;
                let test = if *inclusive {
                    Operator::Greater
                } else {
                    Operator::GreaterEqual
                };
                let empty = self.value(Type::Boolean);
                self.emit(Operation::Binary(empty, test, first, last), span);
                Exit::Branch {
                    condition: empty,
                    yes: target(self, *yes)?,
                    no: target(self, *no)?,
                }
            }
            mir::Exit::RangeDone {
                variable,
                bound,
                yes,
                no,
            } => {
                let (current, _) = self.read(*variable, span)?;
                let (last, _) = self.read(*bound, span)?;
                let finished = self.value(Type::Boolean);
                self.emit(
                    Operation::Binary(finished, Operator::Equal, current, last),
                    span,
                );
                Exit::Branch {
                    condition: finished,
                    yes: target(self, *yes)?,
                    no: target(self, *no)?,
                }
            }
        };
        let _ = index;
        self.close(kind, span);
        Ok(())
    }
}

/// Whether `expression` reads any of `variables`.
fn mentions(expression: &Expression, variables: &[Variable]) -> bool {
    match &expression.kind {
        ExpressionKind::Literal(_) | ExpressionKind::Text(_) => false,
        ExpressionKind::Read(variable)
        | ExpressionKind::Deref(variable)
        | ExpressionKind::Reborrow(variable)
        | ExpressionKind::Project { root: variable, .. }
        | ExpressionKind::Borrow { variable, .. } => {
            variables.iter().any(|other| other.0 == variable.0)
        }
        ExpressionKind::Tuple(elements) => {
            elements.iter().any(|element| mentions(element, variables))
        }
        ExpressionKind::Record { fields, .. } => {
            fields.iter().any(|(_, value)| mentions(value, variables))
        }
        ExpressionKind::ArrayLiteral(values) => {
            values.iter().any(|value| mentions(value, variables))
        }
        ExpressionKind::Cast(inner, _) => mentions(inner, variables),
        ExpressionKind::Index { root, index } => {
            variables.iter().any(|other| other.0 == root.0) || mentions(index, variables)
        }
        ExpressionKind::Match(matched) => {
            mentions(&matched.scrutinee, variables)
                || matched.arms.iter().any(|arm| {
                    mentions(&arm.body, variables)
                        || arm
                            .guard
                            .as_ref()
                            .is_some_and(|guard| mentions(guard, variables))
                })
        }
        ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => {
            mentions(inner, variables)
        }
        ExpressionKind::Binary(_, left, right)
        | ExpressionKind::ShortCircuit { left, right, .. } => {
            mentions(left, variables) || mentions(right, variables)
        }
        ExpressionKind::Invoke {
            callee: receiver,
            arguments,
        }
        | ExpressionKind::Member {
            receiver,
            arguments,
            ..
        } => {
            variables.iter().any(|other| other.0 == receiver.0)
                || arguments.iter().any(|argument| mentions(argument, variables))
        }
        ExpressionKind::MethodCall(call) => {
            variables.iter().any(|other| other.0 == call.receiver.0)
                || call
                    .arguments
                    .iter()
                    .any(|argument| mentions(argument, variables))
        }
        ExpressionKind::Call(_, arguments) => arguments
            .iter()
            .any(|argument| mentions(argument, variables)),
    }
}

/// Every match in an expression, outer before inner.
fn gather_matches<'e>(expression: &'e Expression, out: &mut Vec<(&'e hir::Match, Span)>) {
    cyallo_ast::with_stack(|| gather_in(expression, out))
}

fn gather_in<'e>(expression: &'e Expression, out: &mut Vec<(&'e hir::Match, Span)>) {
    match &expression.kind {
        ExpressionKind::Match(matched) => {
            out.push((matched, expression.span));
            gather_matches(&matched.scrutinee, out);
            for arm in &matched.arms {
                if let Some(guard) = &arm.guard {
                    gather_matches(guard, out);
                }
                gather_matches(&arm.body, out);
            }
        }
        ExpressionKind::Group(inner)
        | ExpressionKind::Unary(_, inner)
        | ExpressionKind::Cast(inner, _) => gather_matches(inner, out),
        ExpressionKind::Binary(_, left, right)
        | ExpressionKind::ShortCircuit { left, right, .. } => {
            gather_matches(left, out);
            gather_matches(right, out);
        }
        ExpressionKind::MethodCall(call) => {
            for argument in &call.arguments {
                gather_matches(argument, out);
            }
        }
        ExpressionKind::Call(_, arguments)
        | ExpressionKind::Invoke { arguments, .. }
        | ExpressionKind::Member { arguments, .. } => {
            for argument in arguments {
                gather_matches(argument, out);
            }
        }
        ExpressionKind::Tuple(elements) => {
            for element in elements {
                gather_matches(element, out);
            }
        }
        ExpressionKind::Record { fields, .. } => {
            for (_, value) in fields {
                gather_matches(value, out);
            }
        }
        _ => {}
    }
}
