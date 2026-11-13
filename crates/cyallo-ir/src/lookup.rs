//! General managed locations and guarded, relative lookup plans.
use crate::intrinsic::Intrinsic;
use crate::{Diagnostic, Span, Type};
use cyallo_ast::IdentifierEq as _;

#[derive(Debug, Clone, Copy)]
pub enum Operation {
    GeneratorResume(bool),
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
            Self::Allocate
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
pub struct Method {
    pub operation: Operation,
    pub result: Type,
    pub receiver: ReceiverMode,
}
/// Resolve a member declaration on the receiver's resolved type. Receiver modes
/// are part of the signature and survive lowering as the operation's operands.
pub fn method(receiver: &Type, name: &str, args: &[Type], field: Option<&str>) -> Option<Method> {
    let owner = match receiver {
        Type::Reference { pointee, .. } => pointee.as_ref(),
        _ => receiver,
    };
    let intrinsic = match (owner, name, args) {
        (
            Type::Managed {
                kind: cyallo_ast::MemoryKind::Rc,
                ..
            },
            "clone",
            [],
        ) => Some(Intrinsic::RcClone),
        (
            Type::Managed {
                kind: cyallo_ast::MemoryKind::Rc,
                ..
            },
            "downgrade",
            [],
        ) => Some(Intrinsic::RcDowngrade),
        (
            Type::Managed {
                kind: cyallo_ast::MemoryKind::Rc,
                ..
            },
            "strong_count",
            [],
        ) => Some(Intrinsic::RcStrongCount),
        (
            Type::Managed {
                kind: cyallo_ast::MemoryKind::Rc,
                ..
            },
            "weak_count",
            [],
        ) => Some(Intrinsic::RcWeakCount),
        (
            Type::Managed {
                kind: cyallo_ast::MemoryKind::Gc,
                ..
            },
            "clone",
            [],
        ) => Some(Intrinsic::GcClone),
        (Type::Storage(cyallo_ast::StorageKind::Weak, _), "upgrade", []) => {
            Some(Intrinsic::RcUpgrade)
        }
        _ => None,
    };
    if let Some(intrinsic) = intrinsic {
        let reference = Type::Reference {
            mutable: false,
            lifetime: None,
            pointee: Box::new(owner.clone()),
        };
        let (operation, result) = call(intrinsic, &[reference])?;
        return Some(Method {
            operation,
            result,
            receiver: ReceiverMode::Owner,
        });
    }
    let (operation, result) = method_signature(receiver.pointee_or_self(), name, args, field)?;
    let receiver = match operation {
        Operation::GeneratorResume(_)
        | Operation::GeneratorClose
        | Operation::CollectionPush(_)
        | Operation::CollectionPop(_)
        | Operation::Slice(true) => ReceiverMode::Mutable,
        Operation::CollectionLength | Operation::Slice(false) | Operation::CellReplace => {
            ReceiverMode::Shared
        }
        Operation::Unwrap
        | Operation::GeneratorYielded
        | Operation::GeneratorCompleted
        | Operation::UnwrapOk
        | Operation::UnwrapErr
        | Operation::Field(_)
        | Operation::Element
        | Operation::Target => ReceiverMode::Move,
        _ => ReceiverMode::Value,
    };
    Some(Method {
        operation,
        result,
        receiver,
    })
}
pub fn parameter_types(receiver: &Type, name: &str) -> Option<Vec<Type>> {
    if name == "resume"
        && let Some((_, input, _)) = crate::generator::parts(receiver.pointee_or_self())
    {
        return Some(vec![input.clone()]);
    }
    match (receiver.pointee_or_self(), name) {
        (
            Type::Storage(
                cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                element,
            ),
            "push" | "push_back" | "push_front",
        ) => Some(vec![*element.clone()]),
        (
            Type::Storage(
                cyallo_ast::StorageKind::Cell | cyallo_ast::StorageKind::Location,
                element,
            ),
            "replace" | "store",
        ) => Some(vec![*element.clone()]),
        _ => None,
    }
}
fn method_signature(
    receiver: &Type,
    name: &str,
    args: &[Type],
    field: Option<&str>,
) -> Option<(Operation, Type)> {
    if let Some((_, input, _)) = crate::generator::parts(receiver) {
        return match (name, args) {
            ("resume", []) | ("next", []) if name == "resume" || crate::generator::unit(input) => {
                Some((
                    Operation::GeneratorResume(false),
                    crate::generator::step(receiver)?,
                ))
            }
            ("resume", [actual]) if crate::memory::same_regions(input, actual) => Some((
                Operation::GeneratorResume(true),
                crate::generator::step(receiver)?,
            )),
            ("close", []) => Some((Operation::GeneratorClose, Type::Void)),
            _ => None,
        };
    }
    if let Type::Storage(cyallo_ast::StorageKind::GeneratorStep, types) = receiver
        && let Type::Tuple(types) = types.as_ref()
        && let [yielded, returned] = types.as_slice()
    {
        return match (name, args) {
            ("is_yielded", []) => Some((Operation::GeneratorIsYielded, Type::Boolean)),
            ("is_complete", []) => Some((Operation::GeneratorIsComplete, Type::Boolean)),
            ("yielded", []) => Some((Operation::GeneratorYielded, yielded.clone())),
            ("completed", []) => Some((Operation::GeneratorCompleted, returned.clone())),
            _ => None,
        };
    }
    if let Type::Array(_, element) | Type::Slice(element) = receiver {
        return match (name, args) {
            ("len", []) => Some((
                Operation::CollectionLength,
                Type::numeric(cyallo_ast::NumberKind::Usize),
            )),
            ("as_slice", []) | ("as_mut_slice", []) => Some((
                Operation::Slice(name == "as_mut_slice"),
                Type::Reference {
                    mutable: name == "as_mut_slice",
                    lifetime: None,
                    pointee: Box::new(Type::Slice(element.clone())),
                },
            )),
            _ => None,
        };
    }
    let Type::Storage(kind, t) = receiver else {
        return None;
    };
    if *kind == cyallo_ast::StorageKind::Result {
        let Type::Tuple(types) = t.as_ref() else {
            return None;
        };
        let [value, error] = types.as_slice() else {
            return None;
        };
        return match (name, args) {
            ("is_ok", []) => Some((Operation::IsOk, Type::Boolean)),
            ("is_err", []) => Some((Operation::IsErr, Type::Boolean)),
            ("unwrap", []) => Some((Operation::UnwrapOk, value.clone())),
            ("unwrap_err", []) => Some((Operation::UnwrapErr, error.clone())),
            _ => None,
        };
    }
    if matches!(
        *kind,
        cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque
    ) {
        return match (name, args) {
            ("as_slice" | "as_mut_slice", []) if *kind == cyallo_ast::StorageKind::Vec => Some((
                Operation::Slice(name == "as_mut_slice"),
                Type::Reference {
                    mutable: name == "as_mut_slice",
                    lifetime: None,
                    pointee: Box::new(Type::Slice(t.clone())),
                },
            )),
            ("push" | "push_back", [value]) if crate::memory::same_regions(value, t) => {
                Some((Operation::CollectionPush(false), Type::Void))
            }
            ("push_front", [value])
                if *kind == cyallo_ast::StorageKind::VecDeque
                    && crate::memory::same_regions(value, t) =>
            {
                Some((Operation::CollectionPush(true), Type::Void))
            }
            ("pop" | "pop_back", []) => Some((
                Operation::CollectionPop(false),
                storage(cyallo_ast::StorageKind::Option, *t.clone()),
            )),
            ("pop_front", []) if *kind == cyallo_ast::StorageKind::VecDeque => Some((
                Operation::CollectionPop(true),
                storage(cyallo_ast::StorageKind::Option, *t.clone()),
            )),
            ("len", []) => Some((
                Operation::CollectionLength,
                Type::numeric(cyallo_ast::NumberKind::Usize),
            )),
            _ => None,
        };
    }
    match (*kind, name, args) {
        (cyallo_ast::StorageKind::Layout, "size", []) => Some((
            Operation::LayoutSize,
            Type::numeric(cyallo_ast::NumberKind::Usize),
        )),
        (cyallo_ast::StorageKind::Layout, "align", []) => Some((
            Operation::LayoutAlign,
            Type::numeric(cyallo_ast::NumberKind::Usize),
        )),
        (cyallo_ast::StorageKind::Location, "load", []) => {
            Some((Operation::LocationLoad, *t.clone()))
        }
        (cyallo_ast::StorageKind::Cell, "replace", [value])
            if crate::memory::same_regions(value, t) =>
        {
            Some((Operation::CellReplace, *t.clone()))
        }
        (cyallo_ast::StorageKind::Location, "store", [value])
            if crate::memory::same_regions(value, t) =>
        {
            Some((Operation::LocationStore, Type::Void))
        }
        (cyallo_ast::StorageKind::Option, "is_some", []) => {
            Some((Operation::IsSome, Type::Boolean))
        }
        (cyallo_ast::StorageKind::Option, "is_none", []) => {
            Some((Operation::IsNone, Type::Boolean))
        }
        (cyallo_ast::StorageKind::Option, "unwrap", [])
        | (cyallo_ast::StorageKind::Option, "or_fail", [Type::String]) => {
            Some((Operation::Unwrap, *t.clone()))
        }
        (cyallo_ast::StorageKind::StoragePath, "field", [Type::String]) => {
            let Type::Record(_, fields) = t.as_ref() else {
                return None;
            };
            let field = field?;
            let index = fields.iter().position(|(n, _)| n.eq_identifier(field))?;
            Some((
                Operation::Field(index),
                storage(
                    cyallo_ast::StorageKind::StoragePath,
                    fields[index].1.clone(),
                ),
            ))
        }
        (cyallo_ast::StorageKind::StoragePath, "element", [index])
            if index.number_kind().is_some_and(|kind| !kind.float()) =>
        {
            let element = match t.as_ref() {
                Type::Array(_, element)
                | Type::Storage(
                    cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                    element,
                ) => element,
                _ => return None,
            };
            Some((
                Operation::Element,
                storage(cyallo_ast::StorageKind::StoragePath, *element.clone()),
            ))
        }
        (cyallo_ast::StorageKind::StoragePath, "target", []) => {
            let target = match t.as_ref() {
                Type::Managed { pointee, .. } => pointee,
                Type::Storage(cyallo_ast::StorageKind::Cell, pointee) => pointee,
                _ => return None,
            };
            Some((
                Operation::Target,
                storage(cyallo_ast::StorageKind::StoragePath, *target.clone()),
            ))
        }
        _ => None,
    }
}
pub fn error(span: Span) -> Diagnostic {
    Diagnostic::new(
        "E150",
        "invalid lookup operation, mapper signature, or argument types",
        span,
    )
}
