//! General managed locations and guarded, relative lookup plans.
use crate::intrinsic::Intrinsic;
use crate::{Diagnostic, Span, Type};

/// What a resumed generator receives as the value of its suspended `yield`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeInput {
    /// The resume type is `()`.
    Unit,
    /// The value passed to `resume(value)`, the call's second operand.
    Value,
}
#[derive(Debug, Clone, Copy)]
pub enum Operation {
    ObjectView,
    ObjectFreeze,
    ViewErrorMessage,
    ModuleLoad,
    LoadErrorNotFound,
    LoadErrorDenied,
    LoadErrorCheck,
    LoadErrorMessage,
    NativeCallable(cyallo_ast::DeclarationId),
    Concurrent(crate::concurrent::Operation),
    /// `start()`: run a new generator to its first suspension.
    GeneratorStart,
    /// `resume(…)`: continue a started generator.
    GeneratorResume(ResumeInput),
    /// `next()` on a `()`-resume generator: start it if new, otherwise resume.
    GeneratorNext,
    GeneratorClose,
    GeneratorIsComplete,
    GeneratorIsYielded,
    GeneratorYielded,
    GeneratorCompleted,
    LayoutNew,
    LayoutForRc,
    LayoutSize,
    LayoutAlign,
    Allocate,
    Deallocate,
    AllocErrorNew,
    ResultNew(bool),
    RcNewIn {
        deallocate: usize,
        borrowed: bool,
        fallible: bool,
    },
    ManagedNew(cyallo_ast::MemoryKind, bool),
    RcClone,
    RcDowngrade,
    RcUpgrade,
    RcStrongCount,
    RcWeakCount,
    GcClone,
    DropValue,
    CloneValue,
    Parse(cyallo_ast::NumberKind),
    IsOk,
    IsErr,
    UnwrapOk,
    UnwrapErr,
    Slice(bool),
    CollectionLiteral(bool),
    CollectionFrom(bool),
    CollectionPush(bool),
    CollectionPop(bool),
    CollectionLength,
    Range(bool),
    /// `range.rev()`: the same elements in descending order.
    RangeReverse,
    ArrayFrom,
    CellNew,
    Empty,
    CellRead,
    CellWrite,
    CellReplace,
    LocationLoad,
    LocationStore,
    Some,
    None,
    IsSome,
    IsNone,
    Unwrap,
    Erase,
    Checked,
    Symbol,
    StoragePathNew,
    Field(usize),
    Element,
    Target,
    Missing,
    Mapped,
}
impl Operation {
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::ObjectView
                | Self::ObjectFreeze
                | Self::ModuleLoad
                | Self::Concurrent(_)
                | Self::Allocate
                | Self::Deallocate
                | Self::RcNewIn { .. }
                | Self::CellWrite
                | Self::CellReplace
                | Self::LocationStore
                | Self::Symbol
                | Self::CollectionPush(_)
                | Self::CollectionPop(_)
        )
    }
}
pub fn storage(name: cyallo_ast::StorageKind, inner: Type) -> Type {
    Type::Storage(name, Box::new(inner))
}
pub fn result_type(value: Type, error: Type) -> Type {
    storage(
        cyallo_ast::StorageKind::Result,
        Type::Tuple(vec![value, error]),
    )
}
/// The payload of an `Option` type, whichever form the type takes.
pub fn option_payload(ty: &Type) -> Option<&Type> {
    use cyallo_ast::EnumEncoding::OptionSome;
    match ty {
        Type::Storage(cyallo_ast::StorageKind::Option, t) => Some(t),
        Type::Enum(contract) => contract
            .variants
            .iter()
            .find(|v| v.encoding == OptionSome)
            .and_then(|v| v.payload.as_ref()),
        _ => None,
    }
}
/// The value and error payloads of a `Result` type, whichever form the type takes.
pub fn result_payload(ty: &Type) -> Option<(&Type, &Type)> {
    use cyallo_ast::EnumEncoding::{ResultErr, ResultOk};
    match ty {
        Type::Storage(cyallo_ast::StorageKind::Result, tuple) => match tuple.as_ref() {
            Type::Tuple(types) => match types.as_slice() {
                [value, error] => Some((value, error)),
                _ => None,
            },
            _ => None,
        },
        Type::Enum(contract) => {
            let payload = |role| {
                contract
                    .variants
                    .iter()
                    .find(|v| v.encoding == role)
                    .and_then(|v| v.payload.as_ref())
            };
            Some((payload(ResultOk)?, payload(ResultErr)?))
        }
        _ => None,
    }
}
fn role_payload(ty: &Type, role: cyallo_ast::EnumEncoding) -> Option<&Type> {
    let Type::Enum(contract) = ty else {
        return None;
    };
    contract
        .variants
        .iter()
        .find(|v| v.encoding == role)
        .and_then(|v| v.payload.as_ref())
}
/// The yielded and completed payloads of a `GeneratorStep` type, in either form.
pub fn step_payload(ty: &Type) -> Option<(&Type, &Type)> {
    use cyallo_ast::EnumEncoding::{StepComplete, StepYielded};
    match ty {
        Type::Storage(cyallo_ast::StorageKind::GeneratorStep, tuple) => match tuple.as_ref() {
            Type::Tuple(types) => match types.as_slice() {
                [yielded, completed] => Some((yielded, completed)),
                _ => None,
            },
            _ => None,
        },
        _ => Some((role_payload(ty, StepYielded)?, role_payload(ty, StepComplete)?)),
    }
}
/// The unsent value type of a `SendError` type, in either form.
pub fn send_payload(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Storage(cyallo_ast::StorageKind::SendError, t) => Some(t),
        _ => role_payload(ty, cyallo_ast::EnumEncoding::SendClosed),
    }
}
pub fn inner(ty: &Type, name: cyallo_ast::StorageKind) -> Option<&Type> {
    match ty {
        Type::Storage(n, t) if *n == name => Some(t),
        _ => None,
    }
}
pub fn constructor(name: Intrinsic, ty: &Type, args: &[Type]) -> Option<(Operation, Type)> {
    let (op, result, valid) = match name {
        // The weak type is the declared result; the argument must be an `rc` owner.
        Intrinsic::RcDowngrade => (
            Operation::RcDowngrade,
            ty.clone(),
            matches!(
                (ty, args),
                (
                    Type::Opaque(_, owner, traits),
                    [owner_argument]
                ) if traits.weak()
                    && matches!(owner.as_slice(), [Type::Managed { kind: cyallo_ast::MemoryKind::Rc, .. }])
                    && matches!(
                        owner_argument.pointee_or_self(),
                        Type::Managed { kind: cyallo_ast::MemoryKind::Rc, .. }
                    )
            ),
        ),
        // `Vec::from(range)`: the collection is the declared result.
        Intrinsic::VecFrom | Intrinsic::DequeFrom => (
            Operation::CollectionFrom(name == Intrinsic::DequeFrom),
            ty.clone(),
            matches!(
                (ty, args),
                (Type::Opaque(_, applied, traits), [Type::Storage(cyallo_ast::StorageKind::Range, range)])
                    if traits.collection()
                        && traits.double_ended() == (name == Intrinsic::DequeFrom)
                        && matches!(applied.as_slice(), [element] if crate::memory::same_regions(element, range))
            ),
        ),
        // A cell is the declared result, applied to the type of its contents.
        Intrinsic::CellNew => (
            Operation::CellNew,
            ty.clone(),
            matches!(
                (ty, args),
                (Type::Opaque(_, applied, traits), [actual])
                    if traits.deref()
                        && matches!(applied.as_slice(), [element] if crate::memory::same_regions(actual, element))
            ),
        ),
        Intrinsic::ArrayEmpty => (
            Operation::Empty,
            Type::Array(vec![(0, -1)], Box::new(ty.clone())),
            args.is_empty(),
        ),
        Intrinsic::OptionNone => (
            Operation::None,
            storage(cyallo_ast::StorageKind::Option, ty.clone()),
            args.is_empty(),
        ),
        Intrinsic::MissingPath => (
            Operation::Missing,
            ty.clone(),
            args.is_empty() && matches!(ty, Type::Opaque(_, applied, traits) if traits.path() && applied.len() == 1),
        ),
        // `path(value)`: the declared path type applied to the value's type.
        Intrinsic::LookupPath => (
            Operation::StoragePathNew,
            ty.clone(),
            matches!(
                (ty, args),
                (Type::Opaque(_, applied, traits), [actual])
                    if traits.path()
                        && matches!(applied.as_slice(), [element] if crate::memory::same_regions(actual, element))
            ),
        ),
        Intrinsic::Checked => (Operation::Checked, ty.clone(), args == [Type::Any]),
        _ => return None,
    };
    valid.then_some((op, result))
}
pub fn call(name: Intrinsic, args: &[Type]) -> Option<(Operation, Type)> {
    if name == Intrinsic::CloneValue
        && args
            .first()
            .is_some_and(|ty| crate::generator::parts(ty.pointee_or_self()).is_some())
    {
        return None;
    }
    if let [argument] = args {
        if name == Intrinsic::CloneValue
            && let Type::Reference { pointee, .. } = argument
            && !crate::memory::cloneable(pointee)
        {
            return None;
        }
        if name == Intrinsic::CloneValue
            && let Type::Reference { pointee, .. } = argument
        {
            return Some((Operation::CloneValue, *pointee.clone()));
        }
        let owner = argument.pointee_or_self();
        match (name, owner) {
            (Intrinsic::DropValue, _) => return Some((Operation::DropValue, Type::Void)),
            (
                Intrinsic::GcClone,
                Type::Managed {
                    kind: cyallo_ast::MemoryKind::Gc,
                    ..
                },
            ) => return Some((Operation::GcClone, owner.clone())),
            (
                Intrinsic::RcClone,
                Type::Managed {
                    kind: cyallo_ast::MemoryKind::Rc,
                    ..
                },
            ) => return Some((Operation::RcClone, owner.clone())),
            (Intrinsic::RcUpgrade, Type::Opaque(_, owner, traits))
                if traits.weak()
                    && matches!(
                        owner.as_slice(),
                        [Type::Managed {
                            kind: cyallo_ast::MemoryKind::Rc,
                            ..
                        }]
                    ) =>
            {
                return Some((
                    Operation::RcUpgrade,
                    storage(cyallo_ast::StorageKind::Option, owner[0].clone()),
                ));
            }
            (
                Intrinsic::RcStrongCount | Intrinsic::RcWeakCount,
                Type::Managed {
                    kind: cyallo_ast::MemoryKind::Rc,
                    ..
                },
            ) => {
                return Some((
                    if name == Intrinsic::RcStrongCount {
                        Operation::RcStrongCount
                    } else {
                        Operation::RcWeakCount
                    },
                    Type::numeric(cyallo_ast::NumberKind::Usize),
                ));
            }
            _ => {}
        }
    }
    match (name, args) {
        (Intrinsic::OptionSome, [t]) => Some((
            Operation::Some,
            storage(cyallo_ast::StorageKind::Option, t.clone()),
        )),
        (Intrinsic::Erase, [_]) => Some((Operation::Erase, Type::Any)),
        (Intrinsic::FreshSymbol, [Type::String]) => Some((Operation::Symbol, Type::Symbol)),
        (Intrinsic::MappedLookup, [receiver, key, Type::Function(_, params, result)]) if matches!(params.as_slice(), [(a, false), (b, false)] if crate::memory::same_regions(a, receiver) && crate::memory::same_regions(b, key)) => {
            match result.as_ref() {
                Type::Opaque(_, applied, traits) if traits.path() && applied.len() == 1 => Some((
                    Operation::Mapped,
                    storage(cyallo_ast::StorageKind::Option, applied[0].clone()),
                )),
                _ => None,
            }
        }
        _ => None,
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverMode {
    Value,
    Shared,
    Mutable,
    Owner,
    Move,
}
pub fn error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E150",
        "invalid lookup operation, mapper signature, or argument types",
        span,
    )
}
