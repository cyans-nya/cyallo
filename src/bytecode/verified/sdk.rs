//! Compile a closed SDK fixture directly from checked IR to verified execution.

use super::{
    HostBindingId, LowerError, ScalarHostBinding, ScalarSdk, VerifiedModule,
    lower::{lower_index, lower_type},
};
use crate::{CompileOptions, Diagnostic, Jobs};
use cyallo_ast::DeclarationId;
use cyallo_ir::{
    core_package::{CorePackage, invalid_core_source},
    import_policy::ImportPolicy,
    module_graph::BindingKind,
    registry::{ReferenceLinkError, Registry},
};
use std::{collections::HashMap, fmt, sync::Arc};

#[derive(Debug)]
pub enum SdkCompileError {
    Check(Diagnostic),
    Link(ReferenceLinkError),
    UnknownEntry(Box<str>),
    MissingReference(DeclarationId),
    ReferenceMayFail(DeclarationId),
    MissingHostCallback(DeclarationId),
    MissingHostDeclaration(DeclarationId),
    InvalidHostSignature(DeclarationId),
    HostEntry(DeclarationId),
    Lower(LowerError),
}

impl fmt::Display for SdkCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(error) => error.fmt(f),
            Self::Link(error) => error.fmt(f),
            Self::UnknownEntry(name) => write!(f, "SDK fixture has no entry routine `{name}`"),
            Self::MissingReference(id) => write!(
                f,
                "SDK declaration {} has no checked source reference",
                id.0
            ),
            Self::ReferenceMayFail(id) => write!(
                f,
                "reference for SDK declaration {} can fail under an infallible binding",
                id.0
            ),
            Self::MissingHostCallback(id) => {
                write!(f, "SDK declaration {} has no host callback", id.0)
            }
            Self::MissingHostDeclaration(id) => {
                write!(f, "SDK declaration {} has no checked routine header", id.0)
            }
            Self::InvalidHostSignature(id) => write!(
                f,
                "SDK declaration {} has an unsupported host signature",
                id.0
            ),
            Self::HostEntry(id) => write!(f, "SDK declaration {} requires a guest caller", id.0),
            Self::Lower(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SdkCompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Check(error) => Some(error),
            Self::Link(error) => Some(error),
            Self::Lower(error) => Some(error),
            _ => None,
        }
    }
}

impl VerifiedModule {
    /// Compile a Modern SDK reference fixture supplied as one bundle by the
    /// embedder. A registry is installed for each resolved bundle. A bound
    /// free function selects an ordinary checked scalar reference routine.
    /// All selected guest code executes in the verified interpreter.
    ///
    /// This entry is for closed source fixtures. World loading, provider
    /// callbacks and constant evaluation are separate execution paths.
    pub fn lower_sdk_reference(
        source: &str,
        entry: &str,
        registry: Registry,
    ) -> Result<Self, SdkCompileError> {
        Self::lower_sdk(source, entry, ScalarSdk::new(registry))
    }

    /// Lower a closed Modern SDK bundle with checked source references and
    /// synchronous scalar host procedures. The verified module retains the
    /// callbacks that were installed for this bundle.
    pub fn lower_sdk(source: &str, entry: &str, sdk: ScalarSdk) -> Result<Self, SdkCompileError> {
        let ScalarSdk {
            registry,
            mut callbacks,
        } = sdk;
        let options = CompileOptions::default();
        let parsed =
            cyallo_parser::parse_modern_snapshot(Arc::from(source), false, Jobs(options.jobs))
                .map_err(SdkCompileError::Check)?;
        let mut archive = source.to_owned();
        let core = CorePackage::embedded();
        let core_base = core
            .append_to(&mut archive)
            .map_err(invalid_core_source)
            .map_err(SdkCompileError::Check)?
            .base;
        let mut graph = cyallo_ir::namespace::link_with_core(
            vec![parsed.to_source_tree()],
            &["main.ya".into()],
            0,
            &ImportPolicy::default(),
            &core,
            core_base,
        )
        .map_err(SdkCompileError::Check)?;
        let entry_symbol = graph.modules[graph.entry_index()]
            .bindings
            .iter()
            .find(|binding| binding.name.as_ref() == entry && binding.kind == BindingKind::Value)
            .ok_or_else(|| SdkCompileError::UnknownEntry(entry.into()))?
            .target
            .clone();
        let links = registry
            .link_scalar_sdk(&mut graph)
            .map_err(SdkCompileError::Link)?;
        let native = graph.native.clone();
        graph
            .with_sources(|sources| {
                cyallo_ir::generics::specialize_modules(sources, &native, options)
            })
            .map_err(SdkCompileError::Check)?;
        // The analyzer publishes declaration headers in deferred mode. Host
        // headers have no guest bodies. Reachable guest bodies are checked
        // before publication by the reference and verified lowering passes.
        let checked = cyallo_ir::analysis::analyze_modules(
            archive.len(),
            &mut graph,
            options,
            !links.hosts.is_empty(),
        )
        .map_err(SdkCompileError::Check)?;
        let routines = &checked.executable.functions;
        for reference in links.references {
            let routine = routines
                .iter()
                .find(|routine| routine.symbol == reference.symbol)
                .ok_or(SdkCompileError::MissingReference(reference.declaration))?
                .checked()
                .map_err(|diagnostic| SdkCompileError::Check(diagnostic.clone()))?;
            if routine.effects.may_fail && !reference.may_fail {
                return Err(SdkCompileError::ReferenceMayFail(reference.declaration));
            }
        }
        let id = routines
            .iter()
            .position(|routine| routine.symbol == entry_symbol)
            .ok_or_else(|| SdkCompileError::UnknownEntry(entry.into()))?;
        let mut hosts = Vec::with_capacity(links.hosts.len());
        let mut host_ids = HashMap::with_capacity(links.hosts.len());
        for host in links.hosts {
            let declaration = host.declaration;
            let routine = routines
                .iter()
                .position(|routine| routine.symbol == host.symbol)
                .ok_or(SdkCompileError::MissingHostDeclaration(declaration))?;
            if routine == id {
                return Err(SdkCompileError::HostEntry(declaration));
            }
            let callback = callbacks
                .remove(&host.path)
                .ok_or(SdkCompileError::MissingHostCallback(declaration))?;
            let signature_error = || SdkCompileError::InvalidHostSignature(declaration);
            let parameters = host
                .parameters
                .iter()
                .map(lower_type)
                .collect::<Option<Box<[_]>>>()
                .ok_or_else(signature_error)?;
            let result = lower_type(&host.result).ok_or_else(signature_error)?;
            let binding =
                HostBindingId(lower_index(routine, hosts.len()).map_err(SdkCompileError::Lower)?);
            hosts.push(ScalarHostBinding {
                declaration,
                parameters,
                result,
                effects: host.effects,
                callback,
            });
            host_ids.insert(routine, binding);
        }
        Self::lower_with_hosts(routines, id, &host_ids, hosts, None, &[])
            .map_err(SdkCompileError::Lower)
    }
}
