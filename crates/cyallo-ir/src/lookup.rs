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
pub fn inner(ty: &Type, name: cyallo_ast::StorageKind) -> Option<&Type> {
    match ty {
        Type::Storage(n, t) if *n == name => Some(t),
        _ => None,
    }
}
pub fn constructor(name: Intrinsic, ty: &Type, args: &[Type]) -> Option<(Operation, Type)> {
    let (op, result, valid) = match name {
        Intrinsic::VecLiteral | Intrinsic::DequeLiteral => (
            Operation::CollectionLiteral(name == Intrinsic::DequeLiteral),
            storage(
                if name == Intrinsic::DequeLiteral {
                    cyallo_ast::StorageKind::VecDeque
                } else {
                    cyallo_ast::StorageKind::Vec
                },
                ty.clone(),
            ),
            args.iter().all(|t| crate::memory::same_regions(t, ty)),
        ),
        Intrinsic::CellNew => (
            Operation::CellNew,
            storage(cyallo_ast::StorageKind::Cell, ty.clone()),
            matches!(args, [actual] if crate::memory::same_regions(actual, ty)),
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
            storage(cyallo_ast::StorageKind::StoragePath, ty.clone()),
            args.is_empty(),
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
            (
                Intrinsic::RcDowngrade,
                Type::Managed {
                    kind: cyallo_ast::MemoryKind::Rc,
                    ..
                },
            ) => {
                return Some((
                    Operation::RcDowngrade,
                    storage(cyallo_ast::StorageKind::Weak, owner.clone()),
                ));
            }
            (Intrinsic::RcUpgrade, Type::Storage(cyallo_ast::StorageKind::Weak, owner))
                if matches!(
                    owner.as_ref(),
                    Type::Managed {
                        kind: cyallo_ast::MemoryKind::Rc,
                        ..
                    }
                ) =>
            {
                return Some((
                    Operation::RcUpgrade,
                    storage(cyallo_ast::StorageKind::Option, *owner.clone()),
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
        (Intrinsic::VecLiteral | Intrinsic::DequeLiteral, [first, rest @ ..])
            if rest.iter().all(|t| crate::memory::same_regions(t, first)) =>
        {
            Some((
                Operation::CollectionLiteral(name == Intrinsic::DequeLiteral),
                storage(
                    if name == Intrinsic::DequeLiteral {
                        cyallo_ast::StorageKind::VecDeque
                    } else {
                        cyallo_ast::StorageKind::Vec
                    },
                    first.clone(),
                ),
            ))
        }
        (Intrinsic::VecFrom | Intrinsic::DequeFrom, [Type::Storage(kind, t)])
            if *kind == cyallo_ast::StorageKind::Range =>
        {
            Some((
                Operation::CollectionFrom(name == Intrinsic::DequeFrom),
                storage(
                    if name == Intrinsic::DequeFrom {
                        cyallo_ast::StorageKind::VecDeque
                    } else {
                        cyallo_ast::StorageKind::Vec
                    },
                    *t.clone(),
                ),
            ))
        }
        (Intrinsic::CellNew, [t]) => Some((
            Operation::CellNew,
            storage(cyallo_ast::StorageKind::Cell, t.clone()),
        )),
        (Intrinsic::OptionSome, [t]) => Some((
            Operation::Some,
            storage(cyallo_ast::StorageKind::Option, t.clone()),
        )),
        (Intrinsic::Erase, [_]) => Some((Operation::Erase, Type::Any)),
        (Intrinsic::FreshSymbol, [Type::String]) => Some((Operation::Symbol, Type::Symbol)),
        (Intrinsic::LookupPath, [t]) => Some((
            Operation::StoragePathNew,
            storage(cyallo_ast::StorageKind::StoragePath, t.clone()),
        )),
        (Intrinsic::MappedLookup, [receiver, key, Type::Function(_, params, result)]) if matches!(params.as_slice(), [(a, false), (b, false)] if crate::memory::same_regions(a, receiver) && crate::memory::same_regions(b, key)) => {
            inner(result, cyallo_ast::StorageKind::StoragePath).map(|t| {
                (
                    Operation::Mapped,
                    storage(cyallo_ast::StorageKind::Option, t.clone()),
                )
            })
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
