//! Stable identities for language primitives. Names belong to declaration lookup;
//! checking, lowering and execution use these variants.
use cyallo_ast::{IntrinsicId, NumberKind};
macro_rules! intrinsics {
    ($($variant:ident),* $(,)?) => {
        #[derive(Debug,Clone,Copy,PartialEq,Eq,Hash)]
        #[repr(u16)]
        pub enum Intrinsic { $($variant),* }
        impl Intrinsic {
            pub const ALL:&'static [Self]=&[$(Self::$variant),*];
            pub fn id(self)->IntrinsicId {IntrinsicId(self as u16, None)}
            pub fn from_id(id:IntrinsicId)->Option<Self>{Self::ALL.get(id.0 as usize).copied()}
            pub fn diagnostic_name(self)->&'static str {match self{$(Self::$variant=>stringify!($variant)),*}}
        }
    }
}
intrinsics! {
    ObjectView,
    ObjectFreeze,
    ModuleLoad,
    ChannelBounded,
    ChannelUnbounded,
    ClockNew,
    TaskSpawn,
    LayoutNew,
    Allocate,
    Deallocate,
    AllocErrorNew,
    ResultOk,
    ResultErr,
    RcTryNewIn,
    Left,
    Right,
    Mid,
    Length,
    LowerChar,
    UpperChar,
    Truncate,
    Random,
    CharCode,
    CharFromCode,
    StringFrom,
    ParseLegacy,
    IsNumeric,
    Trim,
    Upper,
    Lower,
    Contains,
    StartsWith,
    EndsWith,
    Replace,
    Abs,
    Min,
    Max,
    Clamp,
    NumAbs,
    NumMin,
    NumMax,
    NumClamp,
    Round,
    Floor,
    Ceil,
    Sqrt,
    Pow,
    FloatRound,
    FloatFloor,
    FloatCeil,
    FloatSqrt,
    FloatPow,
    Keys,
    Values,
    Count,
    Sum,
    CellNew,
    ArrayEmpty,
    ArrayFrom,
    VecLiteral,
    DequeLiteral,
    VecFrom,
    DequeFrom,
    OptionSome,
    OptionNone,
    Erase,
    Checked,
    FreshSymbol,
    LookupPath,
    MissingPath,
    MappedLookup,
    ContextNew,
    ServiceKeyNew,
    Print,
    Input,
    Parse,
    CastI8,
    CastI16,
    CastI32,
    CastI64,
    CastU8,
    CastU16,
    CastU32,
    CastU64,
    CastIsize,
    CastUsize,
    CastF32,
    CastF64,
    RcClone,
    RcDowngrade,
    RcUpgrade,
    RcStrongCount,
    RcWeakCount,
    GcClone,
    DropValue,
    CloneValue,
    FieldLocation,
    ArrayLength,
    StringLength,
    ArraySum,
}
impl Intrinsic {
    pub fn cast(kind: NumberKind) -> Self {
        use NumberKind::*;
        match kind {
            I8 => Self::CastI8,
            I16 => Self::CastI16,
            I32 => Self::CastI32,
            I64 => Self::CastI64,
            U8 => Self::CastU8,
            U16 => Self::CastU16,
            U32 => Self::CastU32,
            U64 => Self::CastU64,
            Isize => Self::CastIsize,
            Usize => Self::CastUsize,
            F32 => Self::CastF32,
            F64 => Self::CastF64,
        }
    }
    pub fn cast_kind(self) -> Option<NumberKind> {
        Some(match self {
            Self::CastI8 => NumberKind::I8,
            Self::CastI16 => NumberKind::I16,
            Self::CastI32 => NumberKind::I32,
            Self::CastI64 => NumberKind::I64,
            Self::CastU8 => NumberKind::U8,
            Self::CastU16 => NumberKind::U16,
            Self::CastU32 => NumberKind::U32,
            Self::CastU64 => NumberKind::U64,
            Self::CastIsize => NumberKind::Isize,
            Self::CastUsize => NumberKind::Usize,
            Self::CastF32 => NumberKind::F32,
            Self::CastF64 => NumberKind::F64,
            _ => return None,
        })
    }
}
impl std::fmt::Display for Intrinsic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.diagnostic_name())
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Lowering {
    Print,
    Input,
    RcTryNewIn,
    Utility(Intrinsic),
    AddressLength,
    Storage(crate::lookup::Operation),
    Context,
    ServiceKey,
}
#[derive(Debug, Clone)]
pub struct ResolvedCall {
    pub intrinsic: Intrinsic,
    pub result: cyallo_ast::Type,
    pub lowering: Lowering,
}
/// Intrinsics with a compile-time shape contract inspect constant operands here,
/// during type analysis. Backends receive only the resolved operation and type.
pub fn resolve_at_call(
    id: Intrinsic,
    type_args: &[cyallo_ast::Type],
    arguments: &[cyallo_ast::Expr],
    types: &[cyallo_ast::Type],
    span: cyallo_ast::Span,
) -> Result<ResolvedCall, cyallo_ast::Diagnostic> {
    use cyallo_ast::{ExprKind, Literal, Operator, StorageKind, Type};
    if id == Intrinsic::FieldLocation
        && let [declared] = type_args
        && let [owner, Type::String] = types
    {
        let owner = match owner {
            Type::Reference { pointee, .. } => pointee.as_ref(),
            _ => owner,
        };
        if let Type::Managed {
            kind: cyallo_ast::MemoryKind::Gc,
            pointee,
            ..
        } = owner
            && let Type::Record(_, fields) = pointee.as_ref()
            && let Some(field) = arguments.get(1).and_then(cyallo_ast::constant_text)
            && let Some((index, (_, ty))) = fields
                .iter()
                .enumerate()
                .find(|(_, (name, _))| name.as_ref() == field)
            && matches!(declared, Type::Opaque(_, applied, _) if matches!(applied.as_slice(), [element] if crate::declarations::equivalent_types(element, ty)))
        {
            return Ok(ResolvedCall {
                intrinsic: id,
                result: declared.clone(),
                lowering: Lowering::Storage(crate::lookup::Operation::Field(index)),
            });
        }
        return Err(cyallo_ast::Diagnostic::new(
            "E109",
            "retained locations require a declared field of a GC-owned record",
            span,
        ));
    }
    if id == Intrinsic::ArrayFrom && type_args.is_empty() {
        fn number(expression: &cyallo_ast::Expr) -> Option<i128> {
            match &expression.kind {
                ExprKind::Literal(Literal::Integer(value)) => Some(*value as i128),
                ExprKind::Literal(Literal::Number(value)) if !value.kind.float() => {
                    Some(value.int())
                }
                ExprKind::Parenthesized(inner) => number(inner),
                _ => None,
            }
        }
        if let [argument] = arguments
            && let [Type::Storage(StorageKind::Range, element)] = types
            && let ExprKind::Binary(
                operator @ (Operator::Range | Operator::RangeInclusive),
                start,
                end,
            ) = &argument.kind
            && let (Some(start), Some(end)) = (number(start), number(end))
        {
            let length = (end - start + i128::from(*operator == Operator::RangeInclusive)).max(0);
            let length = i64::try_from(length).map_err(|_| {
                cyallo_ast::Diagnostic::new(
                    "E109",
                    "fixed array length exceeds its supported range",
                    span,
                )
            })?;
            return Ok(ResolvedCall {
                intrinsic: id,
                result: Type::Array(vec![(0, length - 1)], element.clone()),
                lowering: Lowering::Storage(crate::lookup::Operation::ArrayFrom),
            });
        }
        return Err(cyallo_ast::Diagnostic::new(
            "E109",
            "fixed array construction requires a range with compile-time bounds",
            span,
        ));
    }
    resolve(id, type_args, types, span)
}
/// Instantiate a declaration against resolved type arguments and operand types.
pub fn resolve(
    id: Intrinsic,
    type_args: &[cyallo_ast::Type],
    arguments: &[cyallo_ast::Type],
    span: cyallo_ast::Span,
) -> Result<ResolvedCall, cyallo_ast::Diagnostic> {
    use cyallo_ast::Type;
    if id == Intrinsic::ObjectView
        && let ([view @ Type::Object(_)], [Type::Dynamic]) = (type_args, arguments)
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: crate::lookup::result_type(
                view.clone(),
                crate::lookup::storage(cyallo_ast::StorageKind::ViewError, Type::Void),
            ),
            lowering: Lowering::Storage(crate::lookup::Operation::ObjectView),
        });
    }
    if id == Intrinsic::ObjectFreeze
        && let [ty] = arguments
        && crate::dynamic::object(ty.pointee_or_self())
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: ty.clone(),
            lowering: Lowering::Storage(crate::lookup::Operation::ObjectFreeze),
        });
    }
    if id == Intrinsic::ModuleLoad && type_args.is_empty() && arguments == [Type::String] {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: crate::lookup::result_type(
                Type::Dynamic,
                crate::lookup::storage(cyallo_ast::StorageKind::LoadError, Type::Void),
            ),
            lowering: Lowering::Storage(crate::lookup::Operation::ModuleLoad),
        });
    }
    if crate::concurrent::is_intrinsic(id) {
        return crate::concurrent::resolve(id, type_args, arguments, span);
    }
    if id == Intrinsic::RcTryNewIn
        && type_args.is_empty()
        && let [value, allocator] = arguments
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: crate::lookup::result_type(
                crate::memory::managed_type(
                    cyallo_ast::MemoryKind::Rc,
                    cyallo_ast::Locality::Local,
                    Some(allocator),
                    value.clone(),
                ),
                crate::memory::error(),
            ),
            lowering: Lowering::RcTryNewIn,
        });
    }
    if let Some((operation, result)) = crate::memory::resolve(id, type_args, arguments) {
        return Ok(ResolvedCall {
            intrinsic: id,
            result,
            lowering: Lowering::Storage(operation),
        });
    }
    let invalid = || {
        let found = arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        cyallo_ast::Diagnostic::new("E112", format!("`{id}` does not accept ({found})"), span)
    };
    if id == Intrinsic::Print
        && type_args.is_empty()
        && arguments.iter().all(|ty| {
            ty.number_kind().is_some()
                || matches!(
                    ty,
                    Type::String | Type::Char | Type::Boolean | Type::Dynamic | Type::Object(_)
                )
        })
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: Type::Void,
            lowering: Lowering::Print,
        });
    }
    if id == Intrinsic::Input
        && type_args.is_empty()
        && let [
            Type::Reference {
                mutable: true,
                pointee,
                ..
            },
        ] = arguments
        && (pointee.number_kind().is_some()
            || matches!(pointee.as_ref(), Type::String | Type::Char | Type::Boolean))
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: Type::Void,
            lowering: Lowering::Input,
        });
    }
    if type_args.is_empty()
        && id == Intrinsic::ArrayLength
        && let [Type::Reference { pointee, .. }] = arguments
        && (matches!(pointee.as_ref(), Type::Array(..)) || pointee.collection_element().is_some())
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: Type::Integer,
            lowering: Lowering::AddressLength,
        });
    }
    if id == Intrinsic::ContextNew && type_args.is_empty() && arguments.is_empty() {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: Type::Context,
            lowering: Lowering::Context,
        });
    }
    if id == Intrinsic::ServiceKeyNew {
        if let [ty @ Type::TraitObject(_)] = type_args
            && arguments == [Type::String]
        {
            return Ok(ResolvedCall {
                intrinsic: id,
                result: Type::ServiceKey(Box::new(ty.clone())),
                lowering: Lowering::ServiceKey,
            });
        }
        return Err(invalid());
    }
    if id == Intrinsic::Parse {
        if let [ty] = type_args
            && let Some(kind) = ty.number_kind()
            && arguments == [Type::String]
        {
            return Ok(ResolvedCall {
                intrinsic: id,
                result: crate::lookup::result_type(
                    ty.clone(),
                    Type::Storage(cyallo_ast::StorageKind::ParseError, Box::new(Type::Void)),
                ),
                lowering: Lowering::Storage(crate::lookup::Operation::Parse(kind)),
            });
        }
        return Err(invalid());
    }
    // A collection literal: the head names the declared collection type, an optional
    // second type argument fixes the element type, otherwise the operands decide.
    if matches!(id, Intrinsic::VecLiteral | Intrinsic::DequeLiteral) {
        let (head, explicit) = match type_args {
            [head] => (head, None),
            [head, element] => (head, Some(element)),
            _ => return Err(invalid()),
        };
        let element = explicit.or(arguments.first()).ok_or_else(invalid)?.clone();
        return match head {
            Type::Opaque(target, applied, traits)
                if applied.is_empty()
                    && traits.collection()
                    && traits.double_ended() == (id == Intrinsic::DequeLiteral)
                    && arguments
                        .iter()
                        .all(|argument| crate::memory::same_regions(argument, &element)) =>
            {
                Ok(ResolvedCall {
                    intrinsic: id,
                    result: Type::Opaque(target.clone(), vec![element], *traits),
                    lowering: Lowering::Storage(crate::lookup::Operation::CollectionLiteral(
                        id == Intrinsic::DequeLiteral,
                    )),
                })
            }
            _ => Err(invalid()),
        };
    }
    let storage = if type_args.is_empty() {
        crate::lookup::call(id, arguments)
    } else if let [ty] = type_args {
        crate::lookup::constructor(id, ty, arguments)
    } else {
        None
    };
    if let Some((op, result)) = storage {
        return Ok(ResolvedCall {
            intrinsic: id,
            result,
            lowering: Lowering::Storage(op),
        });
    }
    if type_args.is_empty()
        && let Some(result) = crate::builtins::result_type(id, arguments)
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result,
            lowering: Lowering::Utility(id),
        });
    }
    Err(invalid())
}

/// Checks the actual source declaration, then validates its native ABI
/// specialization. Generic substitutions belong to the declaration; their
/// ordering is independent of an operation's internal specialization inputs.
pub fn resolve_declared(
    reference: IntrinsicId,
    env: &crate::declarations::TypeEnvironment,
    type_args: &[cyallo_ast::Type],
    arguments: &[cyallo_ast::Expr],
    types: &[cyallo_ast::Type],
    span: cyallo_ast::Span,
) -> Result<ResolvedCall, cyallo_ast::Diagnostic> {
    use crate::declarations::{DeclarationError, invalid_declaration};
    use crate::internal::InternalSemantic;
    use cyallo_ast::Type;
    let id = Intrinsic::from_id(reference)
        .ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, span))?;
    let Some(declaration) = reference.1 else {
        return resolve_at_call(id, type_args, arguments, types, span);
    };
    env.check_native_access(declaration, span)?;
    let contract =
        crate::native_contract::function(env, declaration, type_args, arguments, types, span)?;
    if contract.semantic != InternalSemantic::Function(id) {
        return Err(invalid_declaration(
            DeclarationError::InvalidSignature,
            span,
        ));
    }
    let result_arguments = || {
        crate::lookup::result_payload(&contract.result)
            .map(|(value, error)| vec![value.clone(), error.clone()])
    };
    let specialization = match id {
        Intrinsic::ResultOk | Intrinsic::ResultErr => result_arguments(),
        Intrinsic::ObjectView | Intrinsic::Parse => result_arguments()
            .and_then(|types| types.first().cloned())
            .map(|ty| vec![ty]),
        Intrinsic::OptionNone => {
            crate::lookup::option_payload(&contract.result).map(|ty| vec![ty.clone()])
        }
        Intrinsic::Checked => Some(vec![contract.result.clone()]),
        // A host constructor must declare an opaque host type as its result; any other
        // declared result is an invalid signature for the operation.
        Intrinsic::LayoutNew | Intrinsic::Allocate => Some(vec![contract.result.clone()]),
        Intrinsic::VecFrom | Intrinsic::DequeFrom => match &contract.result {
            Type::Opaque(..) => Some(vec![contract.result.clone()]),
            _ => None,
        },
        Intrinsic::ClockNew | Intrinsic::TaskSpawn | Intrinsic::RcDowngrade => {
            match &contract.result {
                Type::Opaque(..) => Some(vec![contract.result.clone()]),
                _ => None,
            }
        }
        // Constructors of opaque host types take the declared type; the operation
        // validates its shape.
        Intrinsic::MissingPath
        | Intrinsic::CellNew
        | Intrinsic::LookupPath
        | Intrinsic::FieldLocation => match &contract.result {
            Type::Opaque(..) => Some(vec![contract.result.clone()]),
            _ => None,
        },
        Intrinsic::ArrayEmpty => match &contract.result {
            Type::Array(_, element) => Some(vec![*element.clone()]),
            _ => None,
        },
        Intrinsic::ServiceKeyNew => match &contract.result {
            Type::ServiceKey(ty) => Some(vec![*ty.clone()]),
            _ => None,
        },
        Intrinsic::ChannelBounded | Intrinsic::ChannelUnbounded => match &contract.result {
            Type::Tuple(types) => match types.first() {
                Some(Type::Opaque(_, arguments, _)) if arguments.len() == 1 => {
                    Some(vec![arguments[0].clone(), contract.result.clone()])
                }
                _ => None,
            },
            _ => None,
        },
        _ => Some(vec![]),
    }
    .ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, span))?;
    let mut resolved = resolve_at_call(id, &specialization, arguments, types, span)?;
    if !crate::declarations::equivalent_types(&resolved.result, &contract.result) {
        return Err(invalid_declaration(
            DeclarationError::InvalidSignature,
            span,
        ));
    }
    resolved.result = contract.result;
    Ok(resolved)
}
