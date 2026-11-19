//! Link synchronous scalar SDK bindings through the installed declarations.

use super::{Implementation, Reference, RegistrationError, Registry};
use crate::{
    contract::{Effects, Operand, Safepoint},
    module_graph::{BindingKind, ResolvedProgram},
};
use cyallo_ast::{
    DeclarationId, Expr, ExprKind, Function, FunctionBodyKind, ParameterArity, ParameterPassing,
    RoutineKind, Stmt, StmtKind, Type,
};
use std::fmt;

#[derive(Debug)]
pub enum ReferenceLinkError {
    Registration(Vec<RegistrationError>),
    UnsupportedBinding(DeclarationId),
    UnsupportedContract(DeclarationId),
    MissingBinding(DeclarationId),
    MissingSource {
        declaration: DeclarationId,
        path: Box<str>,
    },
    SignatureMismatch(DeclarationId),
}

impl fmt::Display for ReferenceLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registration(errors) => {
                f.write_str("SDK registration failed")?;
                for error in errors {
                    write!(f, "\n{error}")?;
                }
                Ok(())
            }
            Self::UnsupportedBinding(id) => write!(
                f,
                "SDK declaration {} needs a Cyallo reference or scalar host binding",
                id.0
            ),
            Self::UnsupportedContract(id) => write!(
                f,
                "SDK declaration {} requires effects or operand modes outside scalar execution",
                id.0
            ),
            Self::MissingBinding(id) => {
                write!(f, "SDK declaration {} has no installed binding", id.0)
            }
            Self::MissingSource { declaration, path } => write!(
                f,
                "SDK declaration {} has no source reference at `{path}`",
                declaration.0
            ),
            Self::SignatureMismatch(id) => write!(
                f,
                "SDK declaration {} and its source reference need matching scalar signatures",
                id.0
            ),
        }
    }
}

impl std::error::Error for ReferenceLinkError {}

#[derive(Debug)]
pub struct LinkedScalarReference {
    pub declaration: DeclarationId,
    pub symbol: Box<str>,
    pub may_fail: bool,
}

#[derive(Debug)]
pub struct LinkedScalarHost {
    pub declaration: DeclarationId,
    pub path: Box<str>,
    pub symbol: Box<str>,
    pub parameters: Box<[Type]>,
    pub result: Type,
    pub effects: Effects,
}

#[derive(Debug)]
pub struct ScalarLinks {
    pub references: Vec<LinkedScalarReference>,
    pub hosts: Vec<LinkedScalarHost>,
}

impl Registry {
    /// Link synchronous scalar bindings in an embedder-supplied SDK bundle.
    /// Source references become checked forwarding bodies. Host procedures keep
    /// their declarations and get a separate descriptor for lowering.
    /// Link errors leave the program untouched.
    pub fn link_scalar_sdk(
        self,
        program: &mut ResolvedProgram,
    ) -> Result<ScalarLinks, ReferenceLinkError> {
        let installed = self
            .install_for_program(program)
            .map_err(ReferenceLinkError::Registration)?;
        let mut links = Vec::new();
        let mut hosts = Vec::new();
        for (path, declaration) in &program.interface_declarations {
            let declaration = *declaration;
            let binding = installed
                .get(declaration)
                .ok_or(ReferenceLinkError::MissingBinding(declaration))?;
            let (module, function) = declaration_location(program, declaration)
                .ok_or(ReferenceLinkError::UnsupportedBinding(declaration))?;
            let header = &program.modules[module].source.functions[function];
            if let Implementation::HostCall(contract) = &binding.implementation {
                if header.body_kind != FunctionBodyKind::Declaration
                    || header.kind != RoutineKind::Procedure
                    || !scalar_signature(header)
                {
                    return Err(ReferenceLinkError::SignatureMismatch(declaration));
                }
                let effects = contract.effects;
                if contract.safepoint != Safepoint::Never
                    || effects.allocates
                    || effects.fresh_identity
                    || effects.may_park
                    || effects.calls_guest
                    || !read_operands(header, contract)
                {
                    return Err(ReferenceLinkError::UnsupportedContract(declaration));
                }
                hosts.push(LinkedScalarHost {
                    declaration,
                    path: path.clone(),
                    symbol: header.name.clone(),
                    parameters: header.params.iter().map(|param| param.ty.clone()).collect(),
                    result: header.result.clone(),
                    effects,
                });
                continue;
            }
            let Implementation::Intrinsic {
                contract,
                reference: Some(Reference::Source(path)),
                ..
            } = &binding.implementation
            else {
                return Err(ReferenceLinkError::UnsupportedBinding(declaration));
            };
            let source = source_function(program, path).ok_or_else(|| {
                ReferenceLinkError::MissingSource {
                    declaration,
                    path: path.clone(),
                }
            })?;
            if header.body_kind != FunctionBodyKind::Declaration
                || header.kind != RoutineKind::Function
                || source.kind != RoutineKind::Function
                || !scalar_signature(header)
                || !scalar_signature(source)
                || header.result != source.result
                || !header
                    .params
                    .iter()
                    .map(|p| &p.ty)
                    .eq(source.params.iter().map(|p| &p.ty))
            {
                return Err(ReferenceLinkError::SignatureMismatch(declaration));
            }
            let supported_effects = Effects {
                may_fail: contract.effects.may_fail,
                ..Effects::NONE
            };
            if contract.safepoint != Safepoint::Never
                || contract.effects != supported_effects
                || !read_operands(header, contract)
            {
                return Err(ReferenceLinkError::UnsupportedContract(declaration));
            }
            links.push((
                (module, function),
                LinkedScalarReference {
                    declaration,
                    symbol: source.name.clone(),
                    may_fail: contract.effects.may_fail,
                },
            ));
        }

        let mut references = Vec::with_capacity(links.len());
        for ((module, function), reference) in links {
            let header = &mut program.modules[module].source.functions[function];
            let span = header.span;
            let arguments = header
                .params
                .iter()
                .map(|param| Expr {
                    kind: ExprKind::Name(param.name.clone()),
                    span: param.span,
                })
                .collect();
            header.body_kind = FunctionBodyKind::Source;
            header.body = vec![Stmt {
                kind: StmtKind::Return(Expr {
                    kind: ExprKind::Call(reference.symbol.clone(), arguments),
                    span,
                }),
                span,
            }];
            references.push(reference);
        }
        Ok(ScalarLinks { references, hosts })
    }
}

fn declaration_location(
    program: &ResolvedProgram,
    declaration: DeclarationId,
) -> Option<(usize, usize)> {
    program
        .modules
        .iter()
        .enumerate()
        .find_map(|(index, module)| {
            let binding = module.bindings.iter().find(|binding| {
                binding.declaration == declaration
                    && binding.defining_module == module.id
                    && binding.kind == BindingKind::Value
            })?;
            let function = module
                .source
                .functions
                .iter()
                .position(|function| function.name == binding.target)?;
            Some((index, function))
        })
}

fn source_function<'a>(program: &'a ResolvedProgram, path: &str) -> Option<&'a Function> {
    let (module, name) = path.rsplit_once("::")?;
    let module = program
        .modules
        .iter()
        .find(|candidate| candidate.path.as_ref() == module)?;
    let binding = module.bindings.iter().find(|binding| {
        binding.name.as_ref() == name
            && binding.defining_module == module.id
            && binding.kind == BindingKind::Value
    })?;
    module.source.functions.iter().find(|function| {
        function.name == binding.target && function.body_kind == FunctionBodyKind::Source
    })
}

fn scalar_signature(function: &Function) -> bool {
    !function.generator
        && function.type_params.is_empty()
        && function.type_packs.is_empty()
        && function.constraints.is_empty()
        && matches!(function.result, Type::Integer | Type::Boolean)
        && function.params.iter().all(|param| {
            param.passing == ParameterPassing::Value
                && param.arity == ParameterArity::One
                && !param.receiver
                && !param.by_ref
                && matches!(param.ty, Type::Integer | Type::Boolean)
        })
}

fn read_operands(function: &Function, contract: &super::BindingContract) -> bool {
    (0..function.params.len()).all(|index| contract.operands.mode(index) == Operand::Read)
}
