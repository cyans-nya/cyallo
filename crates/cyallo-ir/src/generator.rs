//! Generator types and lexical suspension lowering are independent of task scheduling.
use cyallo_ast::{Function, RoutineKind, StorageKind, Type};

pub fn parts(ty: &Type) -> Option<(&Type, &Type, &Type)> {
    // A generator is any opaque type its provider declared with the `generator`
    // trait; the language knows the trait, not a name.
    let Type::Opaque(_, types, traits) = ty else {
        return None;
    };
    let [yielded, resumed, returned] = types.as_slice() else {
        return None;
    };
    traits.generator().then_some((yielded, resumed, returned))
}
pub fn factory_type(function: &Function) -> Type {
    let mut ty = function.result.clone();
    if function.generator
        && function.kind == RoutineKind::Function
        && let Type::Opaque(_, _, traits) = &mut ty
        && traits.generator()
    {
        *traits = traits.with_pure(true);
    }
    ty
}
pub fn result(function: &Function) -> Type {
    if function.generator
        && let Some((_, _, returned)) = parts(&function.result)
    {
        returned.clone()
    } else {
        function.result.clone()
    }
}
pub fn step(ty: &Type) -> Option<Type> {
    let (yielded, _, returned) = parts(ty)?;
    Some(Type::Storage(
        StorageKind::GeneratorStep,
        Box::new(Type::Tuple(vec![yielded.clone(), returned.clone()])),
    ))
}
pub fn unit(ty: &Type) -> bool {
    matches!(ty, Type::Void) || matches!(ty, Type::Tuple(ts) if ts.is_empty())
}
pub fn borrowed(ty: &Type) -> bool {
    match ty {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .any(borrowed),
        Type::Reference { .. } | Type::Borrowed(_) | Type::Slice(_) => true,
        Type::Managed {
            lifetime: Some(_), ..
        } => true,
        Type::Tuple(ts) => ts.iter().any(borrowed),
        Type::Record(_, fields) | Type::AnonymousRecord(fields) => {
            fields.iter().any(|(_, ty)| borrowed(ty))
        }
        Type::Storage(_, inner) | Type::Array(_, inner) | Type::SparseArray(inner) => {
            borrowed(inner)
        }
        Type::Opaque(_, ts, _) => ts.iter().any(borrowed),
        _ => false,
    }
}
pub const INPUT: &str = "$generator-input";
pub fn input_local(function: &Function, locals: &mut Vec<crate::ir::Local>) {
    if function.generator
        && let Some((_, input, _)) = parts(&function.result)
    {
        locals.push(crate::ir::Local {
            name: INPUT.into(),
            display_name: None,
            ty: input.clone(),
            constant: true,
            span: function.span,
        });
    }
}
