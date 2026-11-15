//! Safe allocation declarations and their checked contracts.
use crate::{
    intrinsic::Intrinsic,
    lookup::{self, Operation},
};
use cyallo_ast::*;
pub fn layout() -> Type {
    Type::Storage(StorageKind::Layout, Box::new(Type::Void))
}
pub fn allocation() -> Type {
    Type::Storage(StorageKind::Allocation, Box::new(Type::Void))
}
pub fn error() -> Type {
    Type::Storage(StorageKind::AllocError, Box::new(Type::Void))
}
pub fn has_borrow(ty: &Type) -> bool {
    match ty {
        Type::Reference { .. } | Type::Borrowed(_) | Type::Any | Type::TraitObject(_) => true,
        Type::Managed {
            lifetime, pointee, ..
        } => lifetime.is_some() || has_borrow(pointee),
        Type::Storage(_, inner)
        | Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner) => has_borrow(inner),
        Type::Record(_, fields) => fields.iter().any(|(_, ty)| has_borrow(ty)),
        Type::Tuple(types) => types.iter().any(has_borrow),
        _ => false,
    }
}
pub fn managed_type(
    kind: MemoryKind,
    locality: Locality,
    allocator: Option<&Type>,
    value: Type,
) -> Type {
    Type::Managed {
        kind,
        locality,
        lifetime: allocator.filter(|ty| has_borrow(ty)).map(|_| "_".into()),
        pointee: Box::new(value),
    }
}
pub fn cloneable(ty: &Type) -> bool {
    match ty {
        Type::Storage(
            StorageKind::Allocation
            | StorageKind::Task
            | StorageKind::Generator
            | StorageKind::PureGenerator,
            _,
        ) => false,
        Type::Storage(_, ty) | Type::Array(_, ty) | Type::SparseArray(ty) | Type::Slice(ty) => {
            cloneable(ty)
        }
        Type::Record(_, fields) => fields.iter().all(|(_, ty)| cloneable(ty)),
        Type::Tuple(types) => types.iter().all(cloneable),
        _ => true,
    }
}
pub fn resolve(id: Intrinsic, types: &[Type], arguments: &[Type]) -> Option<(Operation, Type)> {
    use Intrinsic::*;
    let (operation, result) = match (id, types, arguments) {
        (LayoutNew, [], [a, b]) if a == &Type::numeric(NumberKind::Usize) && b == a => {
            (Operation::LayoutNew, lookup::result_type(layout(), error()))
        }
        (Allocate, [], [ty]) if ty == &layout() => (
            Operation::Allocate,
            lookup::result_type(allocation(), error()),
        ),
        (Deallocate, [], [ty]) if ty == &allocation() => (Operation::Deallocate, Type::Void),
        (AllocErrorNew, [], [Type::String]) => (Operation::AllocErrorNew, error()),
        (ResultOk, [value, error], [actual]) if same_regions(value, actual) => (
            Operation::ResultNew(true),
            lookup::result_type(value.clone(), error.clone()),
        ),
        (ResultErr, [value, error], [actual]) if same_regions(error, actual) => (
            Operation::ResultNew(false),
            lookup::result_type(value.clone(), error.clone()),
        ),
        _ => return None,
    };
    Some((operation, result))
}
/// Region names are local binders; owning allocation types must still retain
/// whether their allocator is borrowed. This comparison never erases that fact.
pub fn same_regions(left: &Type, right: &Type) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Type::RecordRef(a) | Type::Record(a, _), Type::RecordRef(b) | Type::Record(b, _)) => {
            a == b
        }
        (
            Type::Managed {
                kind: a,
                locality: c,
                lifetime: x,
                pointee: p,
            },
            Type::Managed {
                kind: b,
                locality: d,
                lifetime: y,
                pointee: q,
            },
        ) => a == b && c == d && x.is_some() == y.is_some() && same_regions(p, q),
        (
            Type::Reference {
                mutable: a,
                pointee: p,
                ..
            },
            Type::Reference {
                mutable: b,
                pointee: q,
                ..
            },
        ) => a == b && same_regions(p, q),
        (Type::Storage(a, p), Type::Storage(b, q)) => a == b && same_regions(p, q),
        (Type::Array(a, p), Type::Array(b, q)) => a == b && same_regions(p, q),
        (Type::SparseArray(p), Type::SparseArray(q))
        | (Type::Slice(p), Type::Slice(q))
        | (Type::Borrowed(p), Type::Borrowed(q))
        | (Type::ServiceKey(p), Type::ServiceKey(q))
        | (Type::ServiceRef(p), Type::ServiceRef(q)) => same_regions(p, q),
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_regions(a, b))
        }
        (Type::Function(a, p, r), Type::Function(b, q, s)) => {
            a == b
                && p.len() == q.len()
                && p.iter()
                    .zip(q)
                    .all(|((p, a), (q, b))| a == b && same_regions(p, q))
                && same_regions(r, s)
        }
        _ => false,
    }
}
