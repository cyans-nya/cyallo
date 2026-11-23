//! Compilation-owned declaration contracts. Public spellings are lookup keys;
//! only validated private ABI identities select native semantics.
use crate::{
    internal::{
        self, InternalSemantic, MemberIntrinsic, NativeTrait, NativeType, NativeTypeFamily,
    },
    intrinsic::Intrinsic,
};
use cyallo_ast::*;
use std::{
    collections::HashMap,
    fmt,
    ops::{Deref, DerefMut},
    sync::Arc,
};

pub use cyallo_ast::SourceTrust;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Enum,
    Variant,
    Module,
    Record,
    Trait,
    Function,
    Member,
}
impl fmt::Display for DeclarationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enum => "enum",
            Self::Variant => "enum variant",
            Self::Module => "module",
            Self::Record => "record",
            Self::Trait => "trait",
            Self::Function => "function",
            Self::Member => "member",
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationError {
    UntrustedInternal,
    UnknownKey(Box<str>),
    DuplicateAnnotation,
    WrongKind {
        kind: DeclarationKind,
    },
    GenericArity {
        expected: usize,
        actual: usize,
    },
    OpaqueFields,
    OpaqueConstruction,
    /// A source `opaque record` has no fields; only its implementer can make values.
    OpaqueRecordConstruction,
    UnresolvedInterface(Box<str>),
    NotInterfaceModule(Box<str>),
    UnresolvedImplementer(Box<str>),
    NotOpaqueType {
        interface: Box<str>,
        name: Box<str>,
    },
    DuplicateOpaqueBinding(Box<str>),
    MissingOpaqueBinding {
        interface: Box<str>,
        name: Box<str>,
    },
    GenericOpaqueBinding(Box<str>),
    UnresolvedWorld(Box<str>),
    UnresolvedWorldImport(Box<str>),
    CyclicWorldInclude(Box<str>),
    DuplicateWorldField(Box<str>),
    DuplicateWorldExport(Box<str>),
    /// A declaration has both a source annotation and a provider binding.
    BindingConflict(Box<str>),
    /// A provider binding names a declaration the package does not contain.
    UnusedBinding(Box<str>),
    WorldMemberMissing {
        module: Box<str>,
        name: Box<str>,
    },
    WorldMemberMismatch {
        module: Box<str>,
        name: Box<str>,
    },
    BodyRequired,
    NativeBodyForbidden,
    DuplicateCanonical,
    InvalidSignature,
    UnresolvedOwner,
    InvalidPack,
    UnsatisfiedTrait {
        ty: Type,
        bound: Type,
    },
    MissingCallerScope,
    OwnershipContract {
        index: usize,
        required: NativeOperandMode,
        passing: ParameterPassing,
        ty: Type,
    },
    OperandAddress {
        index: usize,
        mutable: bool,
        ty: Type,
    },
    OperandDescriptor {
        index: usize,
        ty: Type,
    },
    UnsupportedSourceFeature(SourceFeature),
    AllocatorContract(AllocatorContractError),
    NativePredicateMethods,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeOperandMode {
    Read,
    Transfer,
    SharedAddress,
    MutableAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFeature {
    GenericConstraints,
    DescriptorRead,
    ParameterPack,
    NativeTypeFamily,
    GenericTrait,
    GenericTraitMethod,
}

impl fmt::Display for SourceFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GenericConstraints => "generic constraints",
            Self::DescriptorRead => "read parameter passing",
            Self::ParameterPack => "parameter packs",
            Self::NativeTypeFamily => "native type families",
            Self::GenericTrait => "generic traits",
            Self::GenericTraitMethod => "generic trait methods",
        })
    }
}

impl fmt::Display for NativeOperandMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read-only",
            Self::Transfer => "transferred",
            Self::SharedAddress => "borrowed",
            Self::MutableAddress => "mutably borrowed",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorContractError {
    GenericParameters,
    MethodCount,
    AllocateShape,
    DeallocateShape,
}

impl fmt::Display for AllocatorContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GenericParameters => "the allocator trait cannot have generic parameters",
            Self::MethodCount => "the allocator trait requires exactly two ordered methods",
            Self::AllocateShape => "the first method must be a bodyless proc(&mut Self, Layout) -> Result<Allocation, AllocError>",
            Self::DeallocateShape => "the second method must be a bodyless proc(&mut Self, Allocation) -> () with ownership transfer",
        })
    }
}
impl std::error::Error for AllocatorContractError {}
impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UntrustedInternal => {
                f.write_str("internal annotations require loader-issued core source provenance")
            }
            Self::UnknownKey(key) => write!(f, "unknown internal ABI key `{key}`"),
            Self::DuplicateAnnotation => {
                f.write_str("a declaration may have only one internal ABI annotation")
            }
            Self::WrongKind { kind } => {
                write!(f, "internal ABI key is not valid on a {kind} declaration")
            }
            Self::GenericArity { expected, actual } => write!(
                f,
                "native declaration requires {expected} generic parameters, found {actual}"
            ),
            Self::OpaqueFields => f.write_str("a native representation declaration must be opaque"),
            Self::OpaqueConstruction => {
                f.write_str("native storage cannot be fabricated with a record literal")
            }
            Self::OpaqueRecordConstruction => {
                f.write_str("an opaque record cannot be constructed with a record literal")
            }
            Self::UnresolvedInterface(path) => {
                write!(f, "unresolved interface module `{path}`")
            }
            Self::NotInterfaceModule(path) => {
                write!(f, "`{path}` is not an `interface mod`")
            }
            Self::UnresolvedImplementer(path) => {
                write!(f, "unresolved implementing module `{path}`")
            }
            Self::NotOpaqueType { interface, name } => {
                write!(
                    f,
                    "`{name}` is not an opaque record of interface `{interface}`"
                )
            }
            Self::DuplicateOpaqueBinding(name) => {
                write!(
                    f,
                    "opaque type `{name}` is already bound by another implementation"
                )
            }
            Self::MissingOpaqueBinding { interface, name } => write!(
                f,
                "implementations of interface `{interface}` leave opaque type `{name}` unbound"
            ),
            Self::GenericOpaqueBinding(name) => write!(
                f,
                "generic opaque type `{name}` cannot be bound by an `impl interface` yet"
            ),
            Self::BindingConflict(path) => write!(
                f,
                "`{path}` has both a source annotation and a provider binding"
            ),
            Self::UnusedBinding(path) => {
                write!(
                    f,
                    "the provider binds `{path}`, which the package does not declare"
                )
            }
            Self::UnresolvedWorld(path) => write!(f, "unresolved world `{path}`"),
            Self::UnresolvedWorldImport(path) => {
                write!(f, "world import `{path}` does not name a module")
            }
            Self::CyclicWorldInclude(name) => write!(f, "world `{name}` includes itself"),
            Self::DuplicateWorldField(name) => write!(
                f,
                "two imports are named `{name}`; give one an alias with `import path as name`"
            ),
            Self::DuplicateWorldExport(name) => {
                write!(f, "world export `{name}` is declared more than once")
            }
            Self::WorldMemberMissing { module, name } => {
                write!(f, "module `{module}` has no accessible routine `{name}`")
            }
            Self::WorldMemberMismatch { module, name } => write!(
                f,
                "the signature of `{name}` in the world differs from the one in `{module}`"
            ),
            Self::BodyRequired => f.write_str("source callable requires an implementation body"),
            Self::NativeBodyForbidden => {
                f.write_str("native callable requires a bodyless declaration")
            }
            Self::DuplicateCanonical => f.write_str("a canonical native type is already declared"),
            Self::InvalidSignature => {
                f.write_str("declaration signature is incompatible with its native ABI")
            }
            Self::UnresolvedOwner => {
                f.write_str("inherent implementation requires a resolved nominal owner")
            }
            Self::InvalidPack => f.write_str(
                "a native parameter pack must be last and reference a declared type pack",
            ),
            Self::UnsatisfiedTrait { ty, bound } => {
                write!(f, "`{ty}` does not implement required trait `{bound}`")
            }
            Self::MissingCallerScope => {
                f.write_str("native call is missing its resolved module scope")
            }
            Self::OwnershipContract {
                index,
                required,
                passing,
                ty,
            } => write!(
                f,
                "native operand {} requires {required} ownership, but its source declaration uses {passing} passing for potentially owning `{ty}`",
                index + 1
            ),
            Self::OperandAddress { index, mutable, ty } => write!(
                f,
                "native operand {} requires an explicit {} reference, found `{ty}`",
                index + 1,
                if *mutable {
                    "mutable"
                } else {
                    "shared or mutable"
                }
            ),
            Self::OperandDescriptor { index, ty } => write!(
                f,
                "native operand {} requires a value descriptor rather than a reference, found `{ty}`",
                index + 1
            ),
            Self::UnsupportedSourceFeature(feature) => write!(
                f,
                "{feature} is not implemented for ordinary source declarations"
            ),
            Self::AllocatorContract(error) => {
                write!(f, "allocator native ABI contract is invalid: {error}")
            }
            Self::NativePredicateMethods => {
                f.write_str("native predicate traits must be opaque and have no methods")
            }
        }
    }
}
impl std::error::Error for DeclarationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocatorContract(error) => Some(error),
            _ => None,
        }
    }
}
pub fn invalid_declaration(error: DeclarationError, span: Span) -> Diagnostic {
    Diagnostic::new("E173", error.to_string(), span)
}
pub fn bind(
    annotations: &[DeclarationAnnotation],
    trust: SourceTrust,
    kind: DeclarationKind,
) -> Result<Option<InternalSemantic>, Diagnostic> {
    let annotations: Vec<_> = annotations
        .iter()
        .filter(|a| a.name.as_ref() == "cyallo_internal")
        .collect();
    let Some(annotation) = annotations.first() else {
        return Ok(None);
    };
    if trust != SourceTrust::Core {
        return Err(invalid_declaration(
            DeclarationError::UntrustedInternal,
            annotation.span,
        ));
    }
    if annotations.len() != 1 {
        return Err(invalid_declaration(
            DeclarationError::DuplicateAnnotation,
            annotations[1].span,
        )
        .with_related(annotation.span, "first annotation"));
    }
    let semantic = internal::from_key(&annotation.value).ok_or_else(|| {
        invalid_declaration(
            DeclarationError::UnknownKey(annotation.value.clone()),
            annotation.span,
        )
    })?;
    let valid = matches!(
        (kind, semantic),
        (DeclarationKind::Variant, InternalSemantic::Variant(_))
            | (
                DeclarationKind::Enum,
                InternalSemantic::Type(NativeType::Storage(
                    StorageKind::Option
                        | StorageKind::Result
                        | StorageKind::GeneratorStep
                        | StorageKind::SendError
                ))
            )
            | (DeclarationKind::Module, InternalSemantic::Prelude)
            | (
                DeclarationKind::Record,
                InternalSemantic::Type(_) | InternalSemantic::TypeFamily(_)
            )
            | (
                DeclarationKind::Trait,
                InternalSemantic::Trait(_) | InternalSemantic::AllocatorTrait
            )
            | (DeclarationKind::Function, InternalSemantic::Function(_))
            | (
                DeclarationKind::Member,
                InternalSemantic::Member(_) | InternalSemantic::Function(_)
            )
    );
    if !valid
        || matches!(
            (kind, semantic),
            (
                DeclarationKind::Record,
                InternalSemantic::Type(NativeType::Storage(
                    StorageKind::Option
                        | StorageKind::Result
                        | StorageKind::GeneratorStep
                        | StorageKind::SendError
                ))
            )
        )
    {
        return Err(invalid_declaration(
            DeclarationError::WrongKind { kind },
            annotation.span,
        ));
    }
    Ok(Some(semantic))
}
pub fn type_arity(ty: NativeType) -> usize {
    use NativeType::*;
    match ty {
        Storage(StorageKind::Result | StorageKind::GeneratorStep) => 2,
        Storage(
            StorageKind::AllocError
            | StorageKind::TypeDescriptor
            | StorageKind::ViewError
            | StorageKind::LoadError
            | StorageKind::ParseError,
        )
        | Any
        | Symbol
        | Context
        | Registration => 0,
        Storage(_) | ServiceKey | ServiceRef => 1,
        // Declared arity, checked against the declaration itself.
        Opaque(_) => 0,
    }
}
pub fn family_arity(family: NativeTypeFamily) -> usize {
    match family {
        NativeTypeFamily::Field | NativeTypeFamily::AllocatedRc => 2,
        _ => 1,
    }
}
pub fn trait_arity(bound: NativeTrait) -> usize {
    match bound {
        NativeTrait::Collection | NativeTrait::NumericCollection => 1,
        _ => 0,
    }
}

/// Fixed ownership at the compiler/runtime ABI boundary. This never resolves a
/// public name: the parsed declaration has already supplied its semantic ID.
fn operand_mode(semantic: InternalSemantic, index: usize) -> Option<NativeOperandMode> {
    use NativeOperandMode::*;
    Some(match semantic {
        InternalSemantic::Function(Intrinsic::Input) if index == 0 => MutableAddress,
        InternalSemantic::Function(Intrinsic::Checked) => Transfer,
        InternalSemantic::Function(intrinsic) => {
            if intrinsic.consumes_argument(index) {
                Transfer
            } else {
                Read
            }
        }
        InternalSemantic::Member(member) => member.contract().operands.mode(index),
        _ => return None,
    })
}

fn potentially_owning(ty: &Type, nonowning_parameter: &impl Fn(&str) -> bool) -> bool {
    if crate::ownership::copy(ty) {
        return false;
    }
    match ty {
        Type::Enum(contract) => contract
            .variants
            .iter()
            .filter_map(|v| v.payload.as_ref())
            .any(|ty| potentially_owning(ty, nonowning_parameter)),
        Type::Named(name) => !nonowning_parameter(name),
        Type::RecordRef(_)
        | Type::Any
        | Type::SelfType
        | Type::ValueParameter(_)
        | Type::Dependent(..)
        | Type::Applied(..) => true,
        Type::Reference { .. } | Type::Borrowed(_) | Type::ServiceKey(_) => false,
        Type::Function(..)
        | Type::Context
        | Type::Registration
        | Type::ServiceRef(_)
        | Type::TraitObject(_) => true,
        Type::Managed {
            kind: MemoryKind::Rc,
            ..
        } => true,
        Type::Storage(
            StorageKind::Range
            | StorageKind::AllocError
            | StorageKind::TypeDescriptor
            | StorageKind::ViewError
            | StorageKind::LoadError
            | StorageKind::ParseError,
            _,
        ) => false,
        Type::Opaque(_, arguments, traits) => {
            traits.resource()
                || arguments
                    .iter()
                    .any(|ty| potentially_owning(ty, nonowning_parameter))
        }
        Type::Storage(_, inner)
        | Type::Array(_, inner)
        | Type::SparseArray(inner)
        | Type::Slice(inner)
        | Type::Managed { pointee: inner, .. } => potentially_owning(inner, nonowning_parameter),
        Type::Tuple(types) => types
            .iter()
            .any(|ty| potentially_owning(ty, nonowning_parameter)),
        Type::Record(_, fields) | Type::AnonymousRecord(fields) => fields
            .iter()
            .any(|(_, ty)| potentially_owning(ty, nonowning_parameter)),
        _ => false,
    }
}

fn validate_operand_modes(
    semantic: InternalSemantic,
    params: &[Param],
    span: Span,
    nonowning_parameter: impl Fn(&str) -> bool,
) -> Result<(), Diagnostic> {
    for (index, parameter) in params.iter().enumerate() {
        let required = operand_mode(semantic, index)
            .ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, span))?;
        if parameter.receiver
            && matches!(parameter.ty, Type::Reference { .. })
            && (required == NativeOperandMode::Transfer
                || semantic == InternalSemantic::Member(MemberIntrinsic::RangeReverse))
        {
            return Err(invalid_declaration(
                DeclarationError::OperandDescriptor {
                    index,
                    ty: parameter.ty.clone(),
                },
                parameter.span,
            ));
        }
        if matches!(
            required,
            NativeOperandMode::SharedAddress | NativeOperandMode::MutableAddress
        ) {
            let mutable = required == NativeOperandMode::MutableAddress;
            if !matches!(parameter.ty, Type::Reference { mutable: supplied, .. } if !mutable || supplied)
                || parameter.by_ref
            {
                return Err(invalid_declaration(
                    DeclarationError::OperandAddress {
                        index,
                        mutable,
                        ty: parameter.ty.clone(),
                    },
                    parameter.span,
                ));
            }
        } else if potentially_owning(&parameter.ty, &nonowning_parameter) {
            let transfers = parameter.passing == ParameterPassing::Value && !parameter.by_ref;
            if transfers != (required == NativeOperandMode::Transfer) {
                return Err(invalid_declaration(
                    DeclarationError::OwnershipContract {
                        index,
                        required,
                        passing: parameter.passing,
                        ty: parameter.ty.clone(),
                    },
                    parameter.span,
                ));
            }
        }
    }
    Ok(())
}

/// Checks fully instantiated parameters against the VM's ownership ABI.
pub fn validate_native_ownership(
    semantic: InternalSemantic,
    params: &[Param],
    span: Span,
) -> Result<(), Diagnostic> {
    validate_operand_modes(semantic, params, span, |_| false)
}

pub fn internal_annotation(
    annotations: &[DeclarationAnnotation],
) -> Option<&DeclarationAnnotation> {
    annotations
        .iter()
        .find(|a| a.name.as_ref() == "cyallo_internal")
}
fn semantic_of(function: &Function) -> Result<InternalSemantic, Diagnostic> {
    let mut annotations = function
        .annotations
        .iter()
        .filter(|a| a.name.as_ref() == "cyallo_internal");
    let semantic = annotations
        .next()
        .and_then(|a| internal::from_key(&a.value));
    if annotations.next().is_some() {
        return Err(invalid_declaration(
            DeclarationError::InvalidSignature,
            function.span,
        ));
    }
    semantic.ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, function.span))
}

fn validate_native_header(
    function: &Function,
    native: &NativeDeclarations,
) -> Result<(), Diagnostic> {
    if function.body_kind != FunctionBodyKind::Declaration || !function.body.is_empty() {
        return Err(invalid_declaration(
            DeclarationError::NativeBodyForbidden,
            function.span,
        ));
    }
    let nonowning_parameter = |parameter: &str| {
        function
            .constraints
            .iter()
            .filter(|constraint| constraint.parameter.as_ref() == parameter)
            .flat_map(|constraint| &constraint.bounds)
            .any(|bound| {
                let name = match bound {
                    Type::Named(name) | Type::Applied(name, _) => name,
                    _ => return false,
                };
                native
                    .traits
                    .iter()
                    .find(|(_, declaration)| declaration.name == *name)
                    .and_then(|(_, declaration)| internal_annotation(&declaration.annotations))
                    .and_then(|annotation| internal::from_key(&annotation.value))
                    .is_some_and(|semantic| {
                        matches!(
                            semantic,
                            InternalSemantic::Trait(
                                NativeTrait::ObjectLike
                                    | NativeTrait::Numeric
                                    | NativeTrait::Integer
                                    | NativeTrait::Floating
                                    | NativeTrait::Format
                                    | NativeTrait::LegacyNumeric
                                    | NativeTrait::GcOwner
                                    | NativeTrait::GcView
                            )
                        )
                    })
            })
    };
    validate_operand_modes(
        semantic_of(function)?,
        &function.params,
        function.span,
        nonowning_parameter,
    )
}

/// Rejects malformed trusted contracts before any call can specialize them.
pub fn validate_native_declarations(native: &NativeDeclarations) -> Result<(), Diagnostic> {
    crate::enums::validate_native_enums(native)?;
    for declaration in &native.functions {
        validate_native_header(&declaration.function, native)?;
    }
    for declaration in &native.methods {
        validate_native_header(&declaration.function, native)?;
    }
    for (_, declaration) in &native.traits {
        match internal_annotation(&declaration.annotations)
            .and_then(|annotation| internal::from_key(&annotation.value))
        {
            Some(InternalSemantic::AllocatorTrait) => validate_allocator_trait(declaration)?,
            Some(InternalSemantic::Trait(bound)) => {
                let expected = trait_arity(bound);
                if declaration.type_params.len() != expected {
                    return Err(invalid_declaration(
                        DeclarationError::GenericArity {
                            expected,
                            actual: declaration.type_params.len(),
                        },
                        declaration.span,
                    ));
                }
                if !declaration.methods.is_empty() {
                    return Err(invalid_declaration(
                        DeclarationError::NativePredicateMethods,
                        declaration.span,
                    ));
                }
            }
            _ => {
                return Err(invalid_declaration(
                    DeclarationError::InvalidSignature,
                    declaration.span,
                ));
            }
        }
    }
    Ok(())
}

/// Allocator calls use positional ABI slots. Validate their semantic shapes,
/// including order, rather than trusting method names or declaration labels.
pub fn validate_allocator_trait(declaration: &Trait) -> Result<(), Diagnostic> {
    allocator_contract(declaration).map_err(|error| {
        invalid_declaration(DeclarationError::AllocatorContract(error), declaration.span)
    })
}

pub fn allocator_contract(declaration: &Trait) -> Result<(), AllocatorContractError> {
    if !declaration.type_params.is_empty() {
        return Err(AllocatorContractError::GenericParameters);
    }
    let [allocate, deallocate] = declaration.methods.as_slice() else {
        return Err(AllocatorContractError::MethodCount);
    };
    let common = |function: &Function| {
        function.kind == RoutineKind::Procedure
            && !function.generator
            && function.body_kind == FunctionBodyKind::Declaration
            && function.body.is_empty()
            && internal_annotation(&function.annotations).is_none()
            && function.type_params.is_empty()
            && function.constraints.is_empty()
            && function.type_packs.is_empty()
            && function.params.len() == 2
            && function.params.iter().all(|parameter| {
                parameter.passing == ParameterPassing::Value
                    && parameter.arity == ParameterArity::One
                    && !parameter.by_ref
            })
            && function.params[0].receiver
            && !function.params[1].receiver
            && matches!(&function.params[0].ty, Type::Reference {mutable: true, pointee, ..} if pointee.as_ref() == &Type::SelfType)
    };
    // The protocol is stated by roles, not names: `allocate` takes a layout and
    // returns `Result<allocation, AllocError>`; `deallocate` takes an allocation.
    let role = |ty: &Type, wanted: fn(&cyallo_ast::OpaqueTraits) -> bool| matches!(ty, Type::Opaque(_, args, traits) if args.is_empty() && wanted(traits));
    if !common(allocate)
        || !role(&allocate.params[1].ty, |t| t.layout())
        || !crate::memory::allocator_result_allocation(&allocate.result)
    {
        return Err(AllocatorContractError::AllocateShape);
    }
    if !common(deallocate)
        || !role(&deallocate.params[1].ty, |t| t.allocation())
        || !equivalent_types(&deallocate.result, &Type::Void)
    {
        return Err(AllocatorContractError::DeallocateShape);
    }
    Ok(())
}

pub(crate) fn unsupported_source(feature: SourceFeature, span: Span) -> Diagnostic {
    invalid_declaration(DeclarationError::UnsupportedSourceFeature(feature), span)
}

fn validate_source_type(ty: &Type, span: Span) -> Result<(), Diagnostic> {
    let mut ty = ty.clone();
    let mut family = false;
    cyallo_ast::visit::visit_type(&mut ty, &mut |ty| {
        family |= matches!(ty, Type::Dependent(..) | Type::ValueParameter(_))
    });
    if family {
        return Err(unsupported_source(SourceFeature::NativeTypeFamily, span));
    }
    Ok(())
}

/// Native-only signature syntax must never disappear through ordinary source
/// monomorphization or trait preparation without having been checked.
pub fn validate_source_function(function: &Function) -> Result<(), Diagnostic> {
    if !function.constraints.is_empty() {
        return Err(unsupported_source(
            SourceFeature::GenericConstraints,
            function.span,
        ));
    }
    if !function.type_packs.is_empty()
        || function
            .params
            .iter()
            .any(|parameter| parameter.arity != ParameterArity::One)
    {
        return Err(unsupported_source(
            SourceFeature::ParameterPack,
            function.span,
        ));
    }
    for parameter in &function.params {
        if parameter.passing == ParameterPassing::Read {
            return Err(unsupported_source(
                SourceFeature::DescriptorRead,
                parameter.span,
            ));
        }
        validate_source_type(&parameter.ty, parameter.span)?;
    }
    validate_source_type(&function.result, function.span)?;
    validate_source_block(&function.body)
}

fn validate_source_block(body: &[Stmt]) -> Result<(), Diagnostic> {
    let mut body = body.to_vec();
    cyallo_ast::visit::visit_block(
        &mut body,
        &mut |expression| {
            match &expression.kind {
                ExprKind::Closure(function, _) => validate_source_function(function)?,
                ExprKind::Cast(_, ty)
                | ExprKind::Frozen(_, ty)
                | ExprKind::ClosureValue(_, _, ty) => validate_source_type(ty, expression.span)?,
                ExprKind::IntrinsicCall(_, types, _)
                | ExprKind::GenericCall(_, types, _)
                | ExprKind::GenericRecord(_, types, _)
                | ExprKind::TypedCollection(_, types, _) => {
                    for ty in types {
                        validate_source_type(ty, expression.span)?;
                    }
                }
                _ => {}
            }
            Ok(())
        },
        &mut |ty, span| validate_source_type(ty, span),
    )
}

/// Checks the ordinary AST after trusted native declarations have been retained
/// separately in Ast.native, and before generics or services erase metadata.
pub fn validate_source_ast(ast: &Ast) -> Result<(), Diagnostic> {
    for alias in &ast.type_aliases {
        validate_source_type(&alias.ty, alias.span)?;
    }
    for declaration in &ast.enums {
        for variant in &declaration.variants {
            if let Some(payload) = &variant.payload {
                validate_source_type(payload, variant.span)?;
            }
        }
    }
    validate_source_block(&ast.body)?;
    for function in &ast.functions {
        validate_source_function(function)?;
    }
    for record in &ast.records {
        for (_, ty) in &record.fields {
            validate_source_type(ty, record.span)?;
        }
    }
    for declaration in &ast.traits {
        if !declaration.type_params.is_empty() {
            return Err(unsupported_source(
                SourceFeature::GenericTrait,
                declaration.span,
            ));
        }
        for method in &declaration.methods {
            if !method.type_params.is_empty() {
                return Err(unsupported_source(
                    SourceFeature::GenericTraitMethod,
                    method.span,
                ));
            }
            validate_source_function(method)?;
        }
    }
    for implementation in &ast.implementations {
        validate_source_type(&implementation.receiver, implementation.span)?;
        for method in &implementation.methods {
            if !method.type_params.is_empty() {
                return Err(unsupported_source(
                    SourceFeature::GenericTraitMethod,
                    method.span,
                ));
            }
            validate_source_function(method)?;
        }
    }
    for implementation in &ast.inherent_implementations {
        validate_source_type(&implementation.receiver, implementation.span)?;
        for method in &implementation.methods {
            validate_source_function(&method.function)?;
        }
    }
    for property in &ast.properties {
        validate_source_type(&property.receiver, property.span)?;
        validate_source_function(&property.getter)?;
    }
    for module in &ast.modules {
        if let Some(body) = &module.body {
            validate_source_ast(body)?;
        }
    }
    Ok(())
}
#[derive(Debug, Clone, Default)]
pub struct TypeEnvironment {
    types: HashMap<String, Type>,
    pub native: Arc<NativeDeclarations>,
}
impl TypeEnvironment {
    /// The provider's weak-reference type: the declaration whose binding carries the
    /// `weak` trait. The language needs it to type `downgrade`; it asks for the
    /// trait, not for a name.
    pub fn weak_type(&self) -> Option<(Box<str>, cyallo_ast::OpaqueTraits)> {
        self.native.types.iter().find_map(|declaration| {
            match crate::internal::from_key(&declaration.annotation.value)? {
                InternalSemantic::Type(NativeType::Opaque(traits)) if traits.weak() => {
                    Some((declaration.name.clone(), traits))
                }
                _ => None,
            }
        })
    }
    pub fn new(mut types: HashMap<String, Type>, mut native: NativeDeclarations) -> Self {
        normalize_native_signatures(&mut native);
        for declaration in &native.enums {
            let arguments = declaration
                .declaration
                .type_params
                .iter()
                .cloned()
                .map(Type::Named)
                .collect::<Vec<_>>();
            let ty = crate::enums::native_type(declaration.storage, arguments);
            if let Some(contract) = crate::enums::contract(&ty, &native) {
                types.insert(
                    contract.name.to_string(),
                    Type::Enum(std::sync::Arc::new(contract)),
                );
            }
        }
        for declaration in &native.functions {
            types.insert(
                declaration.function.name.to_string(),
                Type::NativeCallable(declaration.id),
            );
        }
        Self {
            types,
            native: Arc::new(native),
        }
    }
    pub fn record_types(&self) -> HashMap<String, Type> {
        self.types.clone()
    }
    pub fn function_reference(
        &self,
        declaration: DeclarationId,
        span: Span,
    ) -> Result<IntrinsicId, Diagnostic> {
        let function = self
            .native
            .functions
            .iter()
            .find(|entry| entry.id == declaration)
            .ok_or_else(|| invalid_declaration(DeclarationError::InvalidSignature, span))?;
        match internal_annotation(&function.function.annotations)
            .and_then(|a| internal::from_key(&a.value))
        {
            Some(InternalSemantic::Function(id)) => Ok(id.id().declared(declaration)),
            _ => Err(invalid_declaration(
                DeclarationError::InvalidSignature,
                span,
            )),
        }
    }
}

impl Deref for TypeEnvironment {
    type Target = HashMap<String, Type>;
    fn deref(&self) -> &Self::Target {
        &self.types
    }
}
impl DerefMut for TypeEnvironment {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.types
    }
}

/// Declared signatures spell the core enums with their markers; resolve them to the
/// declared enums, as source annotations are, so a signature compares with the
/// types it is applied to. Type parameters stay as `Named` arguments of the template.
fn normalize_native_signatures(native: &mut NativeDeclarations) {
    let templates: Vec<_> = native
        .enums
        .iter()
        .map(|e| (e.storage, e.declaration.clone()))
        .collect();
    let normalize = |ty: &mut Type| {
        cyallo_ast::visit::visit_type(ty, &mut |ty| {
            if let Type::Storage(kind, inner) = ty
                && let Some((_, declaration)) = templates.iter().find(|(k, _)| k == kind)
                && let Some(contract) = crate::enums::contract_for(declaration, *kind, inner)
            {
                *ty = Type::Enum(std::sync::Arc::new(contract));
            }
        })
    };
    for declaration in &mut native.functions {
        for parameter in &mut declaration.function.params {
            normalize(&mut parameter.ty);
        }
        normalize(&mut declaration.function.result);
    }
    for declaration in &mut native.methods {
        for parameter in &mut declaration.function.params {
            normalize(&mut parameter.ty);
        }
        normalize(&mut declaration.function.result);
        normalize(&mut declaration.receiver);
    }
}
pub fn equivalent_types(left: &Type, right: &Type) -> bool {
    fn canonical(ty: &Type) -> Type {
        let mut ty = ty.clone();
        cyallo_ast::visit::visit_type(&mut ty, &mut |ty| match ty {
            Type::Tuple(items) if items.is_empty() => *ty = Type::Void,
            Type::Numeric(kind) => *ty = Type::numeric(*kind),
            _ => {}
        });
        ty
    }
    crate::memory::same_regions(&canonical(left), &canonical(right))
}

impl TypeEnvironment {
    pub fn member_visible(&self, declaration: &NativeMethodDeclaration, span: Span) -> bool {
        if declaration.public {
            return true;
        }
        let Some(policy) = &self.native.import_policy else {
            return false;
        };
        let Some(caller) = policy.requesters.get(&span.start) else {
            return false;
        };
        let Some(owner) = declaration.module else {
            return false;
        };
        let path = |id| {
            policy
                .modules
                .iter()
                .find(|module| module.id == id)
                .map(|module| module.path.as_ref())
        };
        match (path(*caller), path(owner)) {
            (Some(caller), Some(owner)) => {
                caller == owner
                    || caller
                        .strip_prefix(owner)
                        .is_some_and(|suffix| suffix.starts_with("::"))
            }
            _ => false,
        }
    }
    pub fn check_native_access(
        &self,
        declaration: DeclarationId,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let Some(policy) = &self.native.import_policy else {
            return Ok(());
        };
        let target = self
            .native
            .functions
            .iter()
            .find(|entry| entry.id == declaration)
            .and_then(|entry| entry.module)
            .or_else(|| {
                self.native
                    .methods
                    .iter()
                    .find(|entry| entry.id == declaration)
                    .and_then(|entry| entry.module)
            });
        let target = target
            .ok_or_else(|| invalid_declaration(DeclarationError::MissingCallerScope, span))?;
        let from = policy
            .requesters
            .get(&span.start)
            .copied()
            .ok_or_else(|| invalid_declaration(DeclarationError::MissingCallerScope, span))?;
        if policy
            .access
            .iter()
            .find(|entry| entry.referrer == from)
            .is_some_and(|entry| entry.allowed.contains(&target))
        {
            return Ok(());
        }
        let identity = |id| {
            policy
                .modules
                .iter()
                .find(|module| module.id == id)
                .map(|module| crate::import_policy::ModuleIdentity {
                    id,
                    path: module.path.clone(),
                })
                .ok_or_else(|| invalid_declaration(DeclarationError::MissingCallerScope, span))
        };
        Err(crate::import_policy::import_policy_diagnostic(
            crate::import_policy::ImportPolicyError::Denied {
                referrer: identity(from)?,
                target: identity(target)?,
            },
            span,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native() -> NativeDeclarations {
        crate::namespace::single(cyallo_parser::parse("").unwrap())
            .unwrap()
            .native
    }
    fn function<'a>(native: &'a mut NativeDeclarations, key: &str) -> &'a mut Function {
        &mut native
            .functions
            .iter_mut()
            .find(|entry| entry.function.annotations[0].value.as_ref() == key)
            .unwrap()
            .function
    }
    fn method<'a>(native: &'a mut NativeDeclarations, key: &str) -> &'a mut Function {
        &mut native
            .methods
            .iter_mut()
            .find(|entry| entry.function.annotations[0].value.as_ref() == key)
            .unwrap()
            .function
    }
    fn allocator() -> Trait {
        native()
            .traits
            .into_iter()
            .find(|(_, declaration)| declaration.annotations[0].value.as_ref() == "allocator_trait")
            .unwrap()
            .1
    }

    #[test]
    fn parsed_standard_headers_satisfy_the_fixed_ownership_and_allocator_abis() {
        validate_native_declarations(&native()).unwrap();
    }

    #[test]
    fn resource_transfers_cannot_be_declared_as_nonconsuming_reads() {
        for key in [
            "cell_new",
            "drop_value",
            "any_erase",
            "any_checked",
            "task_spawn",
        ] {
            let mut declarations = native();
            function(&mut declarations, key).params[0].passing = ParameterPassing::Read;
            let error = validate_native_declarations(&declarations).unwrap_err();
            assert_eq!(error.code, "E173");
            assert!(
                error.message.contains("transferred ownership"),
                "{key}: {error}"
            );
        }
        let mut declarations = native();
        method(&mut declarations, "channel_send").params[1].passing = ParameterPassing::Read;
        assert!(
            validate_native_declarations(&declarations)
                .unwrap_err()
                .message
                .contains("operand 2")
        );
    }

    #[test]
    fn backend_reads_cannot_leak_resource_arguments_moved_by_the_source_header() {
        let mut declarations = native();
        function(&mut declarations, "array_length").params[0].passing = ParameterPassing::Value;
        let error = validate_native_declarations(&declarations).unwrap_err();
        assert!(error.message.contains("read-only ownership"));

        let mut declarations = native();
        let clone = method(&mut declarations, "rc_clone_member");
        let Type::Reference { pointee, .. } = &clone.params[0].ty else {
            panic!()
        };
        clone.params[0].ty = *pointee.clone();
        assert!(
            validate_native_declarations(&declarations)
                .unwrap_err()
                .message
                .contains("read-only ownership")
        );
    }

    #[test]
    fn concrete_copy_values_allow_compatible_modes_but_resources_do_not() {
        let mut declarations = native();
        let mut parameter = function(&mut declarations, "cell_new").params[0].clone();
        parameter.passing = ParameterPassing::Read;
        parameter.ty = Type::Integer;
        validate_native_ownership(
            InternalSemantic::Function(Intrinsic::CellNew),
            &[parameter.clone()],
            parameter.span,
        )
        .unwrap();
        parameter.ty = Type::Managed {
            kind: MemoryKind::Rc,
            locality: Locality::Local,
            lifetime: None,
            pointee: Box::new(Type::Integer),
        };
        assert!(
            validate_native_ownership(
                InternalSemantic::Function(Intrinsic::CellNew),
                &[parameter.clone()],
                parameter.span
            )
            .is_err()
        );
        parameter.passing = ParameterPassing::Value;
        assert!(
            validate_native_ownership(
                InternalSemantic::Function(Intrinsic::ArrayLength),
                &[parameter.clone()],
                parameter.span
            )
            .is_err()
        );
    }

    #[test]
    fn address_and_transferring_receiver_shapes_cannot_be_changed_by_source_modes() {
        let mut declarations = native();
        let push = method(&mut declarations, "collection_push_back");
        let Type::Reference { mutable, .. } = &mut push.params[0].ty else {
            panic!()
        };
        *mutable = false;
        assert!(
            validate_native_declarations(&declarations)
                .unwrap_err()
                .message
                .contains("mutable reference")
        );

        let mut declarations = native();
        let unwrap = method(&mut declarations, "option_unwrap");
        unwrap.params[0].ty = Type::Reference {
            mutable: false,
            lifetime: None,
            pointee: Box::new(unwrap.params[0].ty.clone()),
        };
        assert!(
            validate_native_declarations(&declarations)
                .unwrap_err()
                .message
                .contains("value descriptor")
        );
    }

    #[test]
    fn allocator_slots_validate_semantic_types_and_order_independently_of_names() {
        let mut declaration = allocator();
        declaration.name = "RenamedHostAllocator".into();
        declaration.methods[0].name = "acquire_storage".into();
        declaration.methods[1].name = "return_storage".into();
        allocator_contract(&declaration).unwrap();
        declaration.methods.swap(0, 1);
        assert_eq!(
            allocator_contract(&declaration),
            Err(AllocatorContractError::AllocateShape)
        );
        let mut declaration = allocator();
        declaration.methods[1].params[1].passing = ParameterPassing::Read;
        assert_eq!(
            allocator_contract(&declaration),
            Err(AllocatorContractError::DeallocateShape)
        );
        let mut declaration = allocator();
        declaration.methods[0].result = Type::Integer;
        assert_eq!(
            allocator_contract(&declaration),
            Err(AllocatorContractError::AllocateShape)
        );
        let mut declaration = allocator();
        declaration.type_params.push("T".into());
        assert_eq!(
            allocator_contract(&declaration),
            Err(AllocatorContractError::GenericParameters)
        );
        let mut declaration = allocator();
        declaration.methods.pop();
        assert_eq!(
            allocator_contract(&declaration),
            Err(AllocatorContractError::MethodCount)
        );
        assert!(
            AllocatorContractError::DeallocateShape
                .to_string()
                .contains("ownership transfer")
        );
    }

    #[test]
    fn unimplemented_source_constraints_read_modes_packs_and_generic_traits_are_explicit() {
        for (source, feature) in [
            (
                "fn bounded<T: Numeric>(value: T) -> T { return value }",
                SourceFeature::GenericConstraints,
            ),
            (
                "proc inspect(read value: i64) {}",
                SourceFeature::DescriptorRead,
            ),
            (
                "proc output<...T>(...values: T) {}",
                SourceFeature::ParameterPack,
            ),
            ("trait Generic<T> {}", SourceFeature::GenericTrait),
            (
                "trait Generic { proc visit<T>(self: &Self, value: T); }",
                SourceFeature::GenericTraitMethod,
            ),
        ] {
            let ast = cyallo_parser::parse(source).unwrap();
            let error = validate_source_ast(&ast).unwrap_err();
            assert_eq!(error.code, "E173");
            assert!(
                error.message.contains(&feature.to_string()),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn native_type_families_are_rejected_in_ordinary_source_type_contexts() {
        let mut ast = cyallo_parser::parse("let value: i64 = 1").unwrap();
        let StmtKind::Binding(_, annotation, ..) = &mut ast.body[0].kind else {
            panic!()
        };
        *annotation = Some(Type::Dependent(DeclarationId(7), vec![Type::Integer]));
        assert!(
            validate_source_ast(&ast)
                .unwrap_err()
                .message
                .contains("native type families")
        );
        let mut ast = cyallo_parser::parse("let callback = fn() -> i64 { return 1 }").unwrap();
        let StmtKind::Binding(
            _,
            _,
            _,
            _,
            Expr {
                kind: ExprKind::Closure(function, _),
                ..
            },
        ) = &mut ast.body[0].kind
        else {
            panic!()
        };
        function.result = Type::Dependent(DeclarationId(7), vec![Type::Integer]);
        assert!(
            validate_source_ast(&ast)
                .unwrap_err()
                .message
                .contains("native type families")
        );
    }
}
