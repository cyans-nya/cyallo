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
        Type::Numeric(_)
        | Type::Integer
        | Type::Real
        | Type::Boolean
        | Type::Char
        | Type::String
        | Type::Void => true,
        Type::Array(_, t) | Type::SparseArray(t) => transferable(t),
        Type::Tuple(ts) => ts.iter().all(transferable),
        Type::Record(_, fs) => fs.iter().all(|(_, t)| transferable(t)),
        Type::Storage(K::Sender | K::Receiver | K::Clock, _) => true,
        Type::Storage(K::Vec | K::VecDeque | K::Option | K::Result | K::SendError, t) => {
            transferable(t)
        }
        _ => false,
    }
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
    let pair = |t: &Type| {
        Type::Tuple(vec![
            lookup::storage(K::Sender, t.clone()),
            lookup::storage(K::Receiver, t.clone()),
        ])
    };
    let (op, result) = match (id, types, args) {
        (Intrinsic::ChannelBounded, [t], [n])
            if transferable(t) && n.number_kind() == Some(NumberKind::Usize) =>
        {
            (Operation::Bounded, pair(t))
        }
        (Intrinsic::ChannelUnbounded, [t], []) if transferable(t) => {
            (Operation::Unbounded, pair(t))
        }
        (Intrinsic::ClockNew, [], []) => (Operation::Clock, lookup::storage(K::Clock, Type::Void)),
        (Intrinsic::TaskSpawn, [], [callable])
            if cyallo_ast::callable(callable)
                .is_some_and(|(_, p, r)| p.is_empty() && transferable(r)) =>
        {
            let (_, _, result) = cyallo_ast::callable(callable).unwrap();
            (Operation::Spawn, lookup::storage(K::Task, result.clone()))
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
