//! Runtime source compilation and append-only publication. No VM heap crosses
//! the host job: the completed snapshot is installed only after the C resumes.
mod callable;
mod publication;
use crate::{CompileOptions, Span, compiler, ir, modules::SourceRange};
pub(crate) use callable::{Adapter, CallRequest, CompiledCalls, specialize_calls};
use cyallo_ast::{
    self as ast, Ast, Expr, ExprKind, Function, FunctionBodyKind, ModuleDeclaration,
    NativeImportPolicy, Param, RoutineAttributes, RoutineKind, RoutineOrigin, Stmt, StmtKind,
    StorageKind, Type,
};
use cyallo_ir::{
    core_package::{CorePackage, SourceText},
    import_policy::{ImportPolicy, ImportScope, ModuleAccess, ScopeMatch},
    intrinsic::Intrinsic,
};
use cyallo_modules::{LoadError, ModuleLoader, PublicationError as P, SourceModule};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone)]
struct CoreSources {
    root: String,
    sources: Vec<SourceModule>,
    /// The host package's own implementation bindings: runtime-loaded modules link
    /// against exactly the core the host compiled with.
    bindings: cyallo_ir::bindings::NativeBindings,
}
impl CoreSources {
    fn view(&self) -> CorePackage<'_> {
        CorePackage::from_trusted_sources(
            &self.root,
            self.sources.iter().map(|source| SourceText {
                path: &source.path,
                source: &source.source,
            }),
        )
        .expect("validated owned core source")
        .with_bindings(self.bindings.clone())
    }
}
#[derive(Debug, Clone)]
struct Loaded {
    path: PathBuf,
    specifier: PathBuf,
    authority: Vec<Box<str>>,
    module: Box<str>,
    factory: Option<usize>,
    initializer: Option<usize>,
}
#[derive(Debug, Clone)]
pub(crate) struct CompilePlan {
    source_files: Vec<cyallo_ir::module_graph::SourceFile>,
    trees: SourceTrees,
    paths: Vec<String>,
    entry: usize,
    archive: String,
    ranges: Vec<SourceRange>,
    core: CoreSources,
    core_base: u32,
    options: CompileOptions,
    policy: ImportPolicy,
    pub authorization: Option<NativeImportPolicy>,
    loaded: Vec<Loaded>,
    next_factory: usize,
    calls: Vec<(CallRequest, Adapter)>,
}
pub(crate) struct CompileSources {
    pub source_files: Vec<cyallo_ir::module_graph::SourceFile>,
    pub trees: SourceTrees,
    pub paths: Vec<String>,
    pub entry: usize,
    pub archive: String,
    pub ranges: Vec<SourceRange>,
    pub core_base: u32,
}

/// A file's immutable, file-relative syntax and its placement in the archive.
#[derive(Debug, Clone)]
pub(crate) struct ArchivedSyntax {
    pub file: Arc<cyallo_parser::ParsedFile>,
    pub offset: u32,
}

impl ArchivedSyntax {
    pub(crate) fn materialize(&self) -> Result<Ast, ast::Diagnostic> {
        let mut tree = self.file.to_source_tree();
        if self.offset != 0 {
            ast::spans::shift_ast(&mut tree, self.offset)?;
        }
        Ok(tree)
    }
}

/// Source snapshots for the static graph, with synthetic runtime packages in a
/// separate overlay. Materialization is the boundary to the mutable linker.
#[derive(Debug, Clone)]
pub(crate) struct SourceTrees {
    files: Vec<ArchivedSyntax>,
    packages: Vec<Arc<ModuleDeclaration>>,
}

impl SourceTrees {
    pub(crate) fn new(files: Vec<ArchivedSyntax>) -> Self {
        Self {
            files,
            packages: vec![],
        }
    }

    pub(crate) fn materialize(&self, entry: usize) -> Result<Vec<Ast>, ast::Diagnostic> {
        let mut files = self
            .files
            .iter()
            .map(ArchivedSyntax::materialize)
            .collect::<Result<Vec<_>, _>>()?;
        files[entry]
            .modules
            .extend(self.packages.iter().map(|package| (**package).clone()));
        Ok(files)
    }
}

impl CompilePlan {
    pub(crate) fn call_requests(&self) -> Vec<CallRequest> {
        self.calls
            .iter()
            .map(|(request, _)| request.clone())
            .collect()
    }
    pub(crate) fn new(
        sources: CompileSources,
        options: CompileOptions,
        policy: &ImportPolicy,
        core: &CorePackage<'_>,
    ) -> Self {
        let CompileSources {
            source_files,
            trees,
            paths,
            entry,
            archive,
            ranges,
            core_base,
        } = sources;
        Self {
            source_files,
            trees,
            paths,
            entry,
            archive,
            ranges,
            core: CoreSources {
                root: core.root().into(),
                sources: core
                    .sources()
                    .iter()
                    .map(|source| SourceModule::new(source.path, source.source))
                    .collect(),
                bindings: core.bindings().clone(),
            },
            core_base,
            options,
            policy: policy.clone(),
            authorization: None,
            loaded: vec![],
            next_factory: 0,
            calls: vec![],
        }
    }
    pub(crate) fn request(&self, span: Span, specifier: &str) -> Result<Request, LoadError> {
        let path = self
            .ranges
            .iter()
            .find(|range| range.span.start <= span.start && span.end <= range.span.end)
            .and_then(|range| range.path.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&self.paths[self.entry]));
        let path = path.parent().unwrap_or(Path::new("")).join(specifier);
        let policy = self
            .authorization
            .as_ref()
            .ok_or(LoadError::Publication(P::MissingCompilePlan))?;
        let caller = policy
            .requesters
            .get(&span.start)
            .ok_or(LoadError::Publication(P::MissingCompilePlan))?;
        let access = policy
            .access
            .iter()
            .find(|access| access.referrer == *caller)
            .ok_or(LoadError::Publication(P::MissingCompilePlan))?;
        let mut authority: Vec<Box<str>> = policy
            .modules
            .iter()
            .filter(|module| {
                module.trust == ast::SourceTrust::Core && access.allowed.contains(&module.id)
            })
            .map(|module| module.path.clone())
            .collect();
        authority.sort();
        Ok(Request {
            path,
            span,
            authority,
        })
    }
}
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub path: PathBuf,
    pub span: Span,
    authority: Vec<Box<str>>,
}
pub(crate) struct CompiledLoad {
    pub program: Option<Arc<ir::Program>>,
    pub key: usize,
    pub factory: usize,
    pub initializers: Vec<usize>,
}

fn expression(kind: ExprKind, span: Span) -> Expr {
    Expr { kind, span }
}
fn function(
    name: Box<str>,
    params: Vec<Param>,
    result: Type,
    body: Vec<Stmt>,
    span: Span,
) -> Function {
    Function {
        annotations: vec![],
        body_kind: FunctionBodyKind::Source,
        constraints: vec![],
        type_packs: vec![],
        generator: false,
        source_name: "runtime load".into(),
        origin: RoutineOrigin::Source,
        attributes: RoutineAttributes::default(),
        type_params: vec![],
        kind: RoutineKind::Procedure,
        name,
        params,
        result,
        body,
        span,
    }
}

/// Owns loader and source snapshots for one admitted host operation.
pub(crate) fn load(
    program: Arc<ir::Program>,
    loader: ModuleLoader,
    request: Request,
) -> Result<CompiledLoad, LoadError> {
    let mut plan = program
        .dynamic_plan
        .as_ref()
        .ok_or(LoadError::Publication(P::MissingCompilePlan))?
        .as_ref()
        .clone();
    let known = plan.loaded.iter().position(|module| {
        module.specifier == request.path && module.authority == request.authority
    });
    let canonical = match known {
        Some(index) => plan.loaded[index].path.clone(),
        None => loader
            .resolve_file(&request.path)
            .map_err(|error| LoadError::Load(Arc::new(error)))?,
    };
    let loaded = known.or_else(|| {
        plan.loaded
            .iter()
            .position(|module| module.path == canonical && module.authority == request.authority)
    });
    if let Some(index) = loaded
        && let Some(factory) = plan.loaded[index].factory
    {
        return Ok(CompiledLoad {
            program: None,
            key: index,
            factory,
            initializers: plan
                .loaded
                .iter()
                .filter_map(|module| module.initializer)
                .collect(),
        });
    }
    let loaded_index = if let Some(index) = loaded {
        index
    } else {
        let sources = loader
            .load_parsed(&canonical, crate::Jobs(plan.options.jobs))
            .map_err(|error| LoadError::Load(Arc::new(error)))?;
        let mut files = HashMap::new();
        for source in sources {
            plan.archive
                .push_str(&format!("\n// module: {}\n", source.path));
            let offset = u32::try_from(plan.archive.len())
                .map_err(|_| LoadError::Publication(P::SourceArchiveOverflow))?;
            plan.archive.push_str(source.file.source().text());
            let end = u32::try_from(plan.archive.len())
                .map_err(|_| LoadError::Publication(P::SourceArchiveOverflow))?;
            let parsed = ArchivedSyntax {
                file: source.file,
                offset,
            }
            .materialize()
            .map_err(|diagnostic| LoadError::Check {
                path: canonical.clone(),
                diagnostic,
            })?;
            plan.ranges.push(SourceRange {
                path: Some(source.path.clone().into()),
                span: Span::new(offset, end),
            });
            plan.source_files.push(cyallo_ir::module_graph::SourceFile {
                id: cyallo_ir::module_graph::SourceFileId {
                    path: source.path.clone().into(),
                    trust: ast::SourceTrust::User,
                },
                span: Span::new(offset, end),
            });
            files.insert(PathBuf::from(&source.path), parsed);
        }
        let mut root = files
            .remove(&canonical)
            .ok_or(LoadError::Publication(P::MissingModuleSource))?;
        inline_modules(
            &mut root,
            canonical.parent().unwrap_or(loader.grant().root()),
            &mut files,
        )?;
        let name: Box<str> = format!("cyalloDynamicModule{}", plan.loaded.len()).into();
        let module: Box<str> = format!("crate::{name}").into();
        plan.trees.packages.push(Arc::new(ModuleDeclaration {
            interface: false,
            boundary: ast::ModuleBoundary::Package,
            annotations: vec![],
            name,
            public: false,
            body: Some(Box::new(root)),
            span: request.span,
        }));
        plan.policy.scopes.push(ImportScope {
            referrer: module.clone(),
            matching: ScopeMatch::Subtree,
            access: ModuleAccess::Only(request.authority.clone()),
        });
        plan.loaded.push(Loaded {
            path: canonical.clone(),
            specifier: request.path.clone(),
            authority: request.authority.clone(),
            module,
            factory: None,
            initializer: None,
        });
        plan.loaded.len() - 1
    };
    let core = plan.core.view();
    let trees = plan.trees.materialize(plan.entry).map_err(|diagnostic| {
        localize(
            &plan,
            LoadError::Check {
                path: canonical.clone(),
                diagnostic,
            },
        )
    })?;
    let mut linked = crate::namespace::link_with_core(
        trees,
        &plan.paths,
        plan.entry,
        &plan.policy,
        &core,
        plan.core_base,
    )
    .map_err(|diagnostic| {
        localize(
            &plan,
            LoadError::Check {
                path: canonical.clone(),
                diagnostic,
            },
        )
    })?;
    linked.source_files = plan.source_files.clone();
    plan.authorization = linked.native.import_policy.clone();
    let factory_name: Box<str> = format!("$dynamicLoadFactory{}", plan.next_factory).into();
    plan.next_factory += 1;
    // Install every reachable module object first, so reexports and cyclic
    // namespace aliases retain the actual module identity.
    let mut modules = vec![plan.loaded[loaded_index].module.clone()];
    let mut index = 0;
    while index < modules.len() {
        let exports = &linked
            .exports
            .iter()
            .find(|module| module.path == modules[index])
            .ok_or(LoadError::Publication(P::MissingModuleSource))?
            .values;
        for export in exports {
            if export.kind == ast::ExportKind::Module && !modules.contains(&export.target) {
                modules.push(export.target.clone());
            }
        }
        index += 1;
    }
    let namespace_names: Vec<Box<str>> = (0..modules.len())
        .map(|index| format!("$namespace{index}").into())
        .collect();
    let mut body = vec![];
    for name in &namespace_names {
        body.push(Stmt {
            kind: StmtKind::Binding(
                name.clone(),
                None,
                false,
                false,
                expression(
                    ExprKind::ObjectLiteral(ast::ObjectKind::Typed, vec![]),
                    request.span,
                ),
            ),
            span: request.span,
        });
    }
    for (index, module) in modules.iter().enumerate() {
        let exports = linked
            .exports
            .iter()
            .find(|entry| entry.path == *module)
            .unwrap()
            .values
            .clone();
        let mut names = std::collections::HashSet::new();
        for export in exports {
            if !names.insert(export.name.clone()) {
                return Err(LoadError::AmbiguousExport {
                    path: canonical.clone(),
                    name: export.name,
                });
            }
            let target = expression(ExprKind::Name(export.target.clone()), request.span);
            let value = match export.kind {
                ast::ExportKind::Module => expression(
                    ExprKind::Name(
                        namespace_names[modules
                            .iter()
                            .position(|module| *module == export.target)
                            .unwrap()]
                        .clone(),
                    ),
                    request.span,
                ),
                ast::ExportKind::Type => expression(
                    ExprKind::TypeDescriptor(ast::ExportedType {
                        name: export.name.clone(),
                        identity: export.target.clone(),
                    }),
                    request.span,
                ),
                ast::ExportKind::Value => {
                    if linked.functions().any(|function| {
                        function.name == export.target && !function.type_params.is_empty()
                    }) {
                        expression(
                            ExprKind::DeferredCallable(export.target.clone()),
                            request.span,
                        )
                    } else if linked
                        .functions()
                        .any(|function| function.name == export.target)
                        || linked
                            .native
                            .functions
                            .iter()
                            .any(|function| function.function.name == export.target)
                    {
                        target
                    } else {
                        expression(
                            ExprKind::IntrinsicCall(
                                Intrinsic::CloneValue.id(),
                                vec![],
                                vec![expression(
                                    ExprKind::Unary(ast::Operator::Borrow, Box::new(target)),
                                    request.span,
                                )],
                            ),
                            request.span,
                        )
                    }
                }
            };
            let target = expression(
                ExprKind::Field(
                    Box::new(expression(
                        ExprKind::Name(namespace_names[index].clone()),
                        request.span,
                    )),
                    export.name,
                ),
                request.span,
            );
            body.push(Stmt {
                kind: StmtKind::Assign(target, value),
                span: request.span,
            });
        }
    }
    for name in &namespace_names {
        body.push(Stmt {
            kind: StmtKind::Evaluate(expression(
                ExprKind::IntrinsicCall(
                    Intrinsic::ObjectFreeze.id(),
                    vec![],
                    vec![expression(ExprKind::Name(name.clone()), request.span)],
                ),
                request.span,
            )),
            span: request.span,
        });
    }
    let namespace = expression(ExprKind::Name(namespace_names[0].clone()), request.span);
    let erased = expression(
        ExprKind::Cast(Box::new(namespace), Type::Dynamic),
        request.span,
    );
    let error = Type::Storage(StorageKind::LoadError, Box::new(Type::Void));
    let result = Type::Storage(
        StorageKind::Result,
        Box::new(Type::Tuple(vec![Type::Dynamic, error.clone()])),
    );
    let returned = expression(
        ExprKind::EnumConstruct(
            cyallo_ast::VariantId::RESULT_OK,
            result.clone(),
            Some(Box::new(erased)),
        ),
        request.span,
    );
    body.push(Stmt {
        kind: StmtKind::Return(returned),
        span: request.span,
    });
    linked.entry_mut().source.functions.push(function(
        factory_name.clone(),
        vec![],
        result,
        body,
        request.span,
    ));
    let compiled = compiler::compile_resolved_with_cache(
        &plan.archive,
        linked,
        plan.options,
        true,
        program.module_cache.clone(),
    )
    .map_err(|diagnostic| {
        localize(
            &plan,
            LoadError::Check {
                path: canonical.clone(),
                diagnostic,
            },
        )
    })?;
    let (mut extension, entry, factory) = publication::publish(&program, compiled, &factory_name)
        .map_err(|error| localize(&plan, error))?;
    plan.loaded[loaded_index].factory = Some(factory);
    if let Some(entry) = entry {
        plan.loaded[loaded_index].initializer.get_or_insert(entry);
    }
    let initializers = plan.loaded.iter().filter_map(|m| m.initializer).collect();
    extension.dynamic_plan = Some(Arc::new(plan.clone()));
    extension.source_modules = plan.ranges.into_boxed_slice();
    extension.module_loader = Some(loader);
    Ok(CompiledLoad {
        key: loaded_index,
        program: Some(Arc::new(extension)),
        factory,
        initializers,
    })
}

fn inline_modules(
    ast: &mut Ast,
    directory: &Path,
    files: &mut HashMap<PathBuf, Ast>,
) -> Result<(), LoadError> {
    for module in &mut ast.modules {
        let child = directory.join(module.name.as_ref());
        if module.body.is_none() {
            let candidates = [
                directory.join(format!("{}.ya", module.name)),
                child.join("mod.ya"),
            ];
            let path = candidates
                .iter()
                .find(|path| files.contains_key(*path))
                .ok_or(LoadError::Publication(P::MissingModuleSource))?;
            module.body = Some(Box::new(files.remove(path).unwrap()));
        }
        inline_modules(module.body.as_mut().unwrap(), &child, files)?;
    }
    Ok(())
}

fn localize(plan: &CompilePlan, mut error: LoadError) -> LoadError {
    if let LoadError::Check { path, diagnostic } = &mut error
        && let Some((name, _, local)) =
            crate::modules::diagnostic_context(&plan.archive, &plan.ranges, diagnostic)
    {
        *path = PathBuf::from(name);
        *diagnostic = local;
    }
    error
}
