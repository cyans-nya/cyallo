//! Ownership and loan checking over resolved, typed source expressions.
//! Local aliases are allowed. Calls enforce aliasing XOR mutability; storage
//! growth additionally requires that no live view can be invalidated.
use crate::{inference::expression_type, intrinsic::Intrinsic, lookup};
use cyallo_ast::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Projection {
    Field(usize),
    Element(Option<i128>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    root: String,
    external: bool,
    projections: Vec<Projection>,
}
impl Place {
    fn overlaps(&self, other: &Self) -> bool {
        if self.root != other.root {
            return false;
        }
        for (a, b) in self.projections.iter().zip(&other.projections) {
            match (a, b) {
                (Projection::Field(a), Projection::Field(b)) if a != b => return false,
                (Projection::Element(Some(a)), Projection::Element(Some(b))) if a != b => {
                    return false;
                }
                _ => {}
            }
        }
        true
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Loan {
    place: Place,
    mutable: bool,
    view: bool,
    span: Span,
}
#[derive(Clone, Debug, Default)]
struct ValueInfo {
    plain_callable: bool,
    loans: Vec<Loan>,
}
impl ValueInfo {
    fn extend(&mut self, other: Self) {
        for loan in other.loans {
            if !self.loans.contains(&loan) {
                self.loans.push(loan);
            }
        }
    }
}
#[derive(Clone, Debug, Default)]
struct Binding {
    value: ValueInfo,
    moved: Option<Span>,
}
#[derive(Clone, Copy)]
enum Use {
    Read,
    Move,
}
type Result<T> = std::result::Result<T, Diagnostic>;
struct Checker<'a> {
    moves: HashSet<(u32, u32)>,
    types: HashMap<String, Type>,
    routines: &'a HashMap<String, Type>,
    bindings: HashMap<String, Binding>,
    globals: &'a [crate::ir::Local],
    last_use: HashMap<String, u32>,
}
pub fn copy(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Numeric(_)
            | Type::Integer
            | Type::Real
            | Type::Boolean
            | Type::Char
            | Type::Reference { .. }
            | Type::Symbol
            | Type::Void
            | Type::Storage(StorageKind::Layout, _)
            | Type::Managed {
                kind: MemoryKind::Gc,
                ..
            }
    )
}
fn carries_borrows(ty: &Type) -> bool {
    match ty {
        Type::Reference { .. } | Type::Borrowed(_) | Type::Any | Type::TraitObject(_) => true,
        Type::Managed {
            lifetime, pointee, ..
        } => lifetime.is_some() || carries_borrows(pointee),
        Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner)
        | Type::Storage(_, inner) => carries_borrows(inner),
        Type::Tuple(types) => types.iter().any(carries_borrows),
        Type::Record(_, fields) => fields.iter().any(|(_, ty)| carries_borrows(ty)),
        _ => false,
    }
}
pub fn effectful_lifecycle(ty: &Type) -> bool {
    match ty {
        Type::Reference { .. } | Type::Borrowed(_) => false,
        Type::Managed {
            kind: MemoryKind::Rc,
            ..
        } => true,
        Type::Managed { pointee, .. } => effectful_lifecycle(pointee),
        Type::Storage(
            StorageKind::Weak
            | StorageKind::ArenaCell
            | StorageKind::Allocation
            | StorageKind::Generator,
            _,
        ) => true,
        Type::Context | Type::Registration | Type::ServiceRef(_) | Type::TraitObject(_) => true,
        Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner)
        | Type::Storage(_, inner) => effectful_lifecycle(inner),
        Type::Tuple(types) => types.iter().any(effectful_lifecycle),
        Type::Record(_, fields) => fields.iter().any(|(_, ty)| effectful_lifecycle(ty)),
        _ => false,
    }
}
pub fn needs_drop(ty: &Type) -> bool {
    effectful_lifecycle(ty)
        || match ty {
            Type::Function(..) => true,
            Type::Storage(StorageKind::Generator | StorageKind::PureGenerator, _) => true,
            Type::Storage(_, inner) | Type::Array(_, inner) | Type::SparseArray(inner) => {
                needs_drop(inner)
            }
            Type::Tuple(ts) => ts.iter().any(needs_drop),
            Type::Record(_, fs) => fs.iter().any(|(_, t)| needs_drop(t)),
            _ => false,
        }
}
fn error(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new("E118", message, span)
}
pub fn check(
    body: &[Stmt],
    locals: &[crate::ir::Local],
    globals: &[crate::ir::Local],
    routines: &HashMap<String, Type>,
    params: &[Param],
    pure: bool,
    result: &Type,
) -> Result<HashSet<(u32, u32)>> {
    if pure {
        if let Some(local) = locals.iter().find(|local| effectful_lifecycle(&local.ty)) {
            return Err(error(
                "pure fn cannot own a value with effectful construction, cloning or destruction; pass an effectless reference",
                local.span,
            ));
        }
        if effectful_lifecycle(result) {
            return Err(error(
                "pure fn cannot return an owned value with effectful lifecycle",
                body.first().map_or(SPAN, |s| s.span),
            ));
        }
    }
    let types = globals
        .iter()
        .chain(locals)
        .map(|local| (local.name.to_string(), local.ty.clone()))
        .collect();
    let mut checker = Checker {
        moves: HashSet::new(),
        types,
        routines,
        bindings: HashMap::new(),
        globals,
        last_use: HashMap::new(),
    };
    let mut source = body.to_vec();
    crate::composites::visit_block(
        &mut source,
        &mut |expression| {
            if let ExprKind::Name(name) = &expression.kind {
                checker
                    .last_use
                    .entry(name.to_string())
                    .and_modify(|last| *last = (*last).max(expression.span.end))
                    .or_insert(expression.span.end);
            }
            Ok(())
        },
        &mut |_, _| Ok(()),
    )?;
    for param in params {
        if let Type::Reference {
            mutable, pointee, ..
        } = &param.ty
        {
            checker.bindings.insert(
                param.name.to_string(),
                Binding {
                    value: ValueInfo {
                        plain_callable: false,
                        loans: vec![Loan {
                            place: Place {
                                root: param.name.to_string(),
                                external: true,
                                projections: vec![],
                            },
                            mutable: *mutable,
                            view: matches!(pointee.as_ref(), Type::Slice(_)),
                            span: param.span,
                        }],
                    },
                    moved: None,
                },
            );
        }
    }
    checker.block(body)?;
    Ok(checker.moves)
}
impl Checker<'_> {
    fn ty(&self, expression: &Expr) -> Result<Type> {
        expression_type(expression, &self.types, self.routines)
    }
    fn available(&self, name: &str, span: Span) -> Result<()> {
        if let Some(moved) = self.bindings.get(name).and_then(|binding| binding.moved) {
            return Err(error("use of a moved value", span)
                .with_related(moved, "ownership transferred here"));
        }
        Ok(())
    }
    fn loan_live(&self, name: &str, at: u32) -> bool {
        self.last_use.get(name).is_some_and(|last| *last > at)
    }
    fn place(&mut self, expression: &Expr) -> Result<Place> {
        match &expression.kind {
            ExprKind::Invalid(diagnostic) => Err(diagnostic.clone()),
            ExprKind::Name(name) => {
                self.available(name, expression.span)?;
                if matches!(self.ty(expression)?, Type::Reference { .. })
                    && let Some(loan) = self
                        .bindings
                        .get(name.as_ref())
                        .and_then(|binding| binding.value.loans.first())
                {
                    return Ok(loan.place.clone());
                }
                Ok(Place {
                    root: name.to_string(),
                    external: self.globals.iter().any(|global| global.name == *name),
                    projections: vec![],
                })
            }
            ExprKind::Parenthesized(inner) | ExprKind::Unary(Operator::Dereference, inner) => {
                self.place(inner)
            }
            ExprKind::Field(base, name) | ExprKind::ExtensionField(base, name, _) => {
                let mut place = self.place(base)?;
                if let Type::Record(_, fields) = self.ty(base)?.pointee_or_self() {
                    let index = fields
                        .iter()
                        .position(|(field, _)| field == name)
                        .ok_or_else(|| error("unknown field", expression.span))?;
                    place.projections.push(Projection::Field(index));
                }
                Ok(place)
            }
            ExprKind::TupleField(base, index) => {
                let mut place = self.place(base)?;
                place.projections.push(Projection::Field(*index));
                Ok(place)
            }
            ExprKind::Index(base, indices) => {
                let mut place = self.place(base)?;
                for index in indices {
                    self.expression(index, Use::Read)?;
                    let value = match index.kind {
                        ExprKind::Literal(Literal::Integer(value)) => Some(value as i128),
                        ExprKind::Literal(Literal::Number(value)) => Some(value.int()),
                        _ => None,
                    };
                    place.projections.push(Projection::Element(value));
                }
                Ok(place)
            }
            ExprKind::Literal(_)
            | ExprKind::Frozen(..)
            | ExprKind::ArrayLiteral(_)
            | ExprKind::Record(..)
            | ExprKind::Tuple(_) => Ok(Place {
                root: format!(
                    "temporary:{}:{}",
                    expression.span.start, expression.span.end
                ),
                external: false,
                projections: vec![],
            }),
            _ => Err(error(
                "a borrow requires addressable storage",
                expression.span,
            )),
        }
    }
    fn boundary(&self, values: &[ValueInfo], span: Span) -> Result<()> {
        for (index, value) in values.iter().enumerate() {
            for other in &values[index + 1..] {
                for a in &value.loans {
                    for b in &other.loans {
                        if (a.mutable || b.mutable) && a.place.overlaps(&b.place) {
                            return Err(error(
                                "call arguments overlap and include a mutable reference",
                                span,
                            )
                            .with_related(a.span, "first loan")
                            .with_related(b.span, "overlapping loan"));
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn resize(&self, receiver: &Place, span: Span) -> Result<()> {
        for (name, binding) in &self.bindings {
            if self.loan_live(name, span.end) {
                for loan in &binding.value.loans {
                    if loan.view && loan.place.overlaps(receiver) {
                        return Err(error(
                            "collection growth or removal would invalidate a live borrowed view",
                            span,
                        )
                        .with_related(loan.span, "view remains in use after this operation"));
                    }
                }
            }
        }
        Ok(())
    }
    fn call(
        &mut self,
        arguments: &[Expr],
        params: &[(Type, bool)],
        span: Span,
    ) -> Result<ValueInfo> {
        let mut values = vec![];
        for (index, argument) in arguments.iter().enumerate() {
            let read = params
                .get(index)
                .is_some_and(|(ty, by_ref)| *by_ref || matches!(ty, Type::Reference { .. }));
            values.push(self.expression(argument, if read { Use::Read } else { Use::Move })?);
        }
        self.boundary(&values, span)?;
        let mut result = ValueInfo::default();
        for value in values {
            result.extend(value);
        }
        Ok(result)
    }
    fn expression(&mut self, expression: &Expr, usage: Use) -> Result<ValueInfo> {
        cyallo_ast::with_stack(|| {
            let mut moved_projection = None;
            if matches!(usage, Use::Move) && self.ty(expression).is_ok_and(|ty| !copy(&ty)) {
                let base = match &expression.kind {
                    ExprKind::Field(base, _)
                    | ExprKind::TupleField(base, _)
                    | ExprKind::Index(base, _)
                    | ExprKind::Unary(Operator::Dereference, base) => Some(base),
                    _ => None,
                };
                if let Some(base) = base {
                    if matches!(
                        self.ty(base)?,
                        Type::Reference { .. } | Type::Managed { .. }
                    ) {
                        return Err(error(
                            "cannot move an owned value out of borrowed or shared storage; clone it explicitly",
                            expression.span,
                        ));
                    }
                    if let Ok(place) = self.place(base) {
                        self.available(&place.root, expression.span)?;
                        moved_projection = Some(place.root);
                    }
                }
            }
            let mut result = ValueInfo::default();
            match &expression.kind {
                ExprKind::Yield(value, _) => {
                    result.extend(self.expression(value, Use::Move)?);
                }
                ExprKind::Invalid(diagnostic) => return Err(diagnostic.clone()),
                ExprKind::ManagedNew(_, allocator, value) => {
                    if let Some(allocator) = allocator {
                        result.extend(self.expression(allocator, Use::Move)?);
                    }
                    result.extend(self.expression(value, Use::Move)?);
                }
                ExprKind::Name(name) => {
                    self.available(name, expression.span)?;
                    result = self
                        .bindings
                        .get(name.as_ref())
                        .map(|binding| binding.value.clone())
                        .unwrap_or_default();
                    result.plain_callable |= !self.types.contains_key(name.as_ref())
                        && self.routines.contains_key(name.as_ref());
                    if matches!(usage, Use::Move)
                        && let Some(ty) = self.types.get(name.as_ref())
                        && !copy(ty)
                        && !result.plain_callable
                    {
                        let place = Place {
                            root: name.to_string(),
                            external: false,
                            projections: vec![],
                        };
                        for (alias, binding) in &self.bindings {
                            if self.loan_live(alias, expression.span.end)
                                && binding
                                    .value
                                    .loans
                                    .iter()
                                    .any(|loan| loan.place.overlaps(&place))
                            {
                                return Err(error(
                                    "cannot move storage while a borrow remains in use",
                                    expression.span,
                                ));
                            }
                        }
                        self.bindings.entry(name.to_string()).or_default().moved =
                            Some(expression.span);
                        self.moves
                            .insert((expression.span.start, expression.span.end));
                    }
                }
                ExprKind::Unary(operator @ (Operator::Borrow | Operator::BorrowMut), inner) => {
                    let place = self.place(inner)?;
                    let view = matches!(&inner.kind, ExprKind::Index(base,_) if matches!(self.ty(base)?.pointee_or_self(), Type::Storage(StorageKind::Vec,_) | Type::Slice(_)));
                    result.loans.push(Loan {
                        place,
                        mutable: *operator == Operator::BorrowMut,
                        view,
                        span: expression.span,
                    });
                }
                ExprKind::Parenthesized(inner) => return self.expression(inner, usage),
                ExprKind::Unary(Operator::Dereference, inner) => {
                    result = self.expression(inner, Use::Read)?;
                }
                ExprKind::Unary(_, inner) | ExprKind::Cast(inner, _) => {
                    result = self.expression(inner, usage)?;
                }
                ExprKind::Binary(_, a, b) => {
                    self.expression(a, Use::Read)?;
                    self.expression(b, Use::Read)?;
                }
                ExprKind::Field(base, _)
                | ExprKind::ExtensionField(base, _, _)
                | ExprKind::TupleField(base, _) => {
                    result = self.expression(base, Use::Read)?;
                }
                ExprKind::Index(base, indices) => {
                    result = self.expression(base, Use::Read)?;
                    for index in indices {
                        self.expression(index, Use::Read)?;
                    }
                }
                ExprKind::ArrayLiteral(values) | ExprKind::Tuple(values) => {
                    for value in values {
                        result.extend(self.expression(value, Use::Move)?);
                    }
                }
                ExprKind::Record(_, fields) | ExprKind::GenericRecord(_, _, fields) => {
                    for (_, value) in fields {
                        result.extend(self.expression(value, Use::Move)?);
                    }
                }
                ExprKind::IntrinsicCall(id, _, arguments) => {
                    let intrinsic = Intrinsic::from_id(*id)
                        .ok_or_else(|| error("unresolved intrinsic", expression.span))?;
                    if matches!(
                        intrinsic,
                        Intrinsic::RcClone
                            | Intrinsic::RcDowngrade
                            | Intrinsic::GcClone
                            | Intrinsic::CloneValue
                            | Intrinsic::FieldLocation
                    ) && let Some(argument) = arguments.first()
                    {
                        let place = match &argument.kind {
                            ExprKind::Unary(Operator::Borrow | Operator::BorrowMut, inner) => {
                                self.place(inner)?
                            }
                            _ => self.place(argument)?,
                        };
                        return Ok(self
                            .bindings
                            .get(&place.root)
                            .map(|binding| binding.value.clone())
                            .unwrap_or_default());
                    }
                    for (index, argument) in arguments.iter().enumerate() {
                        result.extend(self.expression(
                            argument,
                            if intrinsic.consumes_argument(index) {
                                Use::Move
                            } else {
                                Use::Read
                            },
                        )?);
                    }
                }
                ExprKind::Call(name, arguments) => {
                    self.available(name, expression.span)?;
                    let params = self
                        .routines
                        .get(name.as_ref())
                        .and_then(crate::composites::callable)
                        .map(|(_, params, _)| params.to_vec())
                        .unwrap_or_default();
                    result = self.call(arguments, &params, expression.span)?;
                }
                ExprKind::Invoke(callee, arguments) => {
                    if let ExprKind::Field(base, name) | ExprKind::ExtensionField(base, name, _) =
                        &callee.kind
                    {
                        let receiver = self.ty(base)?;
                        let parameters = lookup::parameter_types(&receiver, name);
                        let types = arguments
                            .iter()
                            .enumerate()
                            .map(|(index, argument)| {
                                let argument = parameters
                                    .as_ref()
                                    .and_then(|types| types.get(index))
                                    .map(|ty| crate::analysis::contextualize(argument, ty))
                                    .transpose()?
                                    .unwrap_or_else(|| argument.clone());
                                self.ty(&argument)
                            })
                            .collect::<Result<Vec<_>>>()?;
                        if let Some(method) = lookup::method(
                            &receiver,
                            name,
                            &types,
                            arguments.first().and_then(cyallo_ast::constant_text),
                        ) {
                            let mut values = vec![self.expression(
                                base,
                                if method.receiver == lookup::ReceiverMode::Move {
                                    Use::Move
                                } else {
                                    Use::Read
                                },
                            )?];
                            for argument in arguments {
                                values.push(self.expression(argument, Use::Move)?);
                            }
                            if matches!(
                                method.operation,
                                lookup::Operation::CollectionPush(_)
                                    | lookup::Operation::CollectionPop(_)
                            ) {
                                let place = self.place(base)?;
                                self.resize(&place, expression.span)?;
                            }
                            self.boundary(&values, expression.span)?;
                            if let lookup::Operation::Slice(mutable) = method.operation {
                                result.loans.push(Loan {
                                    place: self.place(base)?,
                                    mutable,
                                    view: true,
                                    span: expression.span,
                                });
                            } else {
                                for value in values {
                                    result.extend(value);
                                }
                            }
                        } else {
                            self.expression(base, Use::Read)?;
                            for argument in arguments {
                                result.extend(self.expression(argument, Use::Move)?);
                            }
                        }
                    } else {
                        let callee_info = self.expression(callee, Use::Read)?;
                        let params = self
                            .ty(callee)
                            .ok()
                            .and_then(|ty| {
                                crate::composites::callable(&ty)
                                    .map(|(_, params, _)| params.to_vec())
                            })
                            .unwrap_or_default();
                        result = self.call(arguments, &params, expression.span)?;
                        self.boundary(&[callee_info, result.clone()], expression.span)?;
                    }
                }
                ExprKind::ClosureValue(_, captures, _) => {
                    result.plain_callable = captures.is_empty();
                    for capture in captures {
                        result.extend(self.expression(capture, Use::Move)?);
                    }
                }
                ExprKind::Closure(_, _) | ExprKind::Frozen(_, _) | ExprKind::Literal(_) => {}
                ExprKind::TypedCollection(_, _, arguments)
                | ExprKind::GenericCall(_, _, arguments) => {
                    for argument in arguments {
                        result.extend(self.expression(argument, Use::Move)?);
                    }
                }
            }
            if let Some(root) = moved_projection {
                self.bindings.entry(root).or_default().moved = Some(expression.span);
                self.moves
                    .insert((expression.span.start, expression.span.end));
            }
            // Results carry only their own lifetime dependencies.
            if self.ty(expression).is_ok_and(|ty| !carries_borrows(&ty)) {
                result.loans.clear();
            }
            Ok(result)
        })
    }
    fn block(&mut self, body: &[Stmt]) -> Result<()> {
        cyallo_ast::with_stack(|| {
            let mut declared = vec![];
            for statement in body {
                match &statement.kind {
                    StmtKind::Binding(name, _, _, _, value) | StmtKind::Constant(name, value) => {
                        let mut value = self.expression(value, Use::Move)?;
                        if matches!(self.types.get(name.as_ref()), Some(Type::Reference { pointee, .. }) if matches!(pointee.as_ref(), Type::Slice(_)))
                        {
                            for loan in &mut value.loans {
                                loan.view = true;
                            }
                        }
                        self.bindings
                            .insert(name.to_string(), Binding { value, moved: None });
                        declared.push(name.to_string());
                    }
                    StmtKind::Assign(target, value) => {
                        let value = self.expression(value, Use::Move)?;
                        if let ExprKind::Name(name) = &target.kind {
                            self.bindings
                                .insert(name.to_string(), Binding { value, moved: None });
                        } else {
                            self.place(target)?;
                        }
                    }
                    StmtKind::Compound(target, _, value) => {
                        self.place(target)?;
                        self.expression(value, Use::Read)?;
                    }
                    StmtKind::Evaluate(value) => {
                        self.expression(value, Use::Read)?;
                    }
                    StmtKind::Return(value) => {
                        let value = self.expression(value, Use::Move)?;
                        if let Some(loan) = value.loans.iter().find(|loan| !loan.place.external) {
                            return Err(error(
                                "returned value borrows storage owned by this routine",
                                statement.span,
                            )
                            .with_related(loan.span, "borrow created here"));
                        }
                    }
                    StmtKind::Output(values) | StmtKind::Call(_, values) => {
                        for value in values {
                            self.expression(value, Use::Read)?;
                        }
                    }
                    StmtKind::Input(value) => {
                        self.place(value)?;
                    }
                    StmtKind::If(condition, yes, no) => {
                        self.expression(condition, Use::Read)?;
                        let before = self.bindings.clone();
                        self.block(yes)?;
                        let a = self.bindings.clone();
                        self.bindings = before;
                        self.block(no)?;
                        for (name, state) in a {
                            let current = self.bindings.entry(name).or_default();
                            current.value.extend(state.value);
                            current.moved = current.moved.or(state.moved);
                        }
                    }
                    StmtKind::While(condition, body) | StmtKind::Repeat(body, condition) => {
                        self.expression(condition, Use::Read)?;
                        self.block(body)?;
                    }
                    StmtKind::For(_, first, last, step, body) => {
                        self.expression(first, Use::Read)?;
                        self.expression(last, Use::Read)?;
                        if let Some(step) = step {
                            self.expression(step, Use::Read)?;
                        }
                        self.block(body)?;
                    }
                    StmtKind::ForIn(_, iterable, body) => {
                        self.expression(iterable, Use::Read)?;
                        self.block(body)?;
                    }
                    StmtKind::Case(_, _, _)
                    | StmtKind::Declare(_, _)
                    | StmtKind::Break
                    | StmtKind::Continue
                    | StmtKind::ReturnVoid => {}
                }
            }
            let end = body.last().map_or(0, |statement| statement.span.end);
            for (name, binding) in &self.bindings {
                if !declared.contains(name)
                    && self.loan_live(name, end)
                    && let Some(loan) = binding
                        .value
                        .loans
                        .iter()
                        .find(|loan| declared.contains(&loan.place.root))
                {
                    return Err(error("borrow outlives its lexical storage", loan.span));
                }
            }
            Ok(())
        })
    }
}
