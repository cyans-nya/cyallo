//! Stable identities for language primitives. Names belong to declaration lookup;
//! checking, lowering and execution use these variants.
use cyallo_ast::{IntrinsicId, NumberKind};
macro_rules! intrinsics {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Debug,Clone,Copy,PartialEq,Eq,Hash)]
        #[repr(u16)]
        pub enum Intrinsic { $($variant),* }
        impl Intrinsic {
            pub const ALL:&'static [Self]=&[$(Self::$variant),*];
            pub fn id(self)->IntrinsicId {IntrinsicId(self as u16)}
            pub fn from_id(id:IntrinsicId)->Option<Self>{Self::ALL.get(id.0 as usize).copied()}
            pub fn diagnostic_name(self)->&'static str {match self{$(Self::$variant=>$name),*}}
        }
    }
}
intrinsics! {
    LayoutNew=>"std::mem::Layout::new",Allocate=>"std::mem::allocate",Deallocate=>"std::mem::deallocate",AllocErrorNew=>"std::mem::AllocError::new",ResultOk=>"std::result::ok",ResultErr=>"std::result::err",RcTryNewIn=>"std::rc::try_new_in",
    Left=>"std::runtime::caie::left",Right=>"std::runtime::caie::right",Mid=>"std::runtime::caie::mid",Length=>"std::runtime::caie::length",LowerChar=>"std::runtime::caie::lcase",UpperChar=>"std::runtime::caie::ucase",
    Truncate=>"std::runtime::caie::int",Random=>"std::random::uniform",CharCode=>"std::char::code",CharFromCode=>"std::char::from_code",StringFrom=>"std::string::from",
    ParseLegacy=>"std::runtime::caie::str_to_num",IsNumeric=>"std::runtime::caie::is_num",Trim=>"std::string::trim",Upper=>"std::string::upper",Lower=>"std::string::lower",
    Contains=>"std::string::contains",StartsWith=>"std::string::starts_with",EndsWith=>"std::string::ends_with",Replace=>"std::string::replace",
    Abs=>"ABS",Min=>"MIN",Max=>"MAX",Clamp=>"CLAMP",NumAbs=>"std::num::abs",NumMin=>"std::num::min",NumMax=>"std::num::max",NumClamp=>"std::num::clamp",Round=>"ROUND",Floor=>"FLOOR",Ceil=>"CEIL",
    Sqrt=>"SQRT",Pow=>"POW",FloatRound=>"std::num::round",FloatFloor=>"std::num::floor",FloatCeil=>"std::num::ceil",FloatSqrt=>"std::num::sqrt",FloatPow=>"std::num::pow",Keys=>"KEYS",Values=>"VALUES",Count=>"COUNT",Sum=>"SUM",
    CellNew=>"std::cell::Cell::new",ArrayEmpty=>"std::array::empty",ArrayFrom=>"std::array::from",
    VecLiteral=>"std::collections::Vec",DequeLiteral=>"std::collections::VecDeque",VecFrom=>"std::collections::Vec::from",DequeFrom=>"std::collections::VecDeque::from",
    OptionSome=>"std::option::some",OptionNone=>"std::option::none",Erase=>"std::any::erase",
    Checked=>"std::any::checked",FreshSymbol=>"std::any::fresh_symbol",LookupPath=>"std::lookup::path",MissingPath=>"std::lookup::missing",
    MappedLookup=>"std::lookup::mapped",ContextNew=>"std::service::Context::new",ServiceKeyNew=>"std::service::ServiceKey::new",Print=>"std::io::print",Input=>"std::runtime::caie::input",Parse=>"std::num::parse",
    CastI8=>"CAST_I8",CastI16=>"CAST_I16",CastI32=>"CAST_I32",CastI64=>"CAST_I64",
    CastU8=>"CAST_U8",CastU16=>"CAST_U16",CastU32=>"CAST_U32",CastU64=>"CAST_U64",
    CastIsize=>"CAST_ISIZE",CastUsize=>"CAST_USIZE",CastF32=>"CAST_F32",CastF64=>"CAST_F64",
    RcClone=>"std::rc::clone",RcDowngrade=>"std::rc::downgrade",RcUpgrade=>"std::rc::upgrade",RcStrongCount=>"std::rc::strong_count",RcWeakCount=>"std::rc::weak_count",GcClone=>"std::gc::clone",DropValue=>"std::mem::drop",
    CloneValue=>"std::mem::clone",FieldLocation=>"std::lookup::field",
    ArrayLength=>"std::array::len",StringLength=>"std::string::len",ArraySum=>"std::array::sum",
}
impl Intrinsic {
    /// Ownership is part of each intrinsic declaration's parameter contract.
    pub fn consumes_argument(self, _index: usize) -> bool {
        matches!(
            self,
            Self::CellNew
                | Self::Allocate
                | Self::Deallocate
                | Self::ResultOk
                | Self::ResultErr
                | Self::RcTryNewIn
                | Self::VecLiteral
                | Self::DequeLiteral
                | Self::OptionSome
                | Self::Erase
                | Self::LookupPath
                | Self::DropValue
        )
    }
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
        && type_args.is_empty()
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
        {
            return Ok(ResolvedCall {
                intrinsic: id,
                result: crate::lookup::storage(StorageKind::Location, ty.clone()),
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
    if id == Intrinsic::RcTryNewIn
        && type_args.is_empty()
        && let [value, allocator] = arguments
    {
        return Ok(ResolvedCall {
            intrinsic: id,
            result: crate::lookup::result_type(
                crate::memory::managed_type(
                    cyallo_ast::MemoryKind::Rc,
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
            ty.number_kind().is_some() || matches!(ty, Type::String | Type::Char | Type::Boolean)
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
        && matches!(
            pointee.as_ref(),
            Type::Array(..)
                | Type::Storage(
                    cyallo_ast::StorageKind::Vec | cyallo_ast::StorageKind::VecDeque,
                    _
                )
        )
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
