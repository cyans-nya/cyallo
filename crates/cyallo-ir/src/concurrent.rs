//! Contracts of standard concurrency declarations. Later passes use Operation.
use crate::{
    intrinsic::{Intrinsic, Lowering, ResolvedCall},
    lookup::{self},
};
use cyallo_ast::{Diagnostic, NumberKind, Span, StorageKind as K, Type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Bounded,
    Unbounded,
    Clone,
    Close,
    IsClosed,
    Send,
    TrySend,
    Receive,
    TryReceive,
    SendErrorValue,
    SendErrorClosed,
    SendErrorFull,
    SendErrorLimit,
    ReceiveErrorClosed,
    ReceiveErrorEmpty,
    Clock,
    Sleep,
    Spawn,
    Join,
    Cancel,
    IsFinished,
    Completed,
    IsCompleted,
    IsFailed,
    IsCancelled,
    IsStopped,
}
pub fn transferable(ty: &Type) -> bool {
    match ty {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .all(transferable),
        Type::Numeric(_)
        | Type::Integer
        | Type::Real
        | Type::Boolean
        | Type::Char
        | Type::String
        | Type::Void => true,
        Type::Array(_, t) | Type::SparseArray(t) => transferable(t),
        Type::Tuple(ts) => ts.iter().all(transferable),
        Type::Record(_, fs) | Type::AnonymousRecord(fs) => fs.iter().all(|(_, t)| transferable(t)),
        Type::Opaque(_, arguments, traits) => {
            traits.transferable() && arguments.iter().all(transferable)
        }
        Type::Storage(K::Option | K::Result | K::GeneratorStep | K::SendError, t) => {
            transferable(t)
        }
        _ => false,
    }
}
/// The declared result must be an opaque host type applied to exactly `arguments`:
/// the declaration names the type, the contract still constrains its shape.
fn opaque_with(declared: &Type, arguments: &[Type]) -> bool {
    matches!(
        declared,
        Type::Opaque(_, applied, _)
            if applied.len() == arguments.len()
                && applied
                    .iter()
                    .zip(arguments)
                    .all(|(a, b)| crate::declarations::equivalent_types(a, b))
    )
}
/// A channel constructor returns a sender and a receiver of the element type.
fn channel_pair(declared: &Type, element: &Type) -> bool {
    matches!(
        declared,
        Type::Tuple(ends)
            if ends.len() == 2
                && ends.iter().all(|end| opaque_with(end, std::slice::from_ref(element)))
    )
}
pub fn is_intrinsic(id: Intrinsic) -> bool {
    matches!(
        id,
        Intrinsic::ChannelBounded
            | Intrinsic::ChannelUnbounded
            | Intrinsic::ClockNew
            | Intrinsic::TaskSpawn
    )
}
pub fn resolve(
    id: Intrinsic,
    types: &[Type],
    args: &[Type],
    span: Span,
) -> Result<ResolvedCall, Diagnostic> {
    // The sender/receiver pair is the declared result: the declaration names the
    // types, the element type only has to be transferable.
    let (op, result) = match (id, types, args) {
        (Intrinsic::ChannelBounded, [t, declared], [n])
            if transferable(t)
                && channel_pair(declared, t)
                && n.number_kind() == Some(NumberKind::Usize) =>
        {
            (Operation::Bounded, declared.clone())
        }
        (Intrinsic::ChannelUnbounded, [t, declared], [])
            if transferable(t) && channel_pair(declared, t) =>
        {
            (Operation::Unbounded, declared.clone())
        }
        // The result is the declared type: the declaration, not a kind, names it.
        (Intrinsic::ClockNew, [declared], []) if opaque_with(declared, &[]) => {
            (Operation::Clock, declared.clone())
        }
        (Intrinsic::TaskSpawn, [declared], [callable])
            if cyallo_ast::callable(callable).is_some_and(|(_, p, r)| {
                p.is_empty() && transferable(r) && opaque_with(declared, std::slice::from_ref(r))
            }) =>
        {
            // The handle type is the declared result.
            (Operation::Spawn, declared.clone())
        }
        _ => {
            return Err(cyallo_ast::diagnostic::invalid_concurrent_call(
                id.diagnostic_name(),
                types,
                args,
                span,
            ));
        }
    };
    Ok(ResolvedCall {
        intrinsic: id,
        result,
        lowering: Lowering::Storage(lookup::Operation::Concurrent(op)),
    })
}
