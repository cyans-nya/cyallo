//! Specialization of resolved member declarations. Source spellings are used
//! only by native_contract's declaration lookup, never to select an operation.
use crate::{
    declarations::{DeclarationError, TypeEnvironment, invalid_declaration},
    internal::{InternalSemantic, MemberIntrinsic as M},
    lookup::{self, Operation as O, ReceiverMode},
    native_contract, services,
};
use cyallo_ast::*;

#[derive(Debug, Clone, Copy)]
pub enum Body {
    Storage(O),
    Service(services::Operation),
}
#[derive(Debug, Clone)]
pub struct Resolved {
    pub declaration: DeclarationId,
    pub receiver: ReceiverMode,
    pub kind: RoutineKind,
    pub params: Vec<Param>,
    pub result: Type,
    pub obligations: Vec<native_contract::TraitObligation>,
    pub body: Body,
}
fn storage(kind: StorageKind, ty: Type) -> Type {
    lookup::storage(kind, ty)
}
fn representation(ty: &Type) -> &Type {
    match ty {
        Type::Reference { pointee, .. } => pointee,
        _ => ty,
    }
}
fn payload(ty: &Type, kind: StorageKind) -> Option<&Type> {
    lookup::inner(representation(ty), kind)
}
fn equivalent(a: &Type, b: &Type) -> bool {
    crate::declarations::equivalent_types(a, b)
}
pub fn resolve(
    env: &TypeEnvironment,
    receiver: &Type,
    name: &str,
    args: &[Expr],
    types: &[Type],
    span: Span,
) -> Result<Option<Resolved>, Diagnostic> {
    let Some(member) = native_contract::member(env, receiver, name, args, types, span)? else {
        return Ok(None);
    };
    let callable = member.callable;
    env.check_native_access(callable.declaration, span)?;
    let InternalSemantic::Member(id) = callable.semantic else {
        return Err(invalid_declaration(
            DeclarationError::InvalidSignature,
            span,
        ));
    };
    let (body, result) = specialize(
        id,
        &member.receiver_type,
        types,
        args.first().and_then(constant_text),
    )
    .ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, span))?;
    if !equivalent(&callable.result, &result) {
        return Err(invalid_declaration(
            DeclarationError::InvalidSignature,
            span,
        ));
    }
    // A borrowed receiver can use the VM's shallow descriptor ABI without
    // transferring ownership. Operations that mutate container storage retain
    // an address; read-only descriptors also support temporary receivers.
    let mode = match id {
        M::GeneratorIsComplete
        | M::GeneratorIsYielded
        | M::LayoutSize
        | M::LayoutAlign
        | M::ResultIsOk
        | M::ResultIsErr
        | M::OptionIsSome
        | M::OptionIsNone
        | M::SendErrorClosed
        | M::SendErrorFull
        | M::SendErrorLimit
        | M::LoadErrorNotFound
        | M::LoadErrorDenied
        | M::LoadErrorCheck
        | M::ViewErrorMessage
        | M::LoadErrorMessage
        | M::ReceiveErrorClosed
        | M::ReceiveErrorEmpty
        | M::TaskIsCompleted
        | M::TaskIsFailed
        | M::TaskIsCancelled
        | M::TaskIsStopped
        | M::LocationLoad
        | M::LocationStore => ReceiverMode::Value,
        M::RcClone
        | M::RcDowngrade
        | M::RcUpgrade
        | M::RcStrongCount
        | M::RcWeakCount
        | M::GcClone => ReceiverMode::Owner,
        M::ContextExtend
        | M::ContextIsolate
        | M::ContextService
        | M::ContextProvide
        | M::ContextAvailable
        | M::ContextDispose
        | M::RegistrationDispose
        | M::ContextInject
        | M::ContextOnDispose => ReceiverMode::Value,
        _ => member.receiver,
    };
    Ok(Some(Resolved {
        declaration: callable.declaration,
        receiver: mode,
        kind: callable.kind,
        params: callable.params,
        result: callable.result,
        obligations: callable.obligations,
        body,
    }))
}
fn specialize(id: M, receiver: &Type, args: &[Type], field: Option<&str>) -> Option<(Body, Type)> {
    use Body::{Service, Storage};
    use StorageKind as K;
    let r = representation(receiver);
    let boolean = Type::Boolean;
    let usize_type = || Type::numeric(NumberKind::Usize);
    let concurrent = |op| Storage(O::Concurrent(op));
    use crate::concurrent::Operation as C;
    let result = match id {
        M::GeneratorStart | M::GeneratorResume | M::GeneratorNext | M::GeneratorClose => {
            let (_, input, _) = crate::generator::parts(r)?;
            let operation = match id {
                M::GeneratorStart => O::GeneratorStart,
                M::GeneratorResume if args.is_empty() && crate::generator::unit(input) => {
                    O::GeneratorResume(lookup::ResumeInput::Unit)
                }
                M::GeneratorResume if matches!(args, [actual] if equivalent(input, actual)) => {
                    O::GeneratorResume(lookup::ResumeInput::Value)
                }
                M::GeneratorNext if crate::generator::unit(input) => O::GeneratorNext,
                M::GeneratorClose => O::GeneratorClose,
                _ => return None,
            };
            (
                Storage(operation),
                if matches!(id, M::GeneratorClose) {
                    Type::Void
                } else {
                    crate::generator::step(r)?
                },
            )
        }
        M::GeneratorIsComplete
        | M::GeneratorIsYielded
        | M::GeneratorYielded
        | M::GeneratorCompleted => {
            let Type::Tuple(types) = payload(r, K::GeneratorStep)? else {
                return None;
            };
            let [yielded, completed] = types.as_slice() else {
                return None;
            };
            match id {
                M::GeneratorIsComplete => (Storage(O::GeneratorIsComplete), boolean),
                M::GeneratorIsYielded => (Storage(O::GeneratorIsYielded), boolean),
                M::GeneratorYielded => (Storage(O::GeneratorYielded), yielded.clone()),
                M::GeneratorCompleted => (Storage(O::GeneratorCompleted), completed.clone()),
                _ => return None,
            }
        }
        M::LayoutSize | M::LayoutAlign if payload(r, K::Layout).is_some() => (
            Storage(if id == M::LayoutSize {
                O::LayoutSize
            } else {
                O::LayoutAlign
            }),
            usize_type(),
        ),
        M::RcClone | M::GcClone => {
            let Type::Managed { kind, .. } = r else {
                return None;
            };
            if (*kind == MemoryKind::Rc) != (id == M::RcClone) {
                return None;
            }
            (
                Storage(if id == M::RcClone {
                    O::RcClone
                } else {
                    O::GcClone
                }),
                r.clone(),
            )
        }
        M::RcDowngrade => {
            if !matches!(
                r,
                Type::Managed {
                    kind: MemoryKind::Rc,
                    ..
                }
            ) {
                return None;
            }
            (Storage(O::RcDowngrade), storage(K::Weak, r.clone()))
        }
        M::RcUpgrade => (
            Storage(O::RcUpgrade),
            storage(K::Option, payload(r, K::Weak)?.clone()),
        ),
        M::RcStrongCount | M::RcWeakCount
            if matches!(
                r,
                Type::Managed {
                    kind: MemoryKind::Rc,
                    ..
                }
            ) =>
        {
            (
                Storage(if id == M::RcStrongCount {
                    O::RcStrongCount
                } else {
                    O::RcWeakCount
                }),
                usize_type(),
            )
        }
        M::ResultIsOk | M::ResultIsErr | M::ResultUnwrap | M::ResultUnwrapErr => {
            let Type::Tuple(types) = payload(r, K::Result)? else {
                return None;
            };
            let [value, error] = types.as_slice() else {
                return None;
            };
            match id {
                M::ResultIsOk => (Storage(O::IsOk), boolean),
                M::ResultIsErr => (Storage(O::IsErr), boolean),
                M::ResultUnwrap => (Storage(O::UnwrapOk), value.clone()),
                M::ResultUnwrapErr => (Storage(O::UnwrapErr), error.clone()),
                _ => return None,
            }
        }
        M::Slice | M::MutableSlice => {
            let element = match r {
                Type::Array(_, e) | Type::Slice(e) | Type::Storage(K::Vec, e) => e,
                _ => return None,
            };
            (
                Storage(O::Slice(id == M::MutableSlice)),
                Type::Reference {
                    mutable: id == M::MutableSlice,
                    lifetime: None,
                    pointee: Box::new(Type::Slice(element.clone())),
                },
            )
        }
        M::CollectionPushBack
        | M::CollectionPushFront
        | M::CollectionPopBack
        | M::CollectionPopFront => {
            let Type::Storage(kind @ (K::Vec | K::VecDeque), t) = r else {
                return None;
            };
            if matches!(id, M::CollectionPushFront | M::CollectionPopFront) && *kind != K::VecDeque
            {
                return None;
            }
            match id {
                M::CollectionPushBack | M::CollectionPushFront if matches!(args, [a] if equivalent(a,t)) => {
                    (
                        Storage(O::CollectionPush(id == M::CollectionPushFront)),
                        Type::Void,
                    )
                }
                M::CollectionPopBack | M::CollectionPopFront => (
                    Storage(O::CollectionPop(id == M::CollectionPopFront)),
                    storage(K::Option, *t.clone()),
                ),
                _ => return None,
            }
        }
        M::CollectionLength
            if matches!(
                r,
                Type::Array(..) | Type::Slice(_) | Type::Storage(K::Vec | K::VecDeque, _)
            ) =>
        {
            (Storage(O::CollectionLength), usize_type())
        }
        M::RangeReverse if payload(r, K::Range).is_some() => (Storage(O::RangeReverse), r.clone()),
        M::CellReplace if payload(r, K::Cell).is_some() => {
            (Storage(O::CellReplace), payload(r, K::Cell)?.clone())
        }
        M::LocationLoad if payload(r, K::Location).is_some() => {
            (Storage(O::LocationLoad), payload(r, K::Location)?.clone())
        }
        M::LocationStore if payload(r, K::Location).is_some() => {
            (Storage(O::LocationStore), Type::Void)
        }
        M::OptionIsSome | M::OptionIsNone if payload(r, K::Option).is_some() => (
            Storage(if id == M::OptionIsSome {
                O::IsSome
            } else {
                O::IsNone
            }),
            boolean,
        ),
        M::OptionUnwrap | M::OptionOrFail => (Storage(O::Unwrap), payload(r, K::Option)?.clone()),
        M::StoragePathField => {
            let Type::Record(_, fields) = payload(r, K::StoragePath)? else {
                return None;
            };
            let index = fields
                .iter()
                .position(|(name, _)| Some(name.as_ref()) == field)?;
            (
                Storage(O::Field(index)),
                storage(K::StoragePath, fields[index].1.clone()),
            )
        }
        M::StoragePathElement => {
            let element = match payload(r, K::StoragePath)? {
                Type::Array(_, t) | Type::Storage(K::Vec | K::VecDeque, t) => t,
                _ => return None,
            };
            (
                Storage(O::Element),
                storage(K::StoragePath, *element.clone()),
            )
        }
        M::StoragePathTarget => {
            let pointee = match payload(r, K::StoragePath)? {
                Type::Managed { pointee, .. } | Type::Storage(K::Cell, pointee) => pointee,
                _ => return None,
            };
            (
                Storage(O::Target),
                storage(K::StoragePath, *pointee.clone()),
            )
        }
        M::ChannelClone | M::ClockClone
            if matches!(r, Type::Storage(K::Sender | K::Receiver | K::Clock, _)) =>
        {
            (concurrent(C::Clone), r.clone())
        }
        M::ChannelClose | M::ChannelIsClosed
            if matches!(r, Type::Storage(K::Sender | K::Receiver, _)) =>
        {
            (
                concurrent(if id == M::ChannelClose {
                    C::Close
                } else {
                    C::IsClosed
                }),
                if id == M::ChannelClose {
                    Type::Void
                } else {
                    boolean
                },
            )
        }
        M::ChannelSend | M::ChannelTrySend => (
            concurrent(if id == M::ChannelSend {
                C::Send
            } else {
                C::TrySend
            }),
            lookup::result_type(
                Type::Void,
                storage(K::SendError, payload(r, K::Sender)?.clone()),
            ),
        ),
        M::ChannelReceive | M::ChannelTryReceive => (
            concurrent(if id == M::ChannelReceive {
                C::Receive
            } else {
                C::TryReceive
            }),
            lookup::result_type(
                payload(r, K::Receiver)?.clone(),
                storage(K::ReceiveError, Type::Void),
            ),
        ),
        M::SendErrorValue => (
            concurrent(C::SendErrorValue),
            payload(r, K::SendError)?.clone(),
        ),
        M::SendErrorClosed | M::SendErrorFull | M::SendErrorLimit
            if payload(r, K::SendError).is_some() =>
        {
            (
                concurrent(match id {
                    M::SendErrorClosed => C::SendErrorClosed,
                    M::SendErrorFull => C::SendErrorFull,
                    _ => C::SendErrorLimit,
                }),
                boolean,
            )
        }
        M::ReceiveErrorClosed | M::ReceiveErrorEmpty if payload(r, K::ReceiveError).is_some() => (
            concurrent(if id == M::ReceiveErrorClosed {
                C::ReceiveErrorClosed
            } else {
                C::ReceiveErrorEmpty
            }),
            boolean,
        ),
        M::LoadErrorNotFound | M::LoadErrorDenied | M::LoadErrorCheck | M::LoadErrorMessage
            if payload(r, K::LoadError).is_some() =>
        {
            (
                Body::Storage(match id {
                    M::LoadErrorNotFound => O::LoadErrorNotFound,
                    M::LoadErrorDenied => O::LoadErrorDenied,
                    M::LoadErrorCheck => O::LoadErrorCheck,
                    _ => O::LoadErrorMessage,
                }),
                if id == M::LoadErrorMessage {
                    Type::String
                } else {
                    boolean
                },
            )
        }
        M::ViewErrorMessage if payload(r, K::ViewError).is_some() => {
            (Body::Storage(O::ViewErrorMessage), Type::String)
        }
        M::ClockSleep if payload(r, K::Clock).is_some() => (concurrent(C::Sleep), Type::Void),
        M::TaskJoin => (
            concurrent(C::Join),
            storage(K::TaskOutcome, payload(r, K::Task)?.clone()),
        ),
        M::TaskCancel | M::TaskIsFinished if payload(r, K::Task).is_some() => (
            concurrent(if id == M::TaskCancel {
                C::Cancel
            } else {
                C::IsFinished
            }),
            if id == M::TaskCancel {
                Type::Void
            } else {
                boolean
            },
        ),
        M::TaskCompleted => (
            concurrent(C::Completed),
            payload(r, K::TaskOutcome)?.clone(),
        ),
        M::TaskIsCompleted | M::TaskIsFailed | M::TaskIsCancelled | M::TaskIsStopped
            if payload(r, K::TaskOutcome).is_some() =>
        {
            (
                concurrent(match id {
                    M::TaskIsCompleted => C::IsCompleted,
                    M::TaskIsFailed => C::IsFailed,
                    M::TaskIsCancelled => C::IsCancelled,
                    _ => C::IsStopped,
                }),
                boolean,
            )
        }
        M::ContextExtend | M::ContextIsolate if *r == Type::Context => (
            Service(if id == M::ContextExtend {
                services::Operation::Extend
            } else {
                services::Operation::Isolate
            }),
            Type::Context,
        ),
        M::ContextService if *r == Type::Context => {
            let [Type::ServiceKey(t)] = args else {
                return None;
            };
            (
                Service(services::Operation::Reference),
                Type::ServiceRef(t.clone()),
            )
        }
        M::ContextProvide if *r == Type::Context => {
            (Service(services::Operation::Provide), Type::Registration)
        }
        M::ContextAvailable if *r == Type::Context => {
            (Service(services::Operation::Available), boolean)
        }
        M::ContextDispose | M::RegistrationDispose
            if matches!(r, Type::Context | Type::Registration) =>
        {
            (Service(services::Operation::Dispose), Type::Void)
        }
        M::ContextInject if *r == Type::Context => {
            (Service(services::Operation::Inject), Type::Void)
        }
        M::ContextOnDispose if *r == Type::Context => {
            (Service(services::Operation::OnDispose), Type::Void)
        }
        _ => return None,
    };
    Some(result)
}
