//! Typed analysis. AIR walks the expression trees that MIR kept and types them the way
//! the existing route does: it infers a type upward from the operands, and pushes an
//! expected type downward into the literals of an arithmetic tree (`contextualize`)
//! before the operation is typed. The order of those two passes decides which
//! diagnostic a mixed-type expression gets, so it is kept. While walking, AIR flattens
//! each statement to typed operations and builds the blocks, because `&&` and `||`
//! end a block in the middle of an expression.
use super::{SiteId, Variable, hir, mir};
use cyallo_ast::{Diagnostic, Literal, Number, NumberKind, Operator, Param, Span, Type};
use hir::{Expression, ExpressionKind};
use mir::{Located, Statement};
use std::{borrow::Cow, collections::HashMap, fmt};

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
    ReadVariable(Value, Variable),
    /// Store into a declared local (its first store is its definition).
    Store(Variable, Value),
    Binary(Value, Operator, Value, Value),
    Unary(Value, Operator, Value),
    Call(Value, usize, Operands),
    CallStatement(usize, Operands),
    /// Build a record from its fields, in declaration order.
    Aggregate(Value, Vec<Value>),
    /// Read a field of a local or of the record a reference parameter refers to.
    FieldRead(Value, Variable, Box<Place>),
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
    let Some(kind) = wanted.number_kind() else {
        return Ok(Cow::Borrowed(expression));
    };
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
    routine_span: Span,
) -> Result<Body, Diagnostic> {
    // A short-circuit operator ends a block in the middle of an expression, so a
    // MIR block may become several. The count is syntactic, so targets are known
    // before any block is built.
    let mut start = Vec::with_capacity(body.blocks.len());
    let mut total = 0;
    for block in &body.blocks {
        start.push(total);
        total += 1 + 2 * short_circuits(block);
    }
    let mut analysis = Analysis {
        parameters,
        locals: &body.locals,
        result,
        callees,
        definitions,
        start,
        types: Vec::new(),
        operands: Vec::new(),
        local_types: vec![None; body.locals.len()],
        blocks: Vec::with_capacity(total),
        operations: Vec::new(),
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
            Exit::Return(_) | Exit::ReturnVoid | Exit::End => {}
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
                Operation::Store(variable, _) => state[variable.0] = None,
                _ => {}
            }
        }
        Ok(())
    }
    if !blocks.iter().any(|block| {
        block
            .operations
            .iter()
            .any(|operation| matches!(operation.kind, Operation::TakeVariable(..)))
    }) {
        return Ok(());
    }
    let successors = |index: usize| -> Vec<usize> {
        match blocks[index].exit.kind {
            Exit::Branch { yes, no, .. } => vec![yes, no],
            Exit::Jump(target) => vec![target],
            Exit::Next => vec![index + 1],
            Exit::Return(_) | Exit::ReturnVoid | Exit::End => vec![],
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

fn short_circuits(block: &mir::Block) -> usize {
    fn count(expression: &Expression) -> usize {
        match &expression.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Read(_)
            | ExpressionKind::Deref(_)
            | ExpressionKind::Borrow { .. }
            | ExpressionKind::Reborrow(_) => 0,
            ExpressionKind::Project { .. } => 0,
            ExpressionKind::Record { fields, .. } => {
                fields.iter().map(|(_, value)| count(value)).sum()
            }
            ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => count(inner),
            ExpressionKind::Binary(_, left, right) => count(left) + count(right),
            ExpressionKind::ShortCircuit { left, right, .. } => 1 + count(left) + count(right),
            ExpressionKind::Call(_, arguments) => arguments.iter().map(count).sum(),
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
            | Statement::CompoundProject { value, .. } => count(value),
            Statement::CallStatement(call) => count(call),
            Statement::RangeStart { start, finish, .. } => count(start) + count(finish),
            Statement::Marker | Statement::RangeBound { .. } | Statement::RangeStep { .. } => 0,
        })
        .sum();
    statements
        + match &block.exit.kind {
            mir::Exit::Branch { condition, .. } => count(condition),
            mir::Exit::Return(value) => count(value),
            _ => 0,
        }
}

struct Analysis<'a> {
    parameters: &'a [Param],
    locals: &'a [mir::LocalInfo],
    result: &'a Type,
    callees: &'a [Site],
    /// Record definitions, to look a field up by name.
    definitions: &'a HashMap<String, Type>,
    /// The first block built for each MIR block.
    start: Vec<usize>,
    types: Vec<Type>,
    operands: Vec<Value>,
    local_types: Vec<Option<Type>>,
    blocks: Vec<Block>,
    operations: Vec<Located<Operation>>,
}

impl Analysis<'_> {
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

    /// A type with a record's name replaced by its definition, so fields can be looked up.
    fn view(&self, ty: &Type) -> Type {
        match ty {
            Type::Named(name) | Type::RecordRef(name) => self
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
        path: &[Box<str>],
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
        for name in path {
            let Type::Record(_, fields) = &current else {
                return Err(Diagnostic::new(
                    "E116",
                    "unknown field or tuple index",
                    span,
                ));
            };
            let Some((index, (_, field))) = fields
                .iter()
                .enumerate()
                .find(|(_, (field, _))| field == name)
            else {
                return Err(Diagnostic::new(
                    "E116",
                    "unknown field or tuple index",
                    span,
                ));
            };
            let field = self.view(field);
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
        fields: &[(Box<str>, Expression)],
        span: Span,
    ) -> Result<(Value, Type), Diagnostic> {
        let (ty, declared) = self.record_type(site, span)?;
        let mut gathered: Vec<Option<Value>> = vec![None; declared.len()];
        for (name, value) in fields {
            let Some(index) = declared.iter().position(|(field, _)| field == name) else {
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
            ExpressionKind::Read(variable) => self
                .variable_type(*variable)
                .ok_or_else(|| Diagnostic::new("E114", "type is not known yet", expression.span)),
            ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => self.infer(inner),
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
            ExpressionKind::Record { site, .. } => {
                self.record_type(*site, expression.span).map(|(ty, _)| ty)
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
            ExpressionKind::Call(site, _) => {
                Ok(self.callee(*site, expression.span)?.result.clone())
            }
        })
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
        let expression = contextualize(expression, wanted)?;
        self.expression_as(&expression, usage)
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
                ExpressionKind::Read(variable) => {
                    let ty = self.typed(*variable, span)?;
                    if usage == Use::Move && !crate::ownership::copy(&ty) {
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
                    if usage == Use::Move && !crate::ownership::copy(&ty) {
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
                ExpressionKind::Record { site, fields } => self.record(*site, fields, span),
                ExpressionKind::Deref(variable) => {
                    let (reference, ty) = self.read(*variable, span)?;
                    let target = pointee(&ty, span)?;
                    if usage == Use::Move && !crate::ownership::copy(&target) {
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
                        numeric(&ty)
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
                ExpressionKind::Call(site, arguments) => {
                    let (target, result, operands) = self.arguments(*site, arguments, span)?;
                    let value = self.value(result.clone());
                    self.emit(Operation::Call(value, target, operands), span);
                    Ok((value, result))
                }
            }
        })
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
        for block in &body.blocks {
            for statement in &block.statements {
                let span = statement.span;
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
            if !changed {
                break;
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
                .map_or("", |local| &local.name);
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
                let ExpressionKind::Call(site, arguments) = &call.kind else {
                    return Err(invalid(span));
                };
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
        ExpressionKind::Literal(_) => false,
        ExpressionKind::Read(variable)
        | ExpressionKind::Deref(variable)
        | ExpressionKind::Reborrow(variable)
        | ExpressionKind::Project { root: variable, .. }
        | ExpressionKind::Borrow { variable, .. } => {
            variables.iter().any(|other| other.0 == variable.0)
        }
        ExpressionKind::Record { fields, .. } => {
            fields.iter().any(|(_, value)| mentions(value, variables))
        }
        ExpressionKind::Group(inner) | ExpressionKind::Unary(_, inner) => {
            mentions(inner, variables)
        }
        ExpressionKind::Binary(_, left, right)
        | ExpressionKind::ShortCircuit { left, right, .. } => {
            mentions(left, variables) || mentions(right, variables)
        }
        ExpressionKind::Call(_, arguments) => arguments
            .iter()
            .any(|argument| mentions(argument, variables)),
    }
}
