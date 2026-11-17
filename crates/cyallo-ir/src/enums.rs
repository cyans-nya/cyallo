//! Data enum contracts and ordinary constructor declarations.
use cyallo_ast::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    Value,
    Address,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnumError {
    TypeArguments {
        name: Box<str>,
        expected: usize,
        actual: usize,
    },
    NativeContract {
        name: Box<str>,
    },
    DuplicateType {
        name: Box<str>,
    },
    UnresolvedDeclaration {
        name: Box<str>,
    },
    DuplicateVariant {
        name: Box<str>,
    },
    UnitVariantCalled {
        name: Box<str>,
    },
    WrongVariant {
        name: Box<str>,
        enumeration: Box<str>,
    },
    MissingPayload {
        name: Box<str>,
    },
    UnexpectedPayload {
        name: Box<str>,
    },
    PayloadType {
        expected: Type,
        actual: Type,
    },
    RecursiveValue {
        name: Box<str>,
    },
}
impl std::fmt::Display for EnumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeArguments {
                name,
                expected,
                actual,
            } => write!(
                f,
                "type `{}` requires {expected} type arguments, found {actual}",
                source_name(name)
            ),
            Self::NativeContract { name } => write!(
                f,
                "native enum `{}` has an invalid variant or payload contract",
                source_name(name)
            ),
            Self::DuplicateType { name } => write!(f, "duplicate type `{}`", source_name(name)),
            Self::UnresolvedDeclaration { name } => write!(
                f,
                "enum declaration `{}` has not been resolved",
                source_name(name)
            ),
            Self::DuplicateVariant { name } => {
                write!(f, "duplicate enum variant `{}`", source_name(name))
            }
            Self::UnitVariantCalled { name } => write!(
                f,
                "unit variant `{}` is a value and does not take parentheses",
                source_name(name)
            ),
            Self::WrongVariant { name, enumeration } => write!(
                f,
                "variant `{}` does not belong to `{}`",
                source_name(name),
                source_name(enumeration)
            ),
            Self::MissingPayload { name } => {
                write!(f, "variant `{}` requires a payload", source_name(name))
            }
            Self::UnexpectedPayload { name } => {
                write!(f, "variant `{}` has no payload", source_name(name))
            }
            Self::PayloadType { expected, actual } => {
                write!(f, "enum payload requires `{expected}`, found `{actual}`")
            }
            Self::RecursiveValue { name } => write!(
                f,
                "recursive enum `{}` requires indirection",
                source_name(name)
            ),
        }
    }
}
impl std::error::Error for EnumError {}
pub fn invalid_enum(error: EnumError, span: Span) -> Diagnostic {
    Diagnostic::new("E184", error.to_string(), span)
}
pub fn constructors(declaration: &EnumDeclaration) -> Result<Vec<Function>, Diagnostic> {
    if let Some(storage) = native_storage(declaration)
        && declaration.type_params.len()
            != crate::declarations::type_arity(crate::internal::NativeType::Storage(storage))
    {
        return Err(invalid_enum(
            EnumError::NativeContract {
                name: declaration.name.clone(),
            },
            declaration.span,
        ));
    }
    declaration.resolved.as_ref().ok_or_else(|| {
        invalid_enum(
            EnumError::UnresolvedDeclaration {
                name: declaration.name.clone(),
            },
            declaration.span,
        )
    })?;
    declaration
        .variants
        .iter()
        .map(|variant| {
            let resolved = variant.resolved.as_ref().ok_or_else(|| {
                invalid_enum(
                    EnumError::UnresolvedDeclaration {
                        name: variant.name.clone(),
                    },
                    variant.span,
                )
            })?;
            let result = if let Some(storage) = native_storage(declaration) {
                let arguments = declaration
                    .type_params
                    .iter()
                    .cloned()
                    .map(Type::Named)
                    .collect::<Vec<_>>();
                native_type(storage, arguments)
            } else if declaration.type_params.is_empty() {
                Type::Named(declaration.name.clone())
            } else {
                Type::Applied(
                    declaration.name.clone(),
                    declaration
                        .type_params
                        .iter()
                        .cloned()
                        .map(Type::Named)
                        .collect(),
                )
            };
            let parameters = variant
                .payload
                .iter()
                .map(|ty| Param {
                    arity: ParameterArity::One,
                    passing: ParameterPassing::Value,
                    receiver: false,
                    name: "$enum_payload".into(),
                    ty: ty.clone(),
                    by_ref: false,
                    span: variant.span,
                })
                .collect();
            let payload = variant.payload.as_ref().map(|_| {
                Box::new(Expr {
                    kind: ExprKind::Name("$enum_payload".into()),
                    span: variant.span,
                })
            });
            Ok(Function {
                annotations: vec![],
                body_kind: FunctionBodyKind::Source,
                constraints: vec![],
                type_packs: vec![],
                generator: false,
                source_name: variant.name.clone(),
                origin: RoutineOrigin::EnumConstructor,
                attributes: Default::default(),
                type_params: declaration.type_params.clone(),
                kind: RoutineKind::Function,
                name: resolved.symbol.clone(),
                params: parameters,
                result: result.clone(),
                body: vec![Stmt {
                    kind: StmtKind::Return(Expr {
                        kind: ExprKind::EnumConstruct(resolved.declaration, result, payload),
                        span: variant.span,
                    }),
                    span: variant.span,
                }],
                span: variant.span,
            })
        })
        .collect()
}

/// Private representation selection is permitted only on validated core declarations.
pub fn native_storage(declaration: &EnumDeclaration) -> Option<StorageKind> {
    declaration
        .annotations
        .iter()
        .filter(|a| a.name.as_ref() == "cyallo_internal")
        .find_map(|a| match crate::internal::from_key(&a.value) {
            Some(crate::internal::InternalSemantic::Type(
                crate::internal::NativeType::Storage(storage),
            )) => Some(storage),
            _ => None,
        })
}
pub fn contract(ty: &Type, native: &NativeDeclarations) -> Option<EnumType> {
    if let Type::Enum(contract) = ty {
        return Some(EnumType::clone(contract));
    }
    let Type::Storage(storage, inner) = ty else {
        return None;
    };
    let source = &native
        .enums
        .iter()
        .find(|e| e.storage == *storage)?
        .declaration;
    contract_for(source, *storage, inner)
}
/// The enum a native declaration denotes for the given argument payload.
pub fn contract_for(source: &EnumDeclaration, storage: StorageKind, inner: &Type) -> Option<EnumType> {
    let storage = &storage;
    let arguments = native_arguments(*storage, inner)?;
    let bindings = source
        .type_params
        .iter()
        .zip(&arguments)
        .collect::<std::collections::HashMap<_, _>>();
    let variants = source
        .variants
        .iter()
        .map(|variant| {
            let resolved = variant.resolved.as_ref()?;
            let mut payload = variant.payload.clone();
            if let Some(payload) = &mut payload {
                cyallo_ast::visit::visit_type(payload, &mut |ty| {
                    if let Type::Named(name) = ty
                        && let Some(actual) = bindings.get(name)
                    {
                        *ty = (**actual).clone();
                    }
                });
            }
            Some(VariantType {
                declaration: resolved.declaration,
                encoding: resolved.encoding,
                name: variant.name.clone(),
                symbol: resolved.symbol.clone(),
                payload,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let resolved = source.resolved.as_ref()?;
    Some(EnumType {
        identity: EnumIdentity {
            declaration: resolved.declaration,
            arguments,
        },
        name: source.name.clone(),
        package: resolved.package,
        open: source.open,
        variants,
    })
}

pub fn validate_native_enums(native: &NativeDeclarations) -> Result<(), Diagnostic> {
    #[derive(Clone, Copy)]
    enum Payload {
        Unit,
        Parameter(usize),
    }
    use Payload::*;
    let mut kinds = std::collections::HashSet::new();
    for native in &native.enums {
        let declaration = &native.declaration;
        let invalid = || {
            invalid_enum(
                EnumError::NativeContract {
                    name: declaration.name.clone(),
                },
                declaration.span,
            )
        };
        use EnumEncoding::*;
        let roles: &[(EnumEncoding, Payload)] = match native.storage {
            StorageKind::Option => &[(OptionSome, Parameter(0)), (OptionNone, Unit)],
            StorageKind::Result => &[(ResultOk, Parameter(0)), (ResultErr, Parameter(1))],
            StorageKind::GeneratorStep => &[(StepYielded, Parameter(0)), (StepComplete, Parameter(1))],
            StorageKind::SendError => &[
                (SendClosed, Parameter(0)),
                (SendFull, Parameter(0)),
                (SendLimit, Parameter(0)),
            ],
            _ => return Err(invalid()),
        };
        let arity =
            crate::declarations::type_arity(crate::internal::NativeType::Storage(native.storage));
        if !kinds.insert(native.storage)
            || declaration.type_params.len() != arity
            || declaration.variants.len() != roles.len()
            || declaration.open
            || declaration
                .type_params
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != arity
        {
            return Err(invalid());
        }
        let mut encodings = std::collections::HashSet::new();
        for variant in &declaration.variants {
            let encoding = variant.resolved.as_ref().ok_or_else(invalid)?.encoding;
            if !encodings.insert(encoding) {
                return Err(invalid());
            }
            let (_, parameter) = roles
                .iter()
                .find(|(role, _)| *role == encoding)
                .ok_or_else(invalid)?;
            let expected = match parameter {
                Unit => None,
                Parameter(index) => Some(Type::Named(declaration.type_params[*index].clone())),
            };
            if variant.payload != expected {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

/// Validate unused enum templates too. Instantiation must not hide an invalid
/// value-recursive declaration or a malformed use of another generic type.
pub(crate) fn validate_templates(asts: &[Ast]) -> Result<(), Diagnostic> {
    if !asts.iter().any(|ast| !ast.enums.is_empty()) {
        return Ok(());
    }
    use std::collections::HashMap;
    struct Definition {
        params: Vec<Box<str>>,
        fields: Vec<Type>,
    }
    let mut definitions = HashMap::new();
    for ast in asts {
        for record in &ast.records {
            definitions.insert(
                record.name.clone(),
                Definition {
                    params: record.type_params.clone(),
                    fields: record.fields.iter().map(|(_, t)| t.clone()).collect(),
                },
            );
        }
        for enumeration in &ast.enums {
            definitions.insert(
                enumeration.name.clone(),
                Definition {
                    params: enumeration.type_params.clone(),
                    fields: enumeration
                        .variants
                        .iter()
                        .filter_map(|v| v.payload.clone())
                        .collect(),
                },
            );
        }
    }
    fn walk(
        ty: &Type,
        definitions: &HashMap<Box<str>, Definition>,
        active: &mut Vec<(Box<str>, Vec<Type>)>,
        origin: &EnumDeclaration,
        depth: usize,
    ) -> Result<(), Diagnostic> {
        if depth > 256 {
            return Err(invalid_enum(
                EnumError::RecursiveValue {
                    name: origin.name.clone(),
                },
                origin.span,
            ));
        }
        match ty {
            Type::Named(name) | Type::Applied(name, _) if definitions.contains_key(name) => {
                let args = match ty {
                    Type::Applied(_, args) => args.clone(),
                    _ => vec![],
                };
                let definition = &definitions[name];
                if args.len() != definition.params.len() {
                    return Err(invalid_enum(
                        EnumError::TypeArguments {
                            name: name.clone(),
                            expected: definition.params.len(),
                            actual: args.len(),
                        },
                        origin.span,
                    ));
                }
                let instance = (name.clone(), args.clone());
                if active.contains(&instance) {
                    return Err(invalid_enum(
                        EnumError::RecursiveValue {
                            name: origin.name.clone(),
                        },
                        origin.span,
                    ));
                }
                active.push(instance);
                let bindings = definition
                    .params
                    .iter()
                    .zip(&args)
                    .collect::<HashMap<_, _>>();
                for field in &definition.fields {
                    let mut field = field.clone();
                    cyallo_ast::visit::visit_type(&mut field, &mut |ty| {
                        if let Type::Named(name) = ty
                            && let Some(actual) = bindings.get(name)
                        {
                            *ty = (**actual).clone();
                        }
                    });
                    walk(&field, definitions, active, origin, depth + 1)?;
                }
                active.pop();
            }
            Type::Tuple(fields) => {
                for field in fields {
                    walk(field, definitions, active, origin, depth + 1)?;
                }
            }
            Type::AnonymousRecord(fields) | Type::Record(_, fields) => {
                for (_, field) in fields {
                    walk(field, definitions, active, origin, depth + 1)?;
                }
            }
            Type::Array(_, field) | Type::SparseArray(field) | Type::Slice(field) => {
                walk(field, definitions, active, origin, depth + 1)?
            }
            Type::Storage(_, field) => {
                walk(field, definitions, active, origin, depth + 1)?
            }
            Type::Opaque(_, arguments, traits) if !traits.indirect() => {
                for field in arguments {
                    walk(field, definitions, active, origin, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    for enumeration in asts.iter().flat_map(|ast| &ast.enums) {
        let args = enumeration
            .type_params
            .iter()
            .cloned()
            .map(Type::Named)
            .collect::<Vec<_>>();
        walk(
            &Type::Applied(enumeration.name.clone(), args),
            &definitions,
            &mut vec![],
            enumeration,
            0,
        )?;
        // Check arity beyond indirections even though they terminate value recursion.
        for variant in &enumeration.variants {
            if let Some(payload) = &variant.payload {
                let mut error = None;
                cyallo_ast::visit::visit_type(&mut payload.clone(), &mut |ty| {
                    let (name, actual) = match ty {
                        Type::Applied(name, args) => (name, args.len()),
                        Type::Named(name) => (name, 0),
                        _ => return,
                    };
                    if let Some(definition) = definitions.get(name)
                        && actual != definition.params.len()
                    {
                        error = Some(invalid_enum(
                            EnumError::TypeArguments {
                                name: name.clone(),
                                expected: definition.params.len(),
                                actual,
                            },
                            variant.span,
                        ));
                    }
                });
                if let Some(error) = error {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

pub fn native_type(storage: StorageKind, arguments: Vec<Type>) -> Type {
    let inner = match arguments.as_slice() {
        [] => Type::Void,
        [one] => one.clone(),
        _ => Type::Tuple(arguments),
    };
    Type::Storage(storage, Box::new(inner))
}
pub fn native_arguments(storage: StorageKind, inner: &Type) -> Option<Vec<Type>> {
    let arity = crate::declarations::type_arity(crate::internal::NativeType::Storage(storage));
    match (arity, inner) {
        (0, Type::Void) => Some(vec![]),
        (1, inner) => Some(vec![inner.clone()]),
        (arity, Type::Tuple(types)) if types.len() == arity => Some(types.clone()),
        _ => None,
    }
}
pub fn encoding_storage(encoding: EnumEncoding) -> Option<StorageKind> {
    use EnumEncoding::*;
    Some(match encoding {
        General | ReceiveClosed | ReceiveEmpty => return None,
        OptionSome | OptionNone => StorageKind::Option,
        ResultOk | ResultErr => StorageKind::Result,
        StepYielded | StepComplete => StorageKind::GeneratorStep,
        SendClosed | SendFull | SendLimit => StorageKind::SendError,
    })
}
/// Whether a declared enum is one of the core enums (`Option`, `Result`, ...).
pub fn is_core(contract: &EnumType) -> bool {
    contract
        .variants
        .first()
        .and_then(|v| encoding_storage(v.encoding))
        .is_some()
}
/// The core enum a type denotes and its type arguments, whether it is spelled as the
/// `Storage` marker or as the declared enum.
pub fn core_shape(ty: &Type) -> Option<(StorageKind, Vec<Type>)> {
    match ty {
        Type::Storage(
            kind @ (StorageKind::Option
            | StorageKind::Result
            | StorageKind::GeneratorStep
            | StorageKind::SendError),
            inner,
        ) => Some((*kind, native_arguments(*kind, inner)?)),
        Type::Enum(contract) => {
            let kind = encoding_storage(contract.variants.first()?.encoding)?;
            Some((kind, contract.identity.arguments.clone()))
        }
        _ => None,
    }
}
pub fn instantiate_native_template(template: &EnumType, arguments: Vec<Type>) -> Option<EnumType> {
    if template.identity.arguments.len() != arguments.len() {
        return None;
    }
    let bindings = template
        .identity
        .arguments
        .iter()
        .zip(&arguments)
        .map(|(parameter, actual)| {
            if let Type::Named(name) = parameter {
                Some((name, actual))
            } else {
                None
            }
        })
        .collect::<Option<std::collections::HashMap<_, _>>>()?;
    let mut contract = template.clone();
    for variant in &mut contract.variants {
        if let Some(payload) = &mut variant.payload {
            cyallo_ast::visit::visit_type(payload, &mut |ty| {
                if let Type::Named(name) = ty
                    && let Some(actual) = bindings.get(name)
                {
                    *ty = (**actual).clone();
                }
            });
        }
    }
    contract.identity.arguments = arguments;
    Some(contract)
}
