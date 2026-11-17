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
/// The single type argument of an opaque host type (`Sender<T>` → `T`). The type is
/// known by its declaration; its element type is whatever the declaration applied.
/// The argument of an opaque type whose declaration carries a given trait.
fn trait_argument(ty: &Type, has: fn(&cyallo_ast::OpaqueTraits) -> bool) -> Option<&Type> {
    match representation(ty) {
        Type::Opaque(_, arguments, traits) if has(traits) && arguments.len() == 1 => {
            arguments.first()
        }
        _ => None,
    }
}
fn opaque_argument(ty: &Type) -> Option<&Type> {
    match representation(ty) {
        Type::Opaque(_, arguments, _) if arguments.len() == 1 => arguments.first(),
        _ => None,
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
        &callable.result,
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
fn specialize(
    id: M,
    receiver: &Type,
    args: &[Type],
    field: Option<&str>,
    declared: &Type,
) -> Option<(Body, Type)> {
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
                    // The declaration names the step type.
                    let step = crate::generator::step(r)?;
                    equivalent(&step, declared).then(|| declared.clone())?
                },
            )
        }
        M::GeneratorIsComplete
        | M::GeneratorIsYielded
        | M::GeneratorYielded
        | M::GeneratorCompleted => {
            let (yielded, completed) = lookup::step_payload(representation(r))?;
            match id {
                M::GeneratorIsComplete => (Storage(O::GeneratorIsComplete), boolean),
                M::GeneratorIsYielded => (Storage(O::GeneratorIsYielded), boolean),
                M::GeneratorYielded => (Storage(O::GeneratorYielded), yielded.clone()),
                M::GeneratorCompleted => (Storage(O::GeneratorCompleted), completed.clone()),
                _ => return None,
            }
        }
        M::LayoutSize | M::LayoutAlign
            if matches!(r, Type::Opaque(_, _, traits) if traits.layout()) =>
        (
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
            // The weak type is the declared result.
            (Storage(O::RcDowngrade), declared.clone())
        }
        M::RcUpgrade if matches!(r, Type::Opaque(_, _, traits) if traits.weak()) => (
            Storage(O::RcUpgrade),
            // The declaration names the optional owner.
            matches!(lookup::option_payload(declared), Some(t) if equivalent(t, opaque_argument(r)?))
                .then(|| declared.clone())?,
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
            let (value, error) = lookup::result_payload(representation(r))?;
            match id {
                M::ResultIsOk => (Storage(O::IsOk), boolean),
                M::ResultIsErr => (Storage(O::IsErr), boolean),
                M::ResultUnwrap => (Storage(O::UnwrapOk), value.clone()),
                M::ResultUnwrapErr => (Storage(O::UnwrapErr), error.clone()),
                _ => return None,
            }
        }
        M::Slice | M::MutableSlice => {
            let element: &Type = match r {
                Type::Array(_, e) | Type::Slice(e) => e,
                // A growable collection that only grows at the back slices contiguously.
                other if !other.is_double_ended() => other.collection_element()?,
                _ => return None,
            };
            (
                Storage(O::Slice(id == M::MutableSlice)),
                Type::Reference {
                    mutable: id == M::MutableSlice,
                    lifetime: None,
                    pointee: Box::new(Type::Slice(Box::new(element.clone()))),
                },
            )
        }
        M::CollectionPushBack
        | M::CollectionPushFront
        | M::CollectionPopBack
        | M::CollectionPopFront => {
            let t = r.collection_element()?;
            if matches!(id, M::CollectionPushFront | M::CollectionPopFront)
                && !r.is_double_ended()
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
                    matches!(lookup::option_payload(declared), Some(p) if equivalent(p, t))
                        .then(|| declared.clone())?,
                ),
                _ => return None,
            }
        }
        M::CollectionLength
            if matches!(r, Type::Array(..) | Type::Slice(_)) || r.collection_element().is_some() =>
        {
            (Storage(O::CollectionLength), usize_type())
        }
        M::RangeReverse if payload(r, K::Range).is_some() => (Storage(O::RangeReverse), r.clone()),
        M::CellReplace if trait_argument(r, |t| t.deref()).is_some() => (
            Storage(O::CellReplace),
            trait_argument(r, |t| t.deref())?.clone(),
        ),
        M::LocationLoad if opaque_argument(r).is_some() => {
            (Storage(O::LocationLoad), opaque_argument(r)?.clone())
        }
        M::LocationStore if opaque_argument(r).is_some() => (Storage(O::LocationStore), Type::Void),
        M::OptionIsSome | M::OptionIsNone if lookup::option_payload(representation(r)).is_some() => (
            Storage(if id == M::OptionIsSome {
                O::IsSome
            } else {
                O::IsNone
            }),
            boolean,
        ),
        M::OptionUnwrap | M::OptionOrFail => (
            Storage(O::Unwrap),
            lookup::option_payload(representation(r))?.clone(),
        ),
        M::StoragePathField => {
            let Type::Record(_, fields) = trait_argument(r, |t| t.path())? else {
                return None;
            };
            let index = fields
                .iter()
                .position(|(name, _)| Some(name.as_ref()) == field)?;
            // The path type is the declared result; the index comes from the receiver.
            (Storage(O::Field(index)), declared.clone())
        }
        M::StoragePathElement => {
            let path_target = trait_argument(r, |t| t.path())?;
            if !matches!(path_target, Type::Array(..)) && path_target.collection_element().is_none() {
                return None;
            }
            (Storage(O::Element), declared.clone())
        }
        M::StoragePathTarget => {
            match trait_argument(r, |t| t.path())? {
                Type::Managed { .. } => {}
                Type::Opaque(_, arguments, traits) if traits.deref() && arguments.len() == 1 => {}
                _ => return None,
            }
            (Storage(O::Target), declared.clone())
        }
        M::ChannelClone if matches!(r, Type::Opaque(..)) => (concurrent(C::Clone), r.clone()),
        // A clock is known by its declaration: the receiver is whatever opaque type
        // the declared `clone(self: &Self)` names, and the result is that type.
        M::ClockClone if matches!(r, Type::Opaque(..)) => (concurrent(C::Clone), r.clone()),
        M::ChannelClose | M::ChannelIsClosed if matches!(r, Type::Opaque(..)) => {
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
            // The declaration names `Result<(), SendError<T>>`.
            {
                let expected = lookup::result_type(
                    Type::Void,
                    storage(K::SendError, opaque_argument(r)?.clone()),
                );
                equivalent(&expected, declared).then(|| declared.clone())?
            },
        ),
        M::ChannelReceive | M::ChannelTryReceive => (
            concurrent(if id == M::ChannelReceive {
                C::Receive
            } else {
                C::TryReceive
            }),
            // The declaration names the error enum: `Result<T, ReceiveError>`.
            declared.clone(),
        ),
        M::SendErrorValue => (
            concurrent(C::SendErrorValue),
            lookup::send_payload(representation(r))?.clone(),
        ),
        M::SendErrorClosed | M::SendErrorFull | M::SendErrorLimit
            if lookup::send_payload(representation(r)).is_some() =>
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
        M::ReceiveErrorClosed | M::ReceiveErrorEmpty if matches!(representation(r), Type::Enum(_)) => (
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
        M::ClockSleep if matches!(r, Type::Opaque(..)) => (concurrent(C::Sleep), Type::Void),
        // The outcome type is the declared result.
        M::TaskJoin => (concurrent(C::Join), declared.clone()),
        M::TaskCancel | M::TaskIsFinished if matches!(r, Type::Opaque(..)) => (
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
        M::TaskCompleted => (concurrent(C::Completed), opaque_argument(r)?.clone()),
        M::TaskIsCompleted | M::TaskIsFailed | M::TaskIsCancelled | M::TaskIsStopped
            if matches!(r, Type::Opaque(..)) =>
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
