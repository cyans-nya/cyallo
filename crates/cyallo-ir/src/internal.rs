//! Private semantic ABI keys attached to trusted source declarations.
//!
//! Public names, module paths, exports, signatures and visibility belong to the
//! parsed declaration graph. This registry identifies only compiler/runtime
//! semantics and must never be used to resolve a public source path.

use crate::intrinsic::Intrinsic;
use cyallo_ast::StorageKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeType {
    Storage(StorageKind),
    /// An opaque host type; its declaration supplies identity and arity, the
    /// provider's key supplies its traits (`opaque:transfer,effectful,clone`).
    Opaque(cyallo_ast::OpaqueTraits),
    Any,
    Symbol,
    Context,
    Registration,
    ServiceKey,
    ServiceRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeTrait {
    Numeric,
    Integer,
    Floating,
    Printable,
    Transferable,
    Collection,
    NumericCollection,
    Cloneable,
    RcOwner,
    WeakOwner,
    GcOwner,
    GcView,
    Lengthable,
    LegacyNumeric,
    ObjectView,
    ObjectLike,
}

/// Compiler-evaluated type constructors declared in trusted native contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeTypeFamily {
    RangeArray,
    EmptyArray,
    Field,
    Element,
    Target,
    AllocatedRc,
    WeakOf,
}

impl std::fmt::Display for NativeTypeFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RangeArray => "range array",
            Self::EmptyArray => "empty array",
            Self::Field => "field",
            Self::Element => "element",
            Self::Target => "target",
            Self::AllocatedRc => "allocated RC owner",
            Self::WeakOf => "weak owner",
        })
    }
}

/// Member identities are independent of the source member's name and owner path.
/// The resolved declaration supplies receiver mode, parameters and result type.
macro_rules! members {
    ($($variant:ident),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum MemberIntrinsic { $($variant),* }
        impl MemberIntrinsic {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];
        }
    };
}
members! {
    GeneratorStart,
    GeneratorResume,
    GeneratorNext,
    GeneratorClose,
    GeneratorIsComplete,
    GeneratorIsYielded,
    GeneratorYielded,
    GeneratorCompleted,
    LayoutSize,
    LayoutAlign,
    RcClone,
    RcDowngrade,
    RcUpgrade,
    RcStrongCount,
    RcWeakCount,
    GcClone,
    ResultIsOk,
    ResultIsErr,
    ResultUnwrap,
    ResultUnwrapErr,
    Slice,
    MutableSlice,
    CollectionPushBack,
    CollectionPushFront,
    CollectionPopBack,
    CollectionPopFront,
    CollectionLength,
    RangeReverse,
    CellReplace,
    LocationLoad,
    LocationStore,
    OptionIsSome,
    OptionIsNone,
    OptionUnwrap,
    OptionOrFail,
    StoragePathField,
    StoragePathElement,
    StoragePathTarget,
    ChannelClone,
    ChannelClose,
    ChannelIsClosed,
    ChannelSend,
    ChannelTrySend,
    ChannelReceive,
    ChannelTryReceive,
    SendErrorValue,
    SendErrorClosed,
    SendErrorFull,
    SendErrorLimit,
    LoadErrorNotFound,
    LoadErrorDenied,
    LoadErrorCheck,
    LoadErrorMessage,
    ViewErrorMessage,
    ReceiveErrorClosed,
    ReceiveErrorEmpty,
    ClockClone,
    ClockSleep,
    TaskJoin,
    TaskCancel,
    TaskIsFinished,
    TaskCompleted,
    TaskIsCompleted,
    TaskIsFailed,
    TaskIsCancelled,
    TaskIsStopped,
    ContextExtend,
    ContextIsolate,
    ContextService,
    ContextProvide,
    ContextAvailable,
    ContextDispose,
    RegistrationDispose,
    ContextInject,
    ContextOnDispose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalSemantic {
    Type(NativeType),
    Variant(cyallo_ast::EnumEncoding),
    Trait(NativeTrait),
    TypeFamily(NativeTypeFamily),
    Function(Intrinsic),
    Member(MemberIntrinsic),
    Prelude,
    AllocatorTrait,
}

/// `opaque` or `opaque:flag,flag`: a host type with no engine-known identity. Flags:
/// `copy`, `transfer`, `effectful`, `clone`, `resource`, `indirect`, `generator`, `weak`, `deref`, `target`, `path`, `layout`, `allocation`, `collection`, `double_ended`, `location`, `sender`, `receiver`, `task`, `task_outcome`, `clock`.
fn opaque_key(key: &str) -> Option<cyallo_ast::OpaqueTraits> {
    let flags = match key {
        "opaque" => "",
        _ => key.strip_prefix("opaque:")?,
    };
    let mut traits = cyallo_ast::OpaqueTraits::default();
    for flag in flags.split(',').filter(|f| !f.is_empty()) {
        match flag {
            "copy" => traits = traits.with_copyable(true),
            "transfer" => traits = traits.with_transferable(true),
            "effectful" => traits = traits.with_effectful(true),
            "clone" => traits = traits.with_cloneable(true),
            "resource" => traits = traits.with_resource(true),
            "indirect" => traits = traits.with_indirect(true),
            "generator" => traits = traits.with_generator(true),
            "weak" => traits = traits.with_weak(true),
            "deref" => traits = traits.with_deref(true),
            "target" => traits = traits.with_target(true),
            "path" => traits = traits.with_path(true),
            "layout" => traits = traits.with_layout(true),
            "allocation" => traits = traits.with_allocation(true),
            "collection" => traits = traits.with_collection(true),
            "double_ended" => traits = traits.with_double_ended(true),
            "location" => traits = traits.with_location(true),
            "sender" => traits = traits.with_sender(true),
            "receiver" => traits = traits.with_receiver(true),
            "task" => traits = traits.with_task(true),
            "task_outcome" => traits = traits.with_task_outcome(true),
            "clock" => traits = traits.with_clock(true),
            _ => return None,
        }
    }
    Some(traits)
}
macro_rules! abi_keys {
    ($($key:literal => $semantic:expr),* $(,)?) => {
        /// Accepts a private ABI key; retains no caller storage.
        pub fn from_key(key: &str) -> Option<InternalSemantic> {
            if let Some(traits) = opaque_key(key) {
                return Some(Type(N::Opaque(traits)));
            }
            match key {
                $($key => Some($semantic),)*
                _ => None,
            }
        }

        /// The complete private ABI, for validation and diagnostics only.
        pub const ABI_KEYS: &[(&str, InternalSemantic)] = &[
            $(($key, $semantic),)*
        ];
    };
}

use InternalSemantic::{AllocatorTrait, Function, Member, Prelude, Trait, Type, TypeFamily};
use MemberIntrinsic as M;
use NativeTrait as T;
use NativeType as N;
use NativeTypeFamily as F;
use StorageKind as S;

abi_keys! {
    "prelude" => Prelude,
    "allocator_trait" => AllocatorTrait,
    "numeric_trait" => Trait(T::Numeric),
    "integer_trait" => Trait(T::Integer),
    "floating_trait" => Trait(T::Floating),
    "printable_trait" => Trait(T::Printable),
    "transferable_trait" => Trait(T::Transferable),
    "collection_trait" => Trait(T::Collection),
    "numeric_collection_trait" => Trait(T::NumericCollection),
    "cloneable_trait" => Trait(T::Cloneable),
    "rc_owner_trait" => Trait(T::RcOwner),
    "weak_owner_trait" => Trait(T::WeakOwner),
    "gc_owner_trait" => Trait(T::GcOwner),
    "gc_view_trait" => Trait(T::GcView),
    "lengthable_trait" => Trait(T::Lengthable),
    "object_view_trait" => Trait(T::ObjectView),
    "object_like_trait" => Trait(T::ObjectLike),
    "object_view" => Function(Intrinsic::ObjectView),
    "object_freeze" => Function(Intrinsic::ObjectFreeze),
    "type_descriptor_struct" => Type(N::Storage(S::TypeDescriptor)),
    "view_error_struct" => Type(N::Storage(S::ViewError)),
    "view_error_message" => Member(M::ViewErrorMessage),
    "module_load" => Function(Intrinsic::ModuleLoad),
    "load_error_struct" => Type(N::Storage(S::LoadError)),
    "load_error_not_found" => Member(M::LoadErrorNotFound),
    "load_error_denied" => Member(M::LoadErrorDenied),
    "load_error_check" => Member(M::LoadErrorCheck),
    "load_error_message" => Member(M::LoadErrorMessage),
    "legacy_numeric_trait" => Trait(T::LegacyNumeric),
    "range_array_type" => TypeFamily(F::RangeArray),
    "empty_array_type" => TypeFamily(F::EmptyArray),
    "field_type" => TypeFamily(F::Field),
    "element_type" => TypeFamily(F::Element),
    "target_type" => TypeFamily(F::Target),
    "allocated_rc_type" => TypeFamily(F::AllocatedRc),
    "weak_of_type" => TypeFamily(F::WeakOf),
    "any_struct" => Type(N::Any),
    "symbol_struct" => Type(N::Symbol),
    "context_struct" => Type(N::Context),
    "registration_struct" => Type(N::Registration),
    "service_key_struct" => Type(N::ServiceKey),
    "service_ref_struct" => Type(N::ServiceRef),
    "step_yielded_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::StepYielded),
    "step_complete_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::StepComplete),
    "send_closed_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::SendClosed),
    "send_full_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::SendFull),
    "send_limit_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::SendLimit),
    "receive_closed_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::ReceiveClosed),
    "receive_empty_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::ReceiveEmpty),
    "option_some_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::OptionSome),
    "option_none_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::OptionNone),
    "result_ok_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::ResultOk),
    "result_err_variant" => InternalSemantic::Variant(cyallo_ast::EnumEncoding::ResultErr),
    "option_struct" => Type(N::Storage(S::Option)),
    "result_struct" => Type(N::Storage(S::Result)),
    "range_struct" => Type(N::Storage(S::Range)),
    "parse_error_struct" => Type(N::Storage(S::ParseError)),
    "alloc_error_struct" => Type(N::Storage(S::AllocError)),
    "generator_step_struct" => Type(N::Storage(S::GeneratorStep)),
    "send_error_struct" => Type(N::Storage(S::SendError)),
    "channel_bounded" => Function(Intrinsic::ChannelBounded),
    "channel_unbounded" => Function(Intrinsic::ChannelUnbounded),
    "clock_new" => Function(Intrinsic::ClockNew),
    "task_spawn" => Function(Intrinsic::TaskSpawn),
    "layout_new" => Function(Intrinsic::LayoutNew),
    "allocate" => Function(Intrinsic::Allocate),
    "deallocate" => Function(Intrinsic::Deallocate),
    "alloc_error_new" => Function(Intrinsic::AllocErrorNew),
    "result_ok" => Function(Intrinsic::ResultOk),
    "result_err" => Function(Intrinsic::ResultErr),
    "rc_try_new_in" => Function(Intrinsic::RcTryNewIn),
    "rc_clone" => Function(Intrinsic::RcClone),
    "rc_downgrade" => Function(Intrinsic::RcDowngrade),
    "rc_upgrade" => Function(Intrinsic::RcUpgrade),
    "rc_strong_count" => Function(Intrinsic::RcStrongCount),
    "rc_weak_count" => Function(Intrinsic::RcWeakCount),
    "gc_clone" => Function(Intrinsic::GcClone),
    "drop_value" => Function(Intrinsic::DropValue),
    "clone_value" => Function(Intrinsic::CloneValue),
    "field_location" => Function(Intrinsic::FieldLocation),
    "vec_from" => Function(Intrinsic::VecFrom),
    "vec_deque_from" => Function(Intrinsic::DequeFrom),
    "array_empty" => Function(Intrinsic::ArrayEmpty),
    "array_from" => Function(Intrinsic::ArrayFrom),
    "array_length" => Function(Intrinsic::ArrayLength),
    "array_sum" => Function(Intrinsic::ArraySum),
    "numeric_parse" => Function(Intrinsic::Parse),
    "numeric_abs" => Function(Intrinsic::NumAbs),
    "numeric_min" => Function(Intrinsic::NumMin),
    "numeric_max" => Function(Intrinsic::NumMax),
    "numeric_clamp" => Function(Intrinsic::NumClamp),
    "float_round" => Function(Intrinsic::FloatRound),
    "float_floor" => Function(Intrinsic::FloatFloor),
    "float_ceil" => Function(Intrinsic::FloatCeil),
    "float_sqrt" => Function(Intrinsic::FloatSqrt),
    "float_pow" => Function(Intrinsic::FloatPow),
    "string_length" => Function(Intrinsic::StringLength),
    "string_trim" => Function(Intrinsic::Trim),
    "string_upper" => Function(Intrinsic::Upper),
    "string_lower" => Function(Intrinsic::Lower),
    "string_contains" => Function(Intrinsic::Contains),
    "string_starts_with" => Function(Intrinsic::StartsWith),
    "string_ends_with" => Function(Intrinsic::EndsWith),
    "string_replace" => Function(Intrinsic::Replace),
    "string_from" => Function(Intrinsic::StringFrom),
    "char_code" => Function(Intrinsic::CharCode),
    "char_from_code" => Function(Intrinsic::CharFromCode),
    "random_uniform" => Function(Intrinsic::Random),
    "cell_new" => Function(Intrinsic::CellNew),
    "lookup_path" => Function(Intrinsic::LookupPath),
    "lookup_missing" => Function(Intrinsic::MissingPath),
    "lookup_mapped" => Function(Intrinsic::MappedLookup),
    "option_some" => Function(Intrinsic::OptionSome),
    "option_none" => Function(Intrinsic::OptionNone),
    "any_erase" => Function(Intrinsic::Erase),
    "any_checked" => Function(Intrinsic::Checked),
    "symbol_new" => Function(Intrinsic::FreshSymbol),
    "context_new" => Function(Intrinsic::ContextNew),
    "service_key_new" => Function(Intrinsic::ServiceKeyNew),
    "print" => Function(Intrinsic::Print),
    "input" => Function(Intrinsic::Input),
    "caie_left" => Function(Intrinsic::Left),
    "caie_right" => Function(Intrinsic::Right),
    "caie_mid" => Function(Intrinsic::Mid),
    "caie_length" => Function(Intrinsic::Length),
    "caie_lower_char" => Function(Intrinsic::LowerChar),
    "caie_upper_char" => Function(Intrinsic::UpperChar),
    "caie_truncate" => Function(Intrinsic::Truncate),
    "caie_parse" => Function(Intrinsic::ParseLegacy),
    "caie_is_numeric" => Function(Intrinsic::IsNumeric),
    "generator_start" => Member(M::GeneratorStart),
    "generator_resume" => Member(M::GeneratorResume),
    "generator_next" => Member(M::GeneratorNext),
    "generator_close" => Member(M::GeneratorClose),
    "generator_is_complete" => Member(M::GeneratorIsComplete),
    "generator_is_yielded" => Member(M::GeneratorIsYielded),
    "generator_yielded" => Member(M::GeneratorYielded),
    "generator_completed" => Member(M::GeneratorCompleted),
    "layout_size" => Member(M::LayoutSize),
    "layout_align" => Member(M::LayoutAlign),
    "rc_clone_member" => Member(M::RcClone),
    "rc_downgrade_member" => Member(M::RcDowngrade),
    "rc_upgrade_member" => Member(M::RcUpgrade),
    "rc_strong_count_member" => Member(M::RcStrongCount),
    "rc_weak_count_member" => Member(M::RcWeakCount),
    "gc_clone_member" => Member(M::GcClone),
    "result_is_ok" => Member(M::ResultIsOk),
    "result_is_err" => Member(M::ResultIsErr),
    "result_unwrap" => Member(M::ResultUnwrap),
    "result_unwrap_err" => Member(M::ResultUnwrapErr),
    "slice" => Member(M::Slice),
    "mutable_slice" => Member(M::MutableSlice),
    "collection_push_back" => Member(M::CollectionPushBack),
    "collection_push_front" => Member(M::CollectionPushFront),
    "collection_pop_back" => Member(M::CollectionPopBack),
    "collection_pop_front" => Member(M::CollectionPopFront),
    "collection_length" => Member(M::CollectionLength),
    "range_reverse" => Member(M::RangeReverse),
    "cell_replace" => Member(M::CellReplace),
    "location_load" => Member(M::LocationLoad),
    "location_store" => Member(M::LocationStore),
    "option_is_some" => Member(M::OptionIsSome),
    "option_is_none" => Member(M::OptionIsNone),
    "option_unwrap" => Member(M::OptionUnwrap),
    "option_or_fail" => Member(M::OptionOrFail),
    "storage_path_field" => Member(M::StoragePathField),
    "storage_path_element" => Member(M::StoragePathElement),
    "storage_path_target" => Member(M::StoragePathTarget),
    "channel_clone" => Member(M::ChannelClone),
    "channel_close" => Member(M::ChannelClose),
    "channel_is_closed" => Member(M::ChannelIsClosed),
    "channel_send" => Member(M::ChannelSend),
    "channel_try_send" => Member(M::ChannelTrySend),
    "channel_receive" => Member(M::ChannelReceive),
    "channel_try_receive" => Member(M::ChannelTryReceive),
    "send_error_value" => Member(M::SendErrorValue),
    "send_error_closed" => Member(M::SendErrorClosed),
    "send_error_full" => Member(M::SendErrorFull),
    "send_error_limit" => Member(M::SendErrorLimit),
    "receive_error_closed" => Member(M::ReceiveErrorClosed),
    "receive_error_empty" => Member(M::ReceiveErrorEmpty),
    "clock_clone" => Member(M::ClockClone),
    "clock_sleep" => Member(M::ClockSleep),
    "task_join" => Member(M::TaskJoin),
    "task_cancel" => Member(M::TaskCancel),
    "task_is_finished" => Member(M::TaskIsFinished),
    "task_completed" => Member(M::TaskCompleted),
    "task_is_completed" => Member(M::TaskIsCompleted),
    "task_is_failed" => Member(M::TaskIsFailed),
    "task_is_cancelled" => Member(M::TaskIsCancelled),
    "task_is_stopped" => Member(M::TaskIsStopped),
    "context_extend" => Member(M::ContextExtend),
    "context_isolate" => Member(M::ContextIsolate),
    "context_service" => Member(M::ContextService),
    "context_provide" => Member(M::ContextProvide),
    "context_available" => Member(M::ContextAvailable),
    "context_dispose" => Member(M::ContextDispose),
    "registration_dispose" => Member(M::RegistrationDispose),
    "context_inject" => Member(M::ContextInject),
    "context_on_dispose" => Member(M::ContextOnDispose),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_keys_are_unique_and_cannot_be_public_paths() {
        let mut known = std::collections::HashSet::new();
        for (key, semantic) in ABI_KEYS {
            assert!(known.insert(*key), "duplicate private ABI key: {key}");
            assert!(key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
            assert_eq!(from_key(key), Some(*semantic));
        }
        assert_eq!(from_key("std::task::spawn"), None);
        assert_eq!(from_key("Task"), None);
        assert_eq!(from_key("unknown_operation"), None);
    }
}
