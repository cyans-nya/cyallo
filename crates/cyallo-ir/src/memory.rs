//! Safe allocation declarations and their checked contracts.
use crate::{
    intrinsic::Intrinsic,
    lookup::{self, Operation},
};
use cyallo_ast::*;
fn layout_type(ty: &Type) -> bool {
    matches!(ty, Type::Opaque(_, args, traits) if traits.layout() && args.is_empty())
}
fn allocation_type(ty: &Type) -> bool {
    matches!(ty, Type::Opaque(_, args, traits) if traits.allocation() && args.is_empty())
}
/// `Result<Layout, AllocError>` / `Result<Allocation, AllocError>`, with the opaque
/// type taken from the declaration.
fn allocator_result(declared: &Type, wanted: fn(&Type) -> bool) -> bool {
    matches!(
        crate::lookup::result_payload(declared),
        Some((value, error)) if wanted(value) && *error == error_type()
    )
}
pub fn allocator_result_layout(declared: &Type) -> bool {
    allocator_result(declared, layout_type)
}
pub fn allocator_result_allocation(declared: &Type) -> bool {
    allocator_result(declared, allocation_type)
}
fn error_type() -> Type {
    Type::Storage(StorageKind::AllocError, Box::new(Type::Void))
}
pub fn error() -> Type {
    Type::Storage(StorageKind::AllocError, Box::new(Type::Void))
}
pub fn has_borrow(ty: &Type) -> bool {
    match ty {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .any(has_borrow),
        Type::Reference { .. } | Type::Borrowed(_) | Type::Any | Type::TraitObject(_) => true,
        Type::Managed {
            lifetime, pointee, ..
        } => lifetime.is_some() || has_borrow(pointee),
        Type::Storage(_, inner)
        | Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner) => has_borrow(inner),
        Type::Record(_, fields) | Type::AnonymousRecord(fields) => {
            fields.iter().any(|(_, ty)| has_borrow(ty))
        }
        Type::Tuple(types) | Type::Opaque(_, types, _) => types.iter().any(has_borrow),
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
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .all(cloneable),
        Type::Storage(_, ty) | Type::Array(_, ty) | Type::SparseArray(ty) | Type::Slice(ty) => {
            cloneable(ty)
        }
        Type::Record(_, fields) | Type::AnonymousRecord(fields) => {
            fields.iter().all(|(_, ty)| cloneable(ty))
        }
        Type::Tuple(types) => types.iter().all(cloneable),
        Type::Opaque(_, arguments, traits) => traits.cloneable() && arguments.iter().all(cloneable),
        _ => true,
    }
}
pub fn resolve(id: Intrinsic, types: &[Type], arguments: &[Type]) -> Option<(Operation, Type)> {
    use Intrinsic::*;
    let (operation, result) = match (id, types, arguments) {
        // The layout and allocation types are the declared ones; the contract only
        // checks their role in the allocator protocol.
        (LayoutNew, [declared], [a, b])
            if a == &Type::numeric(NumberKind::Usize)
                && b == a
                && allocator_result_layout(declared) =>
        {
            (Operation::LayoutNew, declared.clone())
        }
        (Allocate, [declared], [ty]) if layout_type(ty) && allocator_result_allocation(declared) => {
            (Operation::Allocate, declared.clone())
        }
        (Deallocate, [], [ty]) if allocation_type(ty) => (Operation::Deallocate, Type::Void),
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
    // The marker and the declared enum denote the same core type.
    if matches!(left, Type::Storage(..)) != matches!(right, Type::Storage(..))
        && let (Some((a, p)), Some((b, q))) =
            (crate::enums::core_shape(left), crate::enums::core_shape(right))
    {
        return a == b && p.len() == q.len() && p.iter().zip(&q).all(|(p, q)| same_regions(p, q));
    }
    fn identity(ty: &Type) -> Option<&EnumIdentity> {
        match ty {
            Type::Enum(contract) => Some(&contract.identity),
            Type::EnumRef(identity, _) => Some(identity),
            _ => None,
        }
    }
    if let (Some(a), Some(b)) = (identity(left), identity(right)) {
        return a.declaration == b.declaration
            && a.arguments.len() == b.arguments.len()
            && a.arguments
                .iter()
                .zip(&b.arguments)
                .all(|(a, b)| same_regions(a, b));
    }
    match (left, right) {
        (Type::AnonymousRecord(a), Type::AnonymousRecord(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((an, a), (bn, b))| an == bn && same_regions(a, b))
        }
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
        (Type::Opaque(a, p, x), Type::Opaque(b, q, y)) => {
            a == b
                && x == y
                && p.len() == q.len()
                && p.iter().zip(q).all(|(p, q)| same_regions(p, q))
        }
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
