//! General object/value operations. Module namespaces use these operations too.
use cyallo_ast::{ObjectKind, Operator, Type};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallableTarget {
    Native(cyallo_ast::DeclarationId),
    Source(Box<str>),
}
#[derive(Debug, Clone)]
pub enum Operation {
    TypeDescriptor(cyallo_ast::ExportedType),
    DeferredCallable(Box<str>),
    Object(ObjectKind, Box<[Box<str>]>),
    Get(Box<str>),
    GetKey,
    Set(Box<str>),
    SetKey,
    Delete(Box<str>),
    DeleteKey,
    Erase,
    Check(Type),
    View,
    Freeze,
    Invoke,
    Unary(Operator),
    Binary(Operator),
}
pub fn owned(ty: &Type) -> bool {
    let mut borrowed = false;
    cyallo_ast::visit::visit_type(&mut ty.clone(), &mut |ty| {
        borrowed |= matches!(
            ty,
            Type::Any
                | Type::Reference { .. }
                | Type::Borrowed(_)
                | Type::Managed {
                    lifetime: Some(_),
                    ..
                }
        )
    });
    !borrowed
}
pub fn object(ty: &Type) -> bool {
    matches!(ty, Type::Dynamic | Type::Object(_))
}
