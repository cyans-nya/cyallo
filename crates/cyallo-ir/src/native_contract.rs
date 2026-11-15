//! Instantiation of trusted source contracts before native operation selection.
//!
//! Declaration IDs select headers. Source names are used only for ordinary
//! member lookup; no public path or member spelling identifies an operation.

use crate::{
    declarations::TypeEnvironment,
    internal::{self, InternalSemantic, MemberIntrinsic, NativeTrait, NativeTypeFamily},
    intrinsic::Intrinsic,
    lookup::ReceiverMode,
};
use cyallo_ast::*;
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraitObligationKind {
    Allocator,
    UserTrait,
}

/// Evidence that a later trait resolver must supply before native emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitObligation {
    pub ty: Type,
    pub bound: Type,
    pub kind: TraitObligationKind,
}

#[derive(Debug, Clone)]
pub struct InstantiatedFunction {
    pub declaration: DeclarationId,
    pub semantic: InternalSemantic,
    pub kind: RoutineKind,
    pub params: Vec<Param>,
    pub result: Type,
    pub obligations: Vec<TraitObligation>,
}

#[derive(Debug, Clone)]
pub struct InstantiatedMember {
    /// The receiver is excluded from this expanded parameter list.
    pub callable: InstantiatedFunction,
    pub receiver: ReceiverMode,
    /// Concrete receiver matched to the source owner, after optional GC deref.
    pub receiver_type: Type,
}

/// A checked specialization of a native descriptor into an ordinary callable.
/// Type arguments use source declaration order, including expanded type packs.
#[derive(Debug, Clone)]
pub struct CallableCoercion {
    pub contract: InstantiatedFunction,
    pub type_args: Vec<Type>,
    pub callable_type: Type,
}

/// Reads the instantiated source ownership contract, without consulting an ABI
/// identity. None denotes an invalid operand index rather than a default mode.
pub fn source_consumes_argument(call: &InstantiatedFunction, index: usize) -> Option<bool> {
    call.params
        .get(index)
        .map(|parameter| parameter.passing == ParameterPassing::Value)
}

#[derive(Debug, Clone)]
pub enum NativeContractError {
    UnknownDeclaration(DeclarationId),
    InvalidHeader,
    InvalidAnnotation,
    ArgumentCount {
        minimum: usize,
        maximum: Option<usize>,
        actual: usize,
    },
    TypeArgumentCount {
        maximum: usize,
        actual: usize,
    },
    ArgumentType {
        index: usize,
        expected: Type,
        actual: Type,
    },
    UninferredParameter(Box<str>),
    InvalidPack,
    InvalidPassing {
        index: usize,
    },
    UnsatisfiedBound {
        ty: Type,
        bound: Type,
    },
    InvalidBound(Type),
    UnknownFamily(DeclarationId),
    FamilyArity {
        family: NativeTypeFamily,
        expected: usize,
        actual: usize,
    },
    InvalidFamilyOperand {
        family: NativeTypeFamily,
        operand: Type,
    },
    ValueParameterMissing(usize),
    ConstantRangeRequired,
    ArrayLengthOverflow,
    ConstantFieldRequired,
    UnknownField {
        owner: Type,
        field: Box<str>,
    },
    ReceiverRequiresOwnership(Type),
    AmbiguousMember {
        receiver: Type,
        name: Box<str>,
    },
    ContextualLiteral(Diagnostic),
    CallableRequired(Type),
    CallableEffect {
        expected: RoutineKind,
        actual: RoutineKind,
    },
    CallableParameterMode {
        index: usize,
    },
    UnresolvedCallableInput {
        index: usize,
        ty: Type,
    },
}

impl fmt::Display for NativeContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDeclaration(id) => {
                write!(f, "unknown native callable declaration {}", id.0)
            }
            Self::InvalidHeader => {
                f.write_str("native callable requires a valid bodyless source contract")
            }
            Self::InvalidAnnotation => {
                f.write_str("native callable has no valid semantic identity")
            }
            Self::ArgumentCount {
                minimum,
                maximum: Some(maximum),
                actual,
            } if minimum == maximum => write!(
                f,
                "native declaration expects {minimum} arguments, found {actual}"
            ),
            Self::ArgumentCount {
                minimum,
                maximum: Some(maximum),
                actual,
            } => write!(
                f,
                "native declaration expects {minimum} to {maximum} arguments, found {actual}"
            ),
            Self::ArgumentCount {
                minimum,
                maximum: None,
                actual,
            } => write!(
                f,
                "native declaration expects at least {minimum} arguments, found {actual}"
            ),
            Self::TypeArgumentCount { maximum, actual } => write!(
                f,
                "native declaration accepts at most {maximum} explicit type arguments, found {actual}"
            ),
            Self::ArgumentType {
                index,
                expected,
                actual,
            } => write!(
                f,
                "argument {} requires `{expected}`, found `{actual}`",
                index + 1
            ),
            Self::UninferredParameter(parameter) => {
                write!(f, "cannot infer native type parameter `{parameter}`")
            }
            Self::InvalidPack => f.write_str(
                "native type packs require one final variadic parameter and one final type pack",
            ),
            Self::InvalidPassing { index } => write!(
                f,
                "argument {} combines read descriptor passing with legacy by-reference passing",
                index + 1
            ),
            Self::UnsatisfiedBound { ty, bound } => {
                write!(f, "`{ty}` does not satisfy declared bound `{bound}`")
            }
            Self::InvalidBound(bound) => write!(f, "invalid native generic bound `{bound}`"),
            Self::UnknownFamily(id) => write!(f, "unknown native type-family declaration {}", id.0),
            Self::FamilyArity {
                family,
                expected,
                actual,
            } => write!(
                f,
                "{family} type family expects {expected} operands, found {actual}"
            ),
            Self::InvalidFamilyOperand { family, operand } => {
                write!(f, "{family} type family cannot project `{operand}`")
            }
            Self::ValueParameterMissing(index) => write!(
                f,
                "native contract references missing value parameter {index}"
            ),
            Self::ConstantRangeRequired => f.write_str(
                "fixed array construction requires a range with compile-time integer bounds",
            ),
            Self::ArrayLengthOverflow => {
                f.write_str("fixed array length exceeds its supported range")
            }
            Self::ConstantFieldRequired => {
                f.write_str("field projection requires a compile-time string field name")
            }
            Self::UnknownField { owner, field } => {
                write!(f, "`{owner}` has no declared field `{field}`")
            }
            Self::ReceiverRequiresOwnership(ty) => write!(
                f,
                "member requires ownership of `{ty}`; it cannot move out of a borrowed receiver"
            ),
            Self::AmbiguousMember { receiver, name } => {
                write!(f, "ambiguous native member `{name}` on `{receiver}`")
            }
            Self::ContextualLiteral(error) => error.fmt(f),
            Self::CallableRequired(ty) => write!(
                f,
                "native function conversion requires a function or procedure signature, found `{ty}`"
            ),
            Self::CallableEffect { expected, actual } => write!(
                f,
                "native {actual} cannot satisfy {expected} callable effects"
            ),
            Self::CallableParameterMode { index } => write!(
                f,
                "native callable parameter {} has a different BYREF mode",
                index + 1
            ),
            Self::UnresolvedCallableInput { index, ty } => write!(
                f,
                "cannot infer native callable input {} from unresolved type `{ty}`",
                index + 1
            ),
        }
    }
}
impl std::error::Error for NativeContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ContextualLiteral(error) => Some(error),
            _ => None,
        }
    }
}

/// Converts a typed contract failure at the diagnostic boundary.
pub fn invalid_native_contract(error: NativeContractError, span: Span) -> Diagnostic {
    match error {
        NativeContractError::ContextualLiteral(error) => error,
        error => Diagnostic::new("E177", error.to_string(), span),
    }
}

type Result<T> = std::result::Result<T, NativeContractError>;

#[derive(Default)]
struct Bindings {
    parameters: HashSet<Box<str>>,
    values: HashMap<Box<str>, Type>,
    packs: HashMap<Box<str>, Vec<Type>>,
    obligations: Vec<TraitObligation>,
    owner: Option<(Type, Type)>,
}

impl Bindings {
    fn obligation(&mut self, ty: Type, bound: Type, kind: TraitObligationKind) {
        let obligation = TraitObligation { ty, bound, kind };
        if !self.obligations.contains(&obligation) {
            self.obligations.push(obligation);
        }
    }

    fn unify(
        &mut self,
        env: &TypeEnvironment,
        pattern: &Type,
        actual: &Type,
        index: usize,
    ) -> Result<()> {
        if let Type::Named(name) = pattern
            && self.parameters.contains(name)
        {
            if let Some(existing) = self.values.get(name).cloned() {
                return self.unify(env, &existing, actual, index);
            }
            self.values.insert(name.clone(), normalize(actual));
            return Ok(());
        }
        let actual = view(env, actual);
        let mismatch = || NativeContractError::ArgumentType {
            index,
            expected: pattern.clone(),
            actual: actual.clone(),
        };
        if equivalent(pattern, &actual) {
            return Ok(());
        }
        if *pattern == Type::Dynamic && crate::dynamic::owned(&actual) {
            return Ok(());
        }
        match (pattern, &actual) {
            (Type::Function(..), Type::NativeCallable(declaration)) => {
                let coercion = as_callable(env, *declaration, pattern, Span::default())
                    .map_err(NativeContractError::ContextualLiteral)?;
                self.unify(env, pattern, &coercion.callable_type, index)
            }
            (
                Type::Reference {
                    mutable: required,
                    pointee: p,
                    ..
                },
                Type::Reference {
                    mutable: supplied,
                    pointee: a,
                    ..
                },
            ) if !required || *supplied => {
                if let Type::Slice(element) = p.as_ref() {
                    match a.as_ref() {
                        Type::Array(_, actual) | Type::Storage(StorageKind::Vec, actual) => {
                            return self.unify(env, element, actual, index);
                        }
                        _ => {}
                    }
                }
                self.unify(env, p, a, index)
            }
            (
                Type::Managed {
                    kind: p_kind,
                    locality: p_locality,
                    lifetime: p_life,
                    pointee: p,
                },
                Type::Managed {
                    kind: a_kind,
                    locality: a_locality,
                    lifetime: a_life,
                    pointee: a,
                },
            ) if p_kind == a_kind
                && p_locality == a_locality
                && p_life.is_some() == a_life.is_some() =>
            {
                self.unify(env, p, a, index)
            }
            (Type::Storage(p_kind, p), Type::Storage(a_kind, a))
                if p_kind == a_kind
                    || (*p_kind == StorageKind::Generator
                        && *a_kind == StorageKind::PureGenerator) =>
            {
                self.unify(env, p, a, index)
            }
            (Type::Array(p_bounds, p), Type::Array(a_bounds, a)) if p_bounds == a_bounds => {
                self.unify(env, p, a, index)
            }
            (Type::SparseArray(p), Type::SparseArray(a))
            | (Type::Slice(p), Type::Slice(a))
            | (Type::ServiceKey(p), Type::ServiceKey(a))
            | (Type::ServiceRef(p), Type::ServiceRef(a))
            | (Type::Borrowed(p), Type::Borrowed(a)) => self.unify(env, p, a, index),
            (
                Type::Function(p_kind, p_params, p_result),
                Type::Function(a_kind, a_params, a_result),
            ) if (p_kind == a_kind
                || (*p_kind == RoutineKind::Procedure && *a_kind == RoutineKind::Function))
                && p_params.len() == a_params.len() =>
            {
                for ((p, p_ref), (a, a_ref)) in p_params.iter().zip(a_params) {
                    if p_ref != a_ref {
                        return Err(mismatch());
                    }
                    self.unify(env, p, a, index)?;
                }
                self.unify(env, p_result, a_result, index)
            }
            (Type::Function(..), Type::Borrowed(inner)) => self.unify(env, pattern, inner, index),
            (Type::Tuple(ps), Type::Tuple(ts)) if ps.len() == ts.len() => {
                for (p, a) in ps.iter().zip(ts) {
                    self.unify(env, p, a, index)?;
                }
                Ok(())
            }
            (Type::TraitObject(name), _) if !matches!(actual, Type::TraitObject(_)) => {
                self.obligation(
                    actual.clone(),
                    Type::TraitObject(name.clone()),
                    TraitObligationKind::UserTrait,
                );
                Ok(())
            }
            (Type::Named(name), Type::Record(actual_name, _) | Type::RecordRef(actual_name))
                if name == actual_name =>
            {
                Ok(())
            }
            _ => Err(mismatch()),
        }
    }

    fn substitute(
        &self,
        env: &TypeEnvironment,
        ty: &Type,
        expressions: &[Expr],
        actual: &[Type],
    ) -> Result<Type> {
        if let Some((pattern, owner)) = &self.owner
            && ty == pattern
        {
            return Ok(owner.clone());
        }
        Ok(match ty {
            Type::Named(name) => self.values.get(name).cloned().unwrap_or_else(|| ty.clone()),
            Type::ValueParameter(index) => actual
                .get(*index)
                .cloned()
                .ok_or(NativeContractError::ValueParameterMissing(*index))?,
            Type::Dependent(id, operands) => {
                let operands = operands
                    .iter()
                    .map(|operand| {
                        if matches!(operand, Type::ValueParameter(_)) {
                            Ok(operand.clone())
                        } else {
                            self.substitute(env, operand, expressions, actual)
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                family(env, *id, &operands, expressions, actual)?
            }
            Type::Numeric(kind) => Type::numeric(*kind),
            Type::Reference {
                mutable,
                lifetime,
                pointee,
            } => Type::Reference {
                mutable: *mutable,
                lifetime: lifetime.clone(),
                pointee: Box::new(self.substitute(env, pointee, expressions, actual)?),
            },
            Type::Managed {
                kind,
                locality,
                lifetime,
                pointee,
            } => Type::Managed {
                kind: *kind,
                locality: *locality,
                lifetime: lifetime.clone(),
                pointee: Box::new(self.substitute(env, pointee, expressions, actual)?),
            },
            Type::Storage(kind, inner) => Type::Storage(
                *kind,
                Box::new(self.substitute(env, inner, expressions, actual)?),
            ),
            Type::Array(bounds, inner) => Type::Array(
                bounds.clone(),
                Box::new(self.substitute(env, inner, expressions, actual)?),
            ),
            Type::SparseArray(inner) => Type::SparseArray(Box::new(self.substitute(
                env,
                inner,
                expressions,
                actual,
            )?)),
            Type::Slice(inner) => Type::Slice(Box::new(self.substitute(
                env,
                inner,
                expressions,
                actual,
            )?)),
            Type::Borrowed(inner) => Type::Borrowed(Box::new(self.substitute(
                env,
                inner,
                expressions,
                actual,
            )?)),
            Type::ServiceKey(inner) => Type::ServiceKey(Box::new(self.substitute(
                env,
                inner,
                expressions,
                actual,
            )?)),
            Type::ServiceRef(inner) => Type::ServiceRef(Box::new(self.substitute(
                env,
                inner,
                expressions,
                actual,
            )?)),
            Type::Tuple(types) if types.is_empty() => Type::Void,
            Type::Tuple(types) => Type::Tuple(
                types
                    .iter()
                    .map(|ty| self.substitute(env, ty, expressions, actual))
                    .collect::<Result<_>>()?,
            ),
            Type::Applied(name, types) => Type::Applied(
                name.clone(),
                types
                    .iter()
                    .map(|ty| self.substitute(env, ty, expressions, actual))
                    .collect::<Result<_>>()?,
            ),
            Type::Function(kind, params, result) => Type::Function(
                *kind,
                params
                    .iter()
                    .map(|(ty, by_ref)| {
                        Ok((self.substitute(env, ty, expressions, actual)?, *by_ref))
                    })
                    .collect::<Result<_>>()?,
                Box::new(self.substitute(env, result, expressions, actual)?),
            ),
            Type::Record(name, fields) => Type::Record(
                name.clone(),
                fields
                    .iter()
                    .map(|(name, ty)| {
                        Ok((name.clone(), self.substitute(env, ty, expressions, actual)?))
                    })
                    .collect::<Result<_>>()?,
            ),
            _ => ty.clone(),
        })
    }
}

fn normalize(ty: &Type) -> Type {
    match ty {
        Type::Numeric(kind) => Type::numeric(*kind),
        Type::Tuple(types) if types.is_empty() => Type::Void,
        _ => ty.clone(),
    }
}
fn equivalent(a: &Type, b: &Type) -> bool {
    crate::declarations::equivalent_types(a, b)
}
fn view(env: &TypeEnvironment, ty: &Type) -> Type {
    match ty {
        Type::RecordRef(name) => env
            .get(name.as_ref())
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Named(name) => env
            .get(name.as_ref())
            .filter(|ty| matches!(ty, Type::Record(..) | Type::RecordRef(_)))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        _ => ty.clone(),
    }
}
fn semantic(function: &Function) -> Result<InternalSemantic> {
    if function.annotations.len() != 1 || function.annotations[0].name.as_ref() != "cyallo_internal"
    {
        return Err(NativeContractError::InvalidAnnotation);
    }
    match internal::from_key(&function.annotations[0].value) {
        Some(semantic @ (InternalSemantic::Function(_) | InternalSemantic::Member(_))) => {
            Ok(semantic)
        }
        _ => Err(NativeContractError::InvalidAnnotation),
    }
}

fn collection_element(ty: &Type, numeric_only: bool) -> Option<Type> {
    match ty {
        Type::Array(_, element)
        | Type::Storage(StorageKind::Vec | StorageKind::VecDeque, element) => {
            (!numeric_only || element.number_kind().is_some()).then(|| *element.clone())
        }
        Type::Reference { pointee, .. } | Type::Borrowed(pointee) if !numeric_only => {
            collection_element(pointee, false)
        }
        Type::Slice(element) if !numeric_only => Some(*element.clone()),
        _ => None,
    }
}
fn transferable(env: &TypeEnvironment, ty: &Type, active: &mut HashSet<Box<str>>) -> bool {
    match ty {
        Type::RecordRef(name) => {
            if !active.insert(name.clone()) {
                return false;
            }
            let result = env
                .get(name.as_ref())
                .is_some_and(|ty| transferable(env, ty, active));
            active.remove(name);
            result
        }
        Type::Record(_, fields) => fields.iter().all(|(_, ty)| transferable(env, ty, active)),
        Type::Array(_, inner) | Type::SparseArray(inner) => transferable(env, inner, active),
        Type::Tuple(types) => types.iter().all(|ty| transferable(env, ty, active)),
        Type::Storage(
            StorageKind::Vec
            | StorageKind::VecDeque
            | StorageKind::Option
            | StorageKind::Result
            | StorageKind::SendError,
            inner,
        ) => transferable(env, inner, active),
        _ => crate::concurrent::transferable(ty),
    }
}
fn cloneable(env: &TypeEnvironment, ty: &Type, active: &mut HashSet<Box<str>>) -> bool {
    if !crate::memory::cloneable(ty) {
        return false;
    }
    match ty {
        Type::RecordRef(name) => {
            if !active.insert(name.clone()) {
                return false;
            }
            let result = env
                .get(name.as_ref())
                .is_some_and(|ty| cloneable(env, ty, active));
            active.remove(name);
            result
        }
        Type::Record(_, fields) => fields.iter().all(|(_, ty)| cloneable(env, ty, active)),
        Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner)
        | Type::Storage(_, inner) => cloneable(env, inner, active),
        Type::Tuple(types) => types.iter().all(|ty| cloneable(env, ty, active)),
        _ => crate::memory::cloneable(ty),
    }
}
fn predicate(env: &TypeEnvironment, predicate: NativeTrait, ty: &Type) -> bool {
    match predicate {
        NativeTrait::Numeric => ty.number_kind().is_some(),
        NativeTrait::Integer => ty.number_kind().is_some_and(|kind| !kind.float()),
        NativeTrait::Floating => ty.number_kind().is_some_and(NumberKind::float),
        NativeTrait::Printable => {
            if matches!(ty, Type::Dynamic | Type::Object(_)) {
                return true;
            }
            ty.number_kind().is_some() || matches!(ty, Type::String | Type::Char | Type::Boolean)
        }
        NativeTrait::Transferable => transferable(env, ty, &mut HashSet::new()),
        NativeTrait::Collection => collection_element(ty, false).is_some(),
        NativeTrait::NumericCollection => collection_element(ty, true).is_some(),
        NativeTrait::Cloneable => cloneable(env, ty, &mut HashSet::new()),
        NativeTrait::RcOwner => matches!(
            ty,
            Type::Managed {
                kind: MemoryKind::Rc,
                ..
            }
        ),
        NativeTrait::WeakOwner => {
            matches!(ty, Type::Storage(StorageKind::Weak, owner) if matches!(owner.as_ref(), Type::Managed {kind: MemoryKind::Rc, ..}))
        }
        NativeTrait::GcOwner => matches!(
            ty,
            Type::Managed {
                kind: MemoryKind::Gc,
                ..
            }
        ),
        NativeTrait::GcView => {
            matches!(
                ty,
                Type::Managed {
                    kind: MemoryKind::Gc,
                    ..
                }
            ) || matches!(ty, Type::Reference {pointee, ..} if matches!(pointee.as_ref(), Type::Managed {kind: MemoryKind::Gc, ..}))
        }
        NativeTrait::Lengthable => {
            matches!(ty, Type::String | Type::Array(..) | Type::SparseArray(_))
                || matches!(ty, Type::Reference {pointee, ..} if matches!(pointee.as_ref(), Type::Slice(_) | Type::Array(..)))
        }
        NativeTrait::ObjectView => matches!(ty, Type::Object(_)) && crate::dynamic::owned(ty),
        NativeTrait::ObjectLike => crate::dynamic::object(ty.pointee_or_self()),
        NativeTrait::LegacyNumeric => {
            matches!(ty.number_kind(), Some(NumberKind::I64 | NumberKind::F64))
        }
    }
}

fn bounds(
    env: &TypeEnvironment,
    function: &Function,
    bindings: &mut Bindings,
    expressions: &[Expr],
    actual: &[Type],
    complete: bool,
) -> Result<()> {
    // Collection evidence may infer another parameter (Collection<T> binds T).
    for _ in 0..=function.type_params.len() {
        let before = bindings.values.len();
        for constraint in &function.constraints {
            let values = if let Some(pack) = bindings.packs.get(&constraint.parameter) {
                pack.clone()
            } else if let Some(ty) = bindings.values.get(&constraint.parameter) {
                vec![ty.clone()]
            } else {
                continue;
            };
            for ty in values {
                for bound in &constraint.bounds {
                    let (name, arguments) = match bound {
                        Type::Named(name) | Type::TraitObject(name) => (name, &[][..]),
                        Type::Applied(name, arguments) => (name, arguments.as_slice()),
                        _ => return Err(NativeContractError::InvalidBound(bound.clone())),
                    };
                    let declared = env
                        .native
                        .traits
                        .iter()
                        .find(|(_, declaration)| declaration.name == *name);
                    let bound_semantic = declared
                        .and_then(|(_, declaration)| declaration.annotations.first())
                        .and_then(|annotation| internal::from_key(&annotation.value));
                    match bound_semantic {
                        Some(InternalSemantic::Trait(native)) => {
                            if arguments.len() != crate::declarations::trait_arity(native) {
                                return Err(NativeContractError::InvalidBound(bound.clone()));
                            }
                            if !predicate(env, native, &ty) {
                                return Err(NativeContractError::UnsatisfiedBound {
                                    ty: ty.clone(),
                                    bound: bound.clone(),
                                });
                            }
                            if matches!(
                                native,
                                NativeTrait::Collection | NativeTrait::NumericCollection
                            ) {
                                let element = collection_element(
                                    &ty,
                                    native == NativeTrait::NumericCollection,
                                )
                                .expect("proved collection bound");
                                bindings.unify(env, &arguments[0], &element, 0)?;
                            }
                        }
                        Some(InternalSemantic::AllocatorTrait) => {
                            if !arguments.is_empty() {
                                return Err(NativeContractError::InvalidBound(bound.clone()));
                            }
                            let bound = bindings.substitute(env, bound, expressions, actual)?;
                            bindings.obligation(ty.clone(), bound, TraitObligationKind::Allocator);
                        }
                        _ => {
                            let bound = bindings.substitute(env, bound, expressions, actual)?;
                            bindings.obligation(ty.clone(), bound, TraitObligationKind::UserTrait);
                        }
                    }
                }
            }
        }
        if bindings.values.len() == before {
            break;
        }
    }
    if complete {
        for parameter in &bindings.parameters {
            if !bindings.values.contains_key(parameter) && !bindings.packs.contains_key(parameter) {
                return Err(NativeContractError::UninferredParameter(parameter.clone()));
            }
        }
    }
    Ok(())
}

fn prepare(
    env: &TypeEnvironment,
    function: &Function,
    type_args: &[Type],
    expressions: &[Expr],
    actual: &[Type],
    mut bindings: Bindings,
    complete: bool,
) -> Result<(Vec<Param>, Bindings)> {
    if function.body_kind != FunctionBodyKind::Declaration || !function.body.is_empty() {
        return Err(NativeContractError::InvalidHeader);
    }
    if let Some(index) = function
        .params
        .iter()
        .position(|parameter| parameter.passing == ParameterPassing::Read && parameter.by_ref)
    {
        return Err(NativeContractError::InvalidPassing { index });
    }
    if expressions.len() != actual.len() {
        return Err(NativeContractError::InvalidHeader);
    }
    bindings
        .parameters
        .extend(function.type_params.iter().cloned());
    if function.type_params.iter().collect::<HashSet<_>>().len() != function.type_params.len()
        || function
            .constraints
            .iter()
            .any(|constraint| !bindings.parameters.contains(&constraint.parameter))
    {
        return Err(NativeContractError::InvalidHeader);
    }
    let variadic = function
        .params
        .iter()
        .position(|param| param.arity == ParameterArity::Variadic);
    let pack = function.type_packs.first();
    if function.type_packs.len() > 1
        || match (variadic, pack) {
            (Some(index), Some(pack)) => {
                index + 1 != function.params.len()
                    || function.type_params.last() != Some(pack)
                    || function.params[index].ty != Type::Named(pack.clone())
            }
            (None, None) => false,
            _ => true,
        }
    {
        return Err(NativeContractError::InvalidPack);
    }
    let minimum = variadic.unwrap_or(function.params.len());
    if actual.len() < minimum || (variadic.is_none() && actual.len() != minimum) {
        return Err(NativeContractError::ArgumentCount {
            minimum,
            maximum: variadic.is_none().then_some(minimum),
            actual: actual.len(),
        });
    }
    let fixed_types = function.type_params.len() - usize::from(pack.is_some());
    if pack.is_none() && type_args.len() > fixed_types {
        return Err(NativeContractError::TypeArgumentCount {
            maximum: fixed_types,
            actual: type_args.len(),
        });
    }
    for (parameter, ty) in function.type_params[..fixed_types].iter().zip(type_args) {
        bindings.values.insert(parameter.clone(), normalize(ty));
    }
    if let Some(pack) = pack {
        let count = actual.len() - minimum;
        let types = if type_args.len() > fixed_types {
            let types = type_args[fixed_types..].to_vec();
            if types.len() != count {
                return Err(NativeContractError::InvalidPack);
            }
            types
        } else {
            actual[minimum..].iter().map(normalize).collect()
        };
        bindings.packs.insert(pack.clone(), types);
    }
    let mut expanded = function.params[..minimum].to_vec();
    if let (Some(index), Some(pack)) = (variadic, pack) {
        for (offset, ty) in bindings.packs[pack].iter().enumerate() {
            let mut parameter = function.params[index].clone();
            parameter.arity = ParameterArity::One;
            parameter.name = format!("{}${offset}", parameter.name).into();
            parameter.ty = ty.clone();
            expanded.push(parameter);
        }
    }
    // Unsuffixed numbers supply defaults only after typed operands bind shared T.
    let mut order: Vec<_> = (0..actual.len()).collect();
    order.sort_by_key(|index| {
        (
            matches!(actual[*index], Type::NativeCallable(_)),
            crate::analysis::contextual_number(&expressions[*index]),
            actual[*index] != Type::Real,
        )
    });
    for index in order {
        let expected = bindings.substitute(env, &expanded[index].ty, expressions, actual)?;
        let actual_type = contextual_type(env, &expressions[index], &actual[index], &expected)?;
        bindings.unify(env, &expected, &actual_type, index)?;
        bounds(env, function, &mut bindings, expressions, actual, false)?;
    }
    bounds(env, function, &mut bindings, expressions, actual, complete)?;
    for (index, parameter) in expanded.iter_mut().enumerate() {
        parameter.ty = bindings.substitute(env, &parameter.ty, expressions, actual)?;
        if complete {
            let actual_type =
                contextual_type(env, &expressions[index], &actual[index], &parameter.ty)?;
            bindings.unify(env, &parameter.ty, &actual_type, index)?;
        }
    }
    Ok((expanded, bindings))
}

fn contextual_type(
    env: &TypeEnvironment,
    expression: &Expr,
    actual: &Type,
    expected: &Type,
) -> Result<Type> {
    if crate::analysis::contextual_number(expression) && expected.number_kind().is_some() {
        let contextual = crate::analysis::contextualize(expression, expected)
            .map_err(NativeContractError::ContextualLiteral)?;
        return crate::inference::expression_type(&contextual, &HashMap::new(), env)
            .map_err(NativeContractError::ContextualLiteral);
    }
    Ok(actual.clone())
}

fn instantiate(
    env: &TypeEnvironment,
    declaration: DeclarationId,
    header: &Function,
    type_args: &[Type],
    expressions: &[Expr],
    actual: &[Type],
    bindings: Bindings,
) -> Result<InstantiatedFunction> {
    let semantic = semantic(header)?;
    let (params, bindings) = prepare(env, header, type_args, expressions, actual, bindings, true)?;
    crate::declarations::validate_native_ownership(semantic, &params, header.span)
        .map_err(NativeContractError::ContextualLiteral)?;
    let result = bindings.substitute(env, &header.result, expressions, actual)?;
    let mut kind = header.kind;
    if matches!(
        semantic,
        InternalSemantic::Function(Intrinsic::CloneValue | Intrinsic::DropValue)
    ) {
        let ty = if semantic == InternalSemantic::Function(Intrinsic::CloneValue) {
            params.first().map(|param| param.ty.pointee_or_self())
        } else {
            params.first().map(|param| &param.ty)
        };
        if ty.is_some_and(|ty| !crate::ownership::effectful_lifecycle(ty)) {
            kind = RoutineKind::Function;
        }
    }
    Ok(InstantiatedFunction {
        declaration,
        semantic,
        kind,
        params,
        result,
        obligations: bindings.obligations,
    })
}

/// Instantiates a declaration-owned source signature; retains no argument storage.
pub fn function(
    env: &TypeEnvironment,
    declaration: DeclarationId,
    type_args: &[Type],
    arguments: &[Expr],
    argument_types: &[Type],
    span: Span,
) -> std::result::Result<InstantiatedFunction, Diagnostic> {
    let header = env
        .native
        .functions
        .iter()
        .find(|header| header.id == declaration)
        .ok_or_else(|| {
            invalid_native_contract(NativeContractError::UnknownDeclaration(declaration), span)
        })?;
    instantiate(
        env,
        declaration,
        &header.function,
        type_args,
        arguments,
        argument_types,
        Bindings::default(),
    )
    .map_err(|error| {
        invalid_native_contract(error, span)
            .with_related(header.function.span, "native signature declared here")
    })
}

/// Supplies source-driven contextual parameter types before operand lowering.
pub fn function_parameter_types(
    env: &TypeEnvironment,
    declaration: DeclarationId,
    type_args: &[Type],
    arguments: &[Expr],
    argument_types: &[Type],
) -> std::result::Result<Vec<Type>, Diagnostic> {
    let header = env
        .native
        .functions
        .iter()
        .find(|header| header.id == declaration)
        .ok_or_else(|| {
            invalid_native_contract(
                NativeContractError::UnknownDeclaration(declaration),
                Span::default(),
            )
        })?;
    prepare(
        env,
        &header.function,
        type_args,
        arguments,
        argument_types,
        Bindings::default(),
        false,
    )
    .map(|(params, _)| params.into_iter().map(|param| param.ty).collect())
    .map_err(|error| invalid_native_contract(error, header.function.span))
}

fn unresolved(ty: &Type, env: &TypeEnvironment) -> bool {
    let mut ty = ty.clone();
    let mut found = false;
    cyallo_ast::visit::visit_type(&mut ty, &mut |part| {
        found |= matches!(part, Type::Named(name) if !env.contains_key(name.as_ref()))
            || matches!(
                part,
                Type::SelfType | Type::ValueParameter(_) | Type::Dependent(..)
            );
    });
    found
}

/// Checks a native descriptor against a contextual ordinary function type.
/// Unknown output binders may be left to the enclosing generic resolver; input
/// types must already be known. Value-dependent results still require constants
/// and therefore fail when projected from the synthetic dynamic operands.
pub fn as_callable(
    env: &TypeEnvironment,
    declaration: DeclarationId,
    expected: &Type,
    span: Span,
) -> std::result::Result<CallableCoercion, Diagnostic> {
    let attempt = || -> Result<CallableCoercion> {
        let Type::Function(expected_kind, parameters, expected_result) = expected else {
            return Err(NativeContractError::CallableRequired(expected.clone()));
        };
        let header = &env
            .native
            .functions
            .iter()
            .find(|entry| entry.id == declaration)
            .ok_or(NativeContractError::UnknownDeclaration(declaration))?
            .function;
        for (index, (ty, _)) in parameters.iter().enumerate() {
            if unresolved(ty, env) {
                return Err(NativeContractError::UnresolvedCallableInput {
                    index,
                    ty: ty.clone(),
                });
            }
        }
        let expressions: Vec<_> = parameters
            .iter()
            .enumerate()
            .map(|(index, _)| Expr {
                kind: ExprKind::Name(format!("$native_arg{index}").into()),
                span,
            })
            .collect();
        let actual: Vec<_> = parameters.iter().map(|(ty, _)| ty.clone()).collect();
        let (expanded, mut bindings) = prepare(
            env,
            header,
            &[],
            &expressions,
            &actual,
            Bindings::default(),
            false,
        )?;
        for (index, (parameter, (_, by_ref))) in expanded.iter().zip(parameters).enumerate() {
            if parameter.by_ref != *by_ref {
                return Err(NativeContractError::CallableParameterMode { index });
            }
        }
        if !unresolved(expected_result, env) {
            let native_result = bindings.substitute(env, &header.result, &expressions, &actual)?;
            bindings.unify(env, &native_result, expected_result, parameters.len())?;
        }
        bounds(env, header, &mut bindings, &expressions, &actual, true)?;
        let mut type_args = vec![];
        for parameter in &header.type_params {
            if let Some(pack) = bindings.packs.get(parameter) {
                type_args.extend(pack.iter().cloned());
            } else {
                let ty = bindings
                    .values
                    .get(parameter)
                    .ok_or_else(|| NativeContractError::UninferredParameter(parameter.clone()))?;
                if unresolved(ty, env) {
                    return Err(NativeContractError::UninferredParameter(parameter.clone()));
                }
                type_args.push(ty.clone());
            }
        }
        let contract = instantiate(
            env,
            declaration,
            header,
            &type_args,
            &expressions,
            &actual,
            Bindings::default(),
        )?;
        if contract.kind != *expected_kind
            && !(*expected_kind == RoutineKind::Procedure && contract.kind == RoutineKind::Function)
        {
            return Err(NativeContractError::CallableEffect {
                expected: *expected_kind,
                actual: contract.kind,
            });
        }
        if !unresolved(expected_result, env) && !equivalent(&contract.result, expected_result) {
            return Err(NativeContractError::ArgumentType {
                index: parameters.len(),
                expected: *expected_result.clone(),
                actual: contract.result.clone(),
            });
        }
        let callable_type = Type::Function(
            contract.kind,
            contract
                .params
                .iter()
                .map(|parameter| (parameter.ty.clone(), parameter.by_ref))
                .collect(),
            Box::new(contract.result.clone()),
        );
        Ok(CallableCoercion {
            contract,
            type_args,
            callable_type,
        })
    };
    attempt().map_err(|error| invalid_native_contract(error, span))
}

fn member_owner(
    env: &TypeEnvironment,
    header: &NativeMethodDeclaration,
    receiver: &Type,
) -> Result<Bindings> {
    let owner = match receiver {
        Type::Reference { pointee, .. } => pointee.as_ref(),
        _ => receiver,
    };
    let mut bindings = Bindings::default();
    bindings
        .parameters
        .extend(header.owner_parameters.iter().cloned());
    if let (Type::Slice(element), Type::Array(_, actual)) = (&header.receiver, owner) {
        bindings.unify(env, element, actual, 0)?;
    } else if let (
        Type::Managed {
            kind: p_kind,
            locality: p_locality,
            pointee: p,
            ..
        },
        Type::Managed {
            kind: a_kind,
            locality: a_locality,
            pointee: a,
            ..
        },
    ) = (&header.receiver, owner)
    {
        if p_kind != a_kind || p_locality != a_locality {
            return Err(NativeContractError::ArgumentType {
                index: 0,
                expected: header.receiver.clone(),
                actual: owner.clone(),
            });
        }
        bindings.unify(env, p, a, 0)?;
    } else if let (Type::Storage(StorageKind::Weak, p), Type::Storage(StorageKind::Weak, a)) =
        (&header.receiver, owner)
    {
        match (p.as_ref(), a.as_ref()) {
            (
                Type::Managed {
                    kind: MemoryKind::Rc,
                    locality: p_locality,
                    pointee: p,
                    ..
                },
                Type::Managed {
                    kind: MemoryKind::Rc,
                    locality: a_locality,
                    pointee: a,
                    ..
                },
            ) if p_locality == a_locality => bindings.unify(env, p, a, 0)?,
            _ => {
                return Err(NativeContractError::ArgumentType {
                    index: 0,
                    expected: header.receiver.clone(),
                    actual: owner.clone(),
                });
            }
        }
    } else {
        bindings.unify(env, &header.receiver, owner, 0)?;
    }
    // Array receivers use the declared slice view, including in result types.
    // Other native owners retain region/pure-generator refinements of Self.
    if !matches!(header.receiver, Type::Slice(_)) {
        bindings.owner = Some((header.receiver.clone(), owner.clone()));
    }
    Ok(bindings)
}
fn receiver_mode(header: &Function) -> Result<ReceiverMode> {
    let receiver = header
        .params
        .first()
        .filter(|param| param.receiver)
        .ok_or(NativeContractError::InvalidHeader)?;
    if receiver.passing == ParameterPassing::Read {
        return Ok(ReceiverMode::Value);
    }
    Ok(match receiver.ty {
        Type::Reference { mutable: true, .. } => ReceiverMode::Mutable,
        Type::Reference { .. } => ReceiverMode::Shared,
        _ => ReceiverMode::Move,
    })
}
fn supplied_receiver(receiver: &Type, mode: ReceiverMode) -> Result<Type> {
    match mode {
        ReceiverMode::Shared | ReceiverMode::Mutable => match receiver {
            Type::Reference { mutable, .. } if mode == ReceiverMode::Mutable && !mutable => {
                Err(NativeContractError::ArgumentType {
                    index: 0,
                    expected: Type::Reference {
                        mutable: true,
                        lifetime: None,
                        pointee: Box::new(receiver.pointee_or_self().clone()),
                    },
                    actual: receiver.clone(),
                })
            }
            Type::Reference { .. } => Ok(receiver.clone()),
            _ => Ok(Type::Reference {
                mutable: mode == ReceiverMode::Mutable,
                lifetime: None,
                pointee: Box::new(receiver.clone()),
            }),
        },
        ReceiverMode::Value => match receiver {
            Type::Reference { pointee, .. } => Ok(*pointee.clone()),
            _ => Ok(receiver.clone()),
        },
        _ => match receiver {
            Type::Reference { pointee, .. } if !crate::ownership::copy(pointee) => Err(
                NativeContractError::ReceiverRequiresOwnership(*pointee.clone()),
            ),
            Type::Reference { pointee, .. } => Ok(*pointee.clone()),
            _ => Ok(receiver.clone()),
        },
    }
}

/// Performs ordinary declaration-based member lookup and instantiates its header.
/// Mutable addressability and deferred trait evidence remain caller obligations.
pub fn member(
    env: &TypeEnvironment,
    receiver: &Type,
    name: &str,
    args: &[Expr],
    arg_types: &[Type],
    span: Span,
) -> std::result::Result<Option<InstantiatedMember>, Diagnostic> {
    let direct = member_direct(env, receiver, name, args, arg_types, span)?;
    if direct.is_some() {
        return Ok(direct);
    }
    if let Some(receiver) = dereference_gc(receiver) {
        return member(env, &receiver, name, args, arg_types, span);
    }
    Ok(None)
}

fn dereference_gc(receiver: &Type) -> Option<Type> {
    match receiver {
        Type::Managed {
            kind: MemoryKind::Gc,
            pointee,
            ..
        } => Some(*pointee.clone()),
        Type::Reference {
            mutable,
            lifetime,
            pointee,
        } => dereference_gc(pointee).map(|pointee| Type::Reference {
            mutable: *mutable,
            lifetime: lifetime.clone(),
            pointee: Box::new(pointee),
        }),
        _ => None,
    }
}

fn member_direct(
    env: &TypeEnvironment,
    receiver: &Type,
    name: &str,
    args: &[Expr],
    arg_types: &[Type],
    span: Span,
) -> std::result::Result<Option<InstantiatedMember>, Diagnostic> {
    let mut found = None;
    let mut failure = None;
    let mut first_span = None;
    for header in &env.native.methods {
        if !env.member_visible(header, span) || header.function.source_name.as_ref() != name {
            continue;
        }
        let Ok(bindings) = member_owner(env, header, receiver) else {
            continue;
        };
        let attempt = (|| {
            let mode = receiver_mode(&header.function)?;
            let mut actual = vec![supplied_receiver(receiver, mode)?];
            actual.extend_from_slice(arg_types);
            let mut expressions = vec![Expr {
                kind: ExprKind::Name("$receiver".into()),
                span,
            }];
            expressions.extend_from_slice(args);
            let mut callable = instantiate(
                env,
                header.id,
                &header.function,
                &[],
                &expressions,
                &actual,
                bindings,
            )?;
            callable.params.remove(0);
            let owner = match receiver {
                Type::Reference { pointee, .. } => pointee.as_ref(),
                _ => receiver,
            };
            if matches!(owner, Type::Storage(StorageKind::PureGenerator, _))
                && matches!(
                    callable.semantic,
                    InternalSemantic::Member(
                        MemberIntrinsic::GeneratorStart
                            | MemberIntrinsic::GeneratorResume
                            | MemberIntrinsic::GeneratorNext
                            | MemberIntrinsic::GeneratorClose
                    )
                )
            {
                callable.kind = RoutineKind::Function;
            }
            Ok::<_, NativeContractError>(InstantiatedMember {
                callable,
                receiver: mode,
                receiver_type: receiver.clone(),
            })
        })();
        match attempt {
            Ok(candidate) => {
                if found.is_some() {
                    return Err(invalid_native_contract(
                        NativeContractError::AmbiguousMember {
                            receiver: receiver.clone(),
                            name: name.into(),
                        },
                        span,
                    )
                    .with_related(first_span.unwrap(), "first matching declaration")
                    .with_related(header.function.span, "another matching declaration"));
                }
                first_span = Some(header.function.span);
                found = Some(candidate);
            }
            Err(error) => {
                failure.get_or_insert((error, header.function.span));
            }
        }
    }
    if found.is_none()
        && let Some((error, declaration_span)) = failure
    {
        return Err(invalid_native_contract(error, span)
            .with_related(declaration_span, "native signature declared here"));
    }
    Ok(found)
}

/// Contextual member parameters from the resolved owner; unresolved method type
/// binders remain named until arguments provide evidence. Ambiguity returns None.
pub fn parameter_types(
    env: &TypeEnvironment,
    receiver: &Type,
    name: &str,
    span: Span,
) -> Option<Vec<Type>> {
    parameter_types_direct(env, receiver, name, span).or_else(|| {
        dereference_gc(receiver).and_then(|receiver| parameter_types(env, &receiver, name, span))
    })
}

fn parameter_types_direct(
    env: &TypeEnvironment,
    receiver: &Type,
    name: &str,
    span: Span,
) -> Option<Vec<Type>> {
    let mut found: Option<Vec<Type>> = None;
    for header in &env.native.methods {
        if !env.member_visible(header, span) || header.function.source_name.as_ref() != name {
            continue;
        }
        let Ok(bindings) = member_owner(env, header, receiver) else {
            continue;
        };
        let parameters = header
            .function
            .params
            .iter()
            .skip(1)
            .map(|parameter| bindings.substitute(env, &parameter.ty, &[], &[]))
            .collect::<Result<Vec<_>>>()
            .ok()?;
        if let Some(previous) = &found
            && *previous != parameters
        {
            return None;
        }
        found = Some(parameters);
    }
    found
}

fn family(
    env: &TypeEnvironment,
    declaration: DeclarationId,
    operands: &[Type],
    expressions: &[Expr],
    actual: &[Type],
) -> Result<Type> {
    let record = env
        .native
        .families
        .iter()
        .find(|(id, _)| *id == declaration)
        .map(|(_, record)| record)
        .ok_or(NativeContractError::UnknownFamily(declaration))?;
    let family = match record
        .annotations
        .first()
        .and_then(|annotation| internal::from_key(&annotation.value))
    {
        Some(InternalSemantic::TypeFamily(family)) => family,
        _ => return Err(NativeContractError::UnknownFamily(declaration)),
    };
    let expected = crate::declarations::family_arity(family);
    if operands.len() != expected {
        return Err(NativeContractError::FamilyArity {
            family,
            expected,
            actual: operands.len(),
        });
    }
    let ty = |operand: &Type| match operand {
        Type::ValueParameter(index) => actual
            .get(*index)
            .cloned()
            .ok_or(NativeContractError::ValueParameterMissing(*index)),
        _ => Ok(operand.clone()),
    };
    let expression = |operand: &Type| match operand {
        Type::ValueParameter(index) => expressions
            .get(*index)
            .ok_or(NativeContractError::ValueParameterMissing(*index)),
        _ => Err(NativeContractError::InvalidFamilyOperand {
            family,
            operand: operand.clone(),
        }),
    };
    let invalid = |operand: Type| NativeContractError::InvalidFamilyOperand { family, operand };
    match family {
        NativeTypeFamily::EmptyArray => Ok(Type::Array(vec![(0, -1)], Box::new(ty(&operands[0])?))),
        NativeTypeFamily::RangeArray => {
            let range_type = ty(&operands[0])?;
            let Type::Storage(StorageKind::Range, element) = range_type else {
                return Err(invalid(range_type));
            };
            let range = unparenthesized(expression(&operands[0])?);
            let ExprKind::Binary(
                operator @ (Operator::Range | Operator::RangeInclusive),
                start,
                end,
            ) = &range.kind
            else {
                return Err(NativeContractError::ConstantRangeRequired);
            };
            let start =
                constant_integer(start).ok_or(NativeContractError::ConstantRangeRequired)?;
            let end = constant_integer(end).ok_or(NativeContractError::ConstantRangeRequired)?;
            let length = end
                .checked_sub(start)
                .and_then(|length| {
                    length.checked_add(i128::from(*operator == Operator::RangeInclusive))
                })
                .ok_or(NativeContractError::ArrayLengthOverflow)?
                .max(0);
            let length =
                i64::try_from(length).map_err(|_| NativeContractError::ArrayLengthOverflow)?;
            Ok(Type::Array(vec![(0, length - 1)], element))
        }
        NativeTypeFamily::Field => {
            let mut owner = ty(&operands[0])?;
            if matches!(operands[0], Type::ValueParameter(_)) {
                if let Type::Reference { pointee, .. } = owner {
                    owner = *pointee;
                }
                if let Type::Managed {
                    kind: MemoryKind::Gc,
                    pointee,
                    ..
                } = owner
                {
                    owner = *pointee;
                }
            }
            owner = view(env, &owner);
            let field = constant_text(expression(&operands[1])?)
                .ok_or(NativeContractError::ConstantFieldRequired)?;
            let Type::Record(_, fields) = &owner else {
                return Err(invalid(owner));
            };
            fields
                .iter()
                .find(|(name, _)| name.as_ref() == field)
                .map(|(_, ty)| ty.clone())
                .ok_or_else(|| NativeContractError::UnknownField {
                    owner: owner.clone(),
                    field: field.into(),
                })
        }
        NativeTypeFamily::Element => {
            let owner = ty(&operands[0])?;
            collection_element(&owner, false).ok_or_else(|| invalid(owner))
        }
        NativeTypeFamily::Target => {
            let owner = ty(&operands[0])?;
            match owner {
                Type::Reference { pointee, .. }
                | Type::Managed { pointee, .. }
                | Type::Storage(
                    StorageKind::Cell | StorageKind::Weak | StorageKind::StoragePath,
                    pointee,
                ) => Ok(*pointee),
                _ => Err(invalid(owner)),
            }
        }
        NativeTypeFamily::AllocatedRc => Ok(crate::memory::managed_type(
            MemoryKind::Rc,
            Locality::Local,
            Some(&ty(&operands[1])?),
            ty(&operands[0])?,
        )),
        NativeTypeFamily::WeakOf => {
            let owner = ty(&operands[0])?;
            if !matches!(
                owner,
                Type::Managed {
                    kind: MemoryKind::Rc,
                    ..
                }
            ) {
                return Err(invalid(owner));
            }
            Ok(Type::Storage(StorageKind::Weak, Box::new(owner)))
        }
    }
}

fn unparenthesized(mut expression: &Expr) -> &Expr {
    while let ExprKind::Parenthesized(inner) = &expression.kind {
        expression = inner;
    }
    expression
}
fn constant_integer(expression: &Expr) -> Option<i128> {
    match &unparenthesized(expression).kind {
        ExprKind::Literal(Literal::Integer(value)) => Some(i128::from(*value)),
        ExprKind::Literal(Literal::Number(value)) if !value.kind.float() => Some(value.int()),
        ExprKind::Frozen(value, _) => match value.as_ref() {
            ConstantValue::Number(value) if !value.kind.float() => Some(value.int()),
            ConstantValue::Integer(value) => Some(i128::from(*value)),
            _ => None,
        },
        ExprKind::Unary(Operator::Add, inner) => constant_integer(inner),
        ExprKind::Unary(Operator::Subtract, inner) => constant_integer(inner)?.checked_neg(),
        ExprKind::Binary(operator, a, b) => {
            let a = constant_integer(a)?;
            let b = constant_integer(b)?;
            match operator {
                Operator::Add => a.checked_add(b),
                Operator::Subtract => a.checked_sub(b),
                Operator::Multiply => a.checked_mul(b),
                Operator::Divide | Operator::IntegerDivide => a.checked_div(b),
                Operator::Remainder => a.checked_rem(b),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Evaluates a fully substituted source type, including native dependent types.
pub fn instantiate_type(
    env: &TypeEnvironment,
    ty: &Type,
    arguments: &[Expr],
    argument_types: &[Type],
    span: Span,
) -> std::result::Result<Type, Diagnostic> {
    Bindings::default()
        .substitute(env, ty, arguments, argument_types)
        .map_err(|error| invalid_native_contract(error, span))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn environment() -> TypeEnvironment {
        let mut ast = crate::namespace::single(cyallo_parser::parse("").unwrap()).unwrap();
        let types = crate::composites::resolve(&mut ast).unwrap();
        TypeEnvironment::new(types, ast.native)
    }
    fn value() -> Expr {
        Expr {
            kind: ExprKind::Name("argument".into()),
            span: SPAN,
        }
    }
    fn integer(value: i64) -> Expr {
        Expr {
            kind: ExprKind::Literal(Literal::Integer(value)),
            span: SPAN,
        }
    }
    fn text(value: &str) -> Expr {
        Expr {
            kind: ExprKind::Literal(Literal::String(value.into())),
            span: SPAN,
        }
    }
    fn storage(kind: StorageKind, inner: Type) -> Type {
        Type::Storage(kind, Box::new(inner))
    }
    fn reference(inner: Type) -> Type {
        Type::Reference {
            mutable: false,
            lifetime: None,
            pointee: Box::new(inner),
        }
    }
    fn managed(kind: MemoryKind, lifetime: Option<&str>, inner: Type) -> Type {
        Type::Managed {
            kind,
            locality: Locality::Local,
            lifetime: lifetime.map(Into::into),
            pointee: Box::new(inner),
        }
    }
    fn header(env: &TypeEnvironment, key: &str) -> NativeFunctionDeclaration {
        env.native
            .functions
            .iter()
            .find(|header| header.function.annotations[0].value.as_ref() == key)
            .unwrap_or_else(|| panic!("missing test ABI declaration {key}"))
            .clone()
    }
    fn family_id(env: &TypeEnvironment, key: &str) -> DeclarationId {
        env.native
            .families
            .iter()
            .find(|(_, record)| record.annotations[0].value.as_ref() == key)
            .unwrap()
            .0
    }
    fn call(
        env: &TypeEnvironment,
        key: &str,
        type_args: &[Type],
        expressions: &[Expr],
        actual: &[Type],
    ) -> Result<InstantiatedFunction> {
        let header = header(env, key);
        instantiate(
            env,
            header.id,
            &header.function,
            type_args,
            expressions,
            actual,
            Bindings::default(),
        )
    }

    #[test]
    fn fixed_source_parameters_contextualize_literals_before_emission() {
        let env = environment();
        let declaration = header(&env, "channel_bounded");
        let types = function_parameter_types(
            &env,
            declaration.id,
            &[Type::Integer],
            &[integer(2)],
            &[Type::Integer],
        )
        .unwrap();
        assert_eq!(types, vec![Type::numeric(NumberKind::Usize)]);
        let call = call(
            &env,
            "channel_bounded",
            &[Type::Integer],
            &[integer(2)],
            &[Type::Integer],
        )
        .unwrap();
        assert_eq!(call.params[0].ty, Type::numeric(NumberKind::Usize));
        assert_eq!(
            call.result,
            Type::Tuple(vec![
                storage(StorageKind::Sender, Type::Integer),
                storage(StorageKind::Receiver, Type::Integer)
            ])
        );
    }

    #[test]
    fn generic_numeric_context_comes_from_typed_operands_and_rejects_mixed_types() {
        let env = environment();
        let u8_type = Type::numeric(NumberKind::U8);
        let maximum = call(
            &env,
            "numeric_max",
            &[],
            &[value(), integer(2)],
            &[u8_type.clone(), Type::Integer],
        )
        .unwrap();
        assert_eq!(maximum.result, u8_type);
        assert_eq!(
            maximum
                .params
                .iter()
                .map(|param| param.ty.clone())
                .collect::<Vec<_>>(),
            vec![Type::numeric(NumberKind::U8); 2]
        );
        assert!(matches!(
            call(
                &env,
                "numeric_max",
                &[],
                &[value(), value()],
                &[
                    Type::numeric(NumberKind::U8),
                    Type::numeric(NumberKind::U16)
                ]
            ),
            Err(NativeContractError::ArgumentType { .. })
        ));
        assert!(matches!(
            call(
                &env,
                "numeric_min",
                &[],
                &[value(), value()],
                &[Type::Boolean, Type::Boolean]
            ),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        assert_eq!(
            call(
                &env,
                "numeric_min",
                &[],
                &[value(), value()],
                &[Type::Numeric(NumberKind::I64), Type::Integer]
            )
            .unwrap()
            .result,
            Type::Integer
        );
        assert!(
            call(
                &env,
                "numeric_min",
                &[Type::numeric(NumberKind::U8)],
                &[integer(-1), integer(0)],
                &[Type::Integer, Type::Integer]
            )
            .is_err()
        );
    }

    #[test]
    fn packs_expand_per_argument_and_check_every_bound() {
        let env = environment();
        let printed = call(
            &env,
            "print",
            &[],
            &[text("value="), integer(2), value()],
            &[Type::String, Type::Integer, Type::Boolean],
        )
        .unwrap();
        assert_eq!(
            printed
                .params
                .iter()
                .map(|param| param.ty.clone())
                .collect::<Vec<_>>(),
            vec![Type::String, Type::Integer, Type::Boolean]
        );
        assert_eq!(printed.result, Type::Void);
        assert!(
            printed
                .params
                .iter()
                .all(|parameter| parameter.passing == ParameterPassing::Read)
        );
        assert_eq!(source_consumes_argument(&printed, 0), Some(false));
        assert_eq!(source_consumes_argument(&printed, 3), None);
        assert!(
            call(&env, "print", &[], &[], &[])
                .unwrap()
                .params
                .is_empty()
        );
        assert!(matches!(
            call(&env, "print", &[], &[value()], &[Type::Any]),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        assert!(matches!(
            call(
                &env,
                "print",
                &[Type::String, Type::Integer],
                &[text("x")],
                &[Type::String]
            ),
            Err(NativeContractError::InvalidPack)
        ));
    }

    #[test]
    fn callable_effect_widening_preserves_declared_return_type_and_bounds() {
        let env = environment();
        let pure_callback = Type::Function(RoutineKind::Function, vec![], Box::new(Type::String));
        let spawned = call(&env, "task_spawn", &[], &[value()], &[pure_callback]).unwrap();
        assert_eq!(spawned.kind, RoutineKind::Procedure);
        assert_eq!(spawned.result, storage(StorageKind::Task, Type::String));
        let local_return = Type::Function(
            RoutineKind::Procedure,
            vec![],
            Box::new(managed(MemoryKind::Gc, None, Type::Integer)),
        );
        assert!(matches!(
            call(&env, "task_spawn", &[], &[value()], &[local_return]),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        let wrong_arity = Type::Function(
            RoutineKind::Function,
            vec![(Type::Integer, false)],
            Box::new(Type::Integer),
        );
        assert!(matches!(
            call(&env, "task_spawn", &[], &[value()], &[wrong_arity]),
            Err(NativeContractError::ArgumentType { .. })
        ));
    }

    #[test]
    fn collection_bounds_infer_element_parameters_without_unbound_results() {
        let env = environment();
        let element = Type::numeric(NumberKind::I16);
        let summed = call(
            &env,
            "array_sum",
            &[],
            &[value()],
            &[storage(StorageKind::Vec, element.clone())],
        )
        .unwrap();
        assert_eq!(summed.result, element);
        assert!(matches!(
            call(
                &env,
                "array_sum",
                &[],
                &[value()],
                &[storage(StorageKind::Vec, Type::String)]
            ),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        let length = call(
            &env,
            "array_length",
            &[],
            &[value()],
            &[reference(Type::Slice(Box::new(Type::Boolean)))],
        )
        .unwrap();
        assert_eq!(length.result, Type::Integer);
    }

    #[test]
    fn fixed_array_type_families_use_actual_constant_operands() {
        let env = environment();
        let range = Expr {
            kind: ExprKind::Binary(
                Operator::RangeInclusive,
                Box::new(integer(2)),
                Box::new(integer(4)),
            ),
            span: SPAN,
        };
        let array = call(
            &env,
            "array_from",
            &[],
            &[range],
            &[storage(StorageKind::Range, Type::Integer)],
        )
        .unwrap();
        assert_eq!(
            array.result,
            Type::Array(vec![(0, 2)], Box::new(Type::Integer))
        );
        let empty = call(
            &env,
            "array_empty",
            &[Type::numeric(NumberKind::U8)],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(
            empty.result,
            Type::Array(vec![(0, -1)], Box::new(Type::numeric(NumberKind::U8)))
        );
        assert!(matches!(
            call(
                &env,
                "array_from",
                &[],
                &[value()],
                &[storage(StorageKind::Range, Type::Integer)]
            ),
            Err(NativeContractError::ConstantRangeRequired)
        ));
    }

    #[test]
    fn field_projection_uses_record_identity_and_constant_field_values() {
        let mut env = environment();
        let record = Type::Record("fixture".into(), vec![("field".into(), Type::Boolean)]);
        env.insert("fixture".into(), record.clone());
        let owner = reference(managed(
            MemoryKind::Gc,
            None,
            Type::RecordRef("fixture".into()),
        ));
        let location = call(
            &env,
            "field_location",
            &[],
            &[value(), text("field")],
            &[owner.clone(), Type::String],
        )
        .unwrap();
        assert_eq!(
            location.result,
            storage(StorageKind::Location, Type::Boolean)
        );
        assert_eq!(location.params[0].passing, ParameterPassing::Read);
        assert_eq!(location.params[1].passing, ParameterPassing::Read);
        assert_eq!(location.params[0].ty, owner);
        let direct_owner = managed(MemoryKind::Gc, None, Type::RecordRef("fixture".into()));
        let direct = call(
            &env,
            "field_location",
            &[],
            &[value(), text("field")],
            &[direct_owner.clone(), Type::String],
        )
        .unwrap();
        assert_eq!(direct.params[0].ty, direct_owner);
        assert_eq!(direct.result, storage(StorageKind::Location, Type::Boolean));
        assert!(matches!(
            call(
                &env,
                "field_location",
                &[],
                &[value(), text("field")],
                &[managed(MemoryKind::Rc, None, record.clone()), Type::String]
            ),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        assert!(matches!(
            call(
                &env,
                "field_location",
                &[],
                &[value(), text("absent")],
                &[owner.clone(), Type::String]
            ),
            Err(NativeContractError::UnknownField { .. })
        ));
        assert!(matches!(
            call(
                &env,
                "field_location",
                &[],
                &[value(), value()],
                &[owner, Type::String]
            ),
            Err(NativeContractError::ConstantFieldRequired)
        ));
        let path = storage(StorageKind::StoragePath, record);
        let field = member(
            &env,
            &path,
            "field",
            &[text("field")],
            &[Type::String],
            SPAN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            field.callable.result,
            storage(StorageKind::StoragePath, Type::Boolean)
        );
    }

    #[test]
    fn member_names_select_declarations_without_becoming_operation_names() {
        let mut env = environment();
        for method in &mut Arc::make_mut(&mut env.native).methods {
            if method.function.annotations[0].value.as_ref() == "clock_sleep" {
                method.function.source_name = "pause".into();
            }
        }
        let clock = storage(StorageKind::Clock, Type::Void);
        assert!(
            member(&env, &clock, "sleep", &[integer(1)], &[Type::Integer], SPAN)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            parameter_types(&env, &clock, "pause", Span::default()),
            Some(vec![Type::numeric(NumberKind::U64)])
        );
        let paused = member(&env, &clock, "pause", &[integer(1)], &[Type::Integer], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(
            paused.callable.semantic,
            InternalSemantic::Member(MemberIntrinsic::ClockSleep)
        );
        assert_eq!(paused.receiver, ReceiverMode::Shared);
        assert!(
            member(
                &env,
                &Type::Record("user".into(), vec![]),
                "pause",
                &[],
                &[],
                SPAN
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn ownership_and_generic_unit_overloads_come_from_source_headers() {
        let env = environment();
        let task = storage(StorageKind::Task, Type::Integer);
        let joined = member(&env, &task, "join", &[], &[], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(joined.receiver, ReceiverMode::Move);
        assert_eq!(
            joined.callable.result,
            storage(StorageKind::TaskOutcome, Type::Integer)
        );
        assert!(member(&env, &reference(task), "join", &[], &[], SPAN).is_err());
        let pure = storage(
            StorageKind::PureGenerator,
            Type::Tuple(vec![Type::Integer, Type::Void, Type::Integer]),
        );
        let resumed = member(&env, &pure, "resume", &[], &[], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(resumed.callable.kind, RoutineKind::Function);
        assert!(resumed.callable.params.is_empty());
        assert_eq!(
            resumed.callable.result,
            storage(
                StorageKind::GeneratorStep,
                Type::Tuple(vec![Type::Integer, Type::Integer])
            )
        );
    }

    #[test]
    fn borrowed_allocator_regions_survive_clone_weak_conversion_and_upgrade() {
        let env = environment();
        let owner = managed(MemoryKind::Rc, Some("pool"), Type::Integer);
        let cloned = call(
            &env,
            "rc_clone",
            &[],
            &[value()],
            &[reference(owner.clone())],
        )
        .unwrap();
        assert_eq!(cloned.result, owner);
        let weak = call(
            &env,
            "rc_downgrade",
            &[],
            &[value()],
            &[reference(owner.clone())],
        )
        .unwrap()
        .result;
        assert_eq!(weak, storage(StorageKind::Weak, owner.clone()));
        let upgraded = call(
            &env,
            "rc_upgrade",
            &[],
            &[value()],
            &[reference(weak.clone())],
        )
        .unwrap();
        assert_eq!(upgraded.result, storage(StorageKind::Option, owner.clone()));
        let method = member(&env, &owner, "clone", &[], &[], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(method.callable.result, owner.clone());
        let method = member(&env, &weak, "upgrade", &[], &[], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(method.callable.result, storage(StorageKind::Option, owner));
    }

    #[test]
    fn allocator_and_user_trait_evidence_is_retained_as_an_obligation() {
        let env = environment();
        let allocator = Type::Reference {
            mutable: true,
            lifetime: Some("pool".into()),
            pointee: Box::new(Type::Record("allocator".into(), vec![])),
        };
        let allocated = call(
            &env,
            "rc_try_new_in",
            &[],
            &[integer(1), value()],
            &[Type::Integer, allocator.clone()],
        )
        .unwrap();
        assert_eq!(allocated.obligations.len(), 1);
        assert_eq!(
            allocated.obligations[0].kind,
            TraitObligationKind::Allocator
        );
        assert_eq!(allocated.obligations[0].ty, allocator);
        let Type::Storage(StorageKind::Result, result) = allocated.result else {
            panic!("declared Result")
        };
        let Type::Tuple(result) = *result else {
            panic!("Result type arguments")
        };
        assert!(
            matches!(&result[0],Type::Managed {kind:MemoryKind::Rc,lifetime:Some(_),pointee,..} if **pointee==Type::Integer)
        );

        let mut header = header(&env, "numeric_abs");
        header.function.constraints[0].bounds = vec![Type::Named("user_trait".into())];
        let checked = instantiate(
            &env,
            header.id,
            &header.function,
            &[],
            &[value()],
            &[Type::Boolean],
            Bindings::default(),
        )
        .unwrap();
        assert_eq!(
            checked.obligations,
            vec![TraitObligation {
                ty: Type::Boolean,
                bound: Type::Named("user_trait".into()),
                kind: TraitObligationKind::UserTrait
            }]
        );
    }

    #[test]
    fn slice_member_views_and_conditional_clone_effects_preserve_the_contract() {
        let env = environment();
        let array = Type::Array(vec![(0, 2)], Box::new(Type::Integer));
        let slice = member(&env, &array, "as_slice", &[], &[], SPAN)
            .unwrap()
            .unwrap();
        assert_eq!(
            slice.callable.result,
            reference(Type::Slice(Box::new(Type::Integer)))
        );
        let pure_clone = call(
            &env,
            "clone_value",
            &[],
            &[value()],
            &[reference(Type::Integer)],
        )
        .unwrap();
        assert_eq!(pure_clone.kind, RoutineKind::Function);
        let effectful_clone = call(
            &env,
            "clone_value",
            &[],
            &[value()],
            &[reference(managed(MemoryKind::Rc, None, Type::Integer))],
        )
        .unwrap();
        assert_eq!(effectful_clone.kind, RoutineKind::Procedure);
    }

    #[test]
    fn malformed_contracts_and_missing_evidence_fail_with_typed_errors() {
        let env = environment();
        let mut malformed = header(&env, "print");
        malformed.function.type_packs.clear();
        assert!(matches!(
            instantiate(
                &env,
                malformed.id,
                &malformed.function,
                &[],
                &[],
                &[],
                Bindings::default()
            ),
            Err(NativeContractError::InvalidPack)
        ));
        let mut malformed = header(&env, "numeric_abs");
        malformed.function.constraints[0].parameter = "undeclared".into();
        assert!(matches!(
            instantiate(
                &env,
                malformed.id,
                &malformed.function,
                &[],
                &[integer(1)],
                &[Type::Integer],
                Bindings::default()
            ),
            Err(NativeContractError::InvalidHeader)
        ));
        assert!(matches!(
            call(&env, "option_none", &[], &[], &[]),
            Err(NativeContractError::UninferredParameter(_))
        ));
        assert!(matches!(
            call(
                &env,
                "numeric_abs",
                &[Type::Integer, Type::Integer],
                &[integer(1)],
                &[Type::Integer]
            ),
            Err(NativeContractError::TypeArgumentCount { .. })
        ));
        let missing = Type::Dependent(DeclarationId(u32::MAX), vec![]);
        assert!(matches!(
            Bindings::default().substitute(&env, &missing, &[], &[]),
            Err(NativeContractError::UnknownFamily(_))
        ));
        let invalid = Type::Dependent(family_id(&env, "field_type"), vec![Type::Integer]);
        assert!(matches!(
            Bindings::default().substitute(&env, &invalid, &[], &[]),
            Err(NativeContractError::FamilyArity { .. })
        ));
        let mut malformed = header(&env, "string_trim");
        malformed.function.params[0].by_ref = true;
        assert!(matches!(
            instantiate(
                &env,
                malformed.id,
                &malformed.function,
                &[],
                &[text("x")],
                &[Type::String],
                Bindings::default()
            ),
            Err(NativeContractError::InvalidPassing { index: 0 })
        ));
        malformed.function.params[0].by_ref = false;
        malformed.function.body_kind = FunctionBodyKind::Source;
        assert!(matches!(
            instantiate(
                &env,
                malformed.id,
                &malformed.function,
                &[],
                &[text("x")],
                &[Type::String],
                Bindings::default()
            ),
            Err(NativeContractError::InvalidHeader)
        ));
    }

    #[test]
    fn read_modes_retain_value_types_and_follow_source_metadata() {
        let env = environment();
        let trimmed = call(&env, "string_trim", &[], &[text(" x ")], &[Type::String]).unwrap();
        assert_eq!(trimmed.params[0].ty, Type::String);
        assert_eq!(trimmed.params[0].passing, ParameterPassing::Read);
        assert_eq!(source_consumes_argument(&trimmed, 0), Some(false));
        let mut changed = header(&env, "string_trim");
        changed.function.params[0].passing = ParameterPassing::Value;
        let changed = instantiate(
            &env,
            changed.id,
            &changed.function,
            &[],
            &[text("x")],
            &[Type::String],
            Bindings::default(),
        )
        .unwrap();
        assert_eq!(changed.semantic, trimmed.semantic);
        assert_eq!(source_consumes_argument(&changed, 0), Some(true));

        let array = Type::Array(vec![(0, 1)], Box::new(Type::Integer));
        let length = call(
            &env,
            "array_length",
            &[],
            &[value()],
            std::slice::from_ref(&array),
        )
        .unwrap();
        assert_eq!(length.params[0].ty, array);
        assert_eq!(source_consumes_argument(&length, 0), Some(false));
        let reversed = member(
            &env,
            &storage(StorageKind::Range, Type::Integer),
            "rev",
            &[],
            &[],
            SPAN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reversed.receiver, ReceiverMode::Value);
    }

    #[test]
    fn payloads_callbacks_and_service_keys_keep_their_independent_source_modes() {
        let env = environment();
        let sent = member(
            &env,
            &storage(StorageKind::Sender, Type::String),
            "send",
            &[text("payload")],
            &[Type::String],
            SPAN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(source_consumes_argument(&sent.callable, 0), Some(true));
        let spawned = call(
            &env,
            "task_spawn",
            &[],
            &[value()],
            &[Type::Function(
                RoutineKind::Procedure,
                vec![],
                Box::new(Type::Integer),
            )],
        )
        .unwrap();
        assert_eq!(source_consumes_argument(&spawned, 0), Some(true));
        let mapper = Type::Function(
            RoutineKind::Function,
            vec![(Type::Integer, false), (Type::Integer, false)],
            Box::new(storage(StorageKind::StoragePath, Type::Integer)),
        );
        let mapped = call(
            &env,
            "lookup_mapped",
            &[],
            &[integer(1), integer(2), value()],
            &[Type::Integer, Type::Integer, mapper],
        )
        .unwrap();
        assert!(
            mapped
                .params
                .iter()
                .all(|parameter| parameter.passing == ParameterPassing::Read)
        );

        let interface = Type::TraitObject("service_interface".into());
        let key = Type::ServiceKey(Box::new(interface.clone()));
        let provided = member(
            &env,
            &Type::Context,
            "provide",
            &[value(), value()],
            &[key.clone(), interface],
            SPAN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(source_consumes_argument(&provided.callable, 0), Some(false));
        assert_eq!(source_consumes_argument(&provided.callable, 1), Some(true));
        let injected = member(
            &env,
            &Type::Context,
            "inject",
            &[value(), value()],
            &[
                key,
                Type::Function(
                    RoutineKind::Procedure,
                    vec![(Type::Context, false)],
                    Box::new(Type::Void),
                ),
            ],
            SPAN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(source_consumes_argument(&injected.callable, 0), Some(false));
        assert_eq!(source_consumes_argument(&injected.callable, 1), Some(true));
    }

    fn callable(kind: RoutineKind, parameters: Vec<Type>, result: Type) -> Type {
        Type::Function(
            kind,
            parameters.into_iter().map(|ty| (ty, false)).collect(),
            Box::new(result),
        )
    }

    #[test]
    fn native_callable_infers_numeric_inputs_and_widens_effects() {
        let env = environment();
        let id = header(&env, "numeric_abs").id;
        let expected = callable(
            RoutineKind::Procedure,
            vec![Type::numeric(NumberKind::I16)],
            Type::numeric(NumberKind::I16),
        );
        let converted = as_callable(&env, id, &expected, SPAN).unwrap();
        assert_eq!(converted.type_args, vec![Type::numeric(NumberKind::I16)]);
        assert_eq!(converted.contract.kind, RoutineKind::Function);
        assert_eq!(
            converted.callable_type,
            callable(
                RoutineKind::Function,
                vec![Type::numeric(NumberKind::I16)],
                Type::numeric(NumberKind::I16)
            )
        );
        assert_eq!(converted.contract.declaration, id);
    }

    #[test]
    fn native_callable_infers_return_only_generics_and_type_families() {
        let env = environment();
        let parsed = storage(
            StorageKind::Result,
            Type::Tuple(vec![
                Type::numeric(NumberKind::U8),
                storage(StorageKind::ParseError, Type::Void),
            ]),
        );
        let converted = as_callable(
            &env,
            header(&env, "numeric_parse").id,
            &callable(RoutineKind::Function, vec![Type::String], parsed.clone()),
            SPAN,
        )
        .unwrap();
        assert_eq!(converted.type_args, vec![Type::numeric(NumberKind::U8)]);
        assert_eq!(converted.contract.result, parsed);
        let empty = Type::Array(vec![(0, -1)], Box::new(Type::String));
        let converted = as_callable(
            &env,
            header(&env, "array_empty").id,
            &callable(RoutineKind::Function, vec![], empty.clone()),
            SPAN,
        )
        .unwrap();
        assert_eq!(converted.type_args, vec![Type::String]);
        assert_eq!(converted.contract.result, empty);
    }

    #[test]
    fn native_callable_derives_generic_callback_output_without_defaulting_inputs() {
        let env = environment();
        let id = header(&env, "numeric_abs").id;
        let converted = as_callable(
            &env,
            id,
            &callable(
                RoutineKind::Function,
                vec![Type::Real],
                Type::Named("Output".into()),
            ),
            SPAN,
        )
        .unwrap();
        assert_eq!(converted.contract.result, Type::Real);
        assert!(
            as_callable(
                &env,
                id,
                &callable(
                    RoutineKind::Function,
                    vec![Type::Named("Input".into())],
                    Type::Real
                ),
                SPAN
            )
            .unwrap_err()
            .message
            .contains("unresolved type")
        );
        assert!(
            as_callable(
                &env,
                id,
                &callable(RoutineKind::Function, vec![Type::Boolean], Type::Boolean),
                SPAN
            )
            .unwrap_err()
            .message
            .contains("declared bound")
        );
    }

    #[test]
    fn native_callable_checks_pack_predicates_effects_and_parameter_modes() {
        let env = environment();
        let id = header(&env, "print").id;
        let converted = as_callable(
            &env,
            id,
            &callable(
                RoutineKind::Procedure,
                vec![Type::String, Type::Integer],
                Type::Void,
            ),
            SPAN,
        )
        .unwrap();
        assert_eq!(converted.type_args, vec![Type::String, Type::Integer]);
        assert!(
            converted
                .contract
                .params
                .iter()
                .all(|parameter| parameter.passing == ParameterPassing::Read)
        );
        assert!(
            as_callable(
                &env,
                id,
                &callable(RoutineKind::Function, vec![Type::String], Type::Void),
                SPAN
            )
            .unwrap_err()
            .message
            .contains("effects")
        );
        assert!(
            as_callable(
                &env,
                id,
                &callable(
                    RoutineKind::Procedure,
                    vec![Type::Tuple(vec![Type::Integer])],
                    Type::Void
                ),
                SPAN
            )
            .unwrap_err()
            .message
            .contains("declared bound")
        );
        let by_ref = Type::Function(
            RoutineKind::Function,
            vec![(Type::Integer, true)],
            Box::new(Type::Integer),
        );
        assert!(
            as_callable(&env, header(&env, "numeric_abs").id, &by_ref, SPAN)
                .unwrap_err()
                .message
                .contains("BYREF")
        );
    }

    #[test]
    fn native_callable_rejects_dynamic_value_dependent_results() {
        let env = environment();
        let expected = callable(
            RoutineKind::Function,
            vec![storage(StorageKind::Range, Type::Integer)],
            Type::Array(vec![(0, 2)], Box::new(Type::Integer)),
        );
        assert!(
            as_callable(&env, header(&env, "array_from").id, &expected, SPAN)
                .unwrap_err()
                .message
                .contains("compile-time integer bounds")
        );
    }

    #[test]
    fn gc_member_autoderef_preserves_direct_owner_priority_and_borrow_mutability() {
        let env = environment();
        let vector = storage(StorageKind::Vec, Type::Integer);
        let gc = managed(MemoryKind::Gc, None, vector.clone());
        let length = member(&env, &gc, "len", &[], &[], SPAN).unwrap().unwrap();
        assert_eq!(length.receiver_type, vector);
        assert_eq!(
            length.callable.semantic,
            InternalSemantic::Member(MemberIntrinsic::CollectionLength)
        );
        assert_eq!(
            parameter_types(&env, &gc, "push", Span::default()),
            Some(vec![Type::Integer])
        );
        let cloned = member(&env, &gc, "clone", &[], &[], SPAN).unwrap().unwrap();
        assert_eq!(cloned.receiver_type, gc.clone());
        assert_eq!(
            cloned.callable.semantic,
            InternalSemantic::Member(MemberIntrinsic::GcClone)
        );
        let borrowed = reference(gc);
        assert!(
            member(
                &env,
                &borrowed,
                "push",
                &[integer(1)],
                &[Type::Integer],
                SPAN
            )
            .is_err()
        );
    }

    #[test]
    fn clone_bound_rejects_known_move_only_tasks_but_any_keeps_a_dynamic_check() {
        let env = environment();
        assert!(matches!(
            call(
                &env,
                "clone_value",
                &[],
                &[value()],
                &[reference(storage(StorageKind::Task, Type::Integer))]
            ),
            Err(NativeContractError::UnsatisfiedBound { .. })
        ));
        let erased = call(
            &env,
            "clone_value",
            &[],
            &[value()],
            &[reference(Type::Any)],
        )
        .unwrap();
        assert_eq!(erased.kind, RoutineKind::Procedure);
        assert_eq!(erased.result, Type::Any);
    }
}
