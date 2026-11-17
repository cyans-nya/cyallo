//! Source adapters and executable packaging. Language semantics live in cyallo-ir.
use crate::ir;
use cyallo_ast::*;
pub(crate) use cyallo_ir::analysis::contextualize;
pub use cyallo_ir::{CompileOptions, Dialect};
type Result<T> = std::result::Result<T, Diagnostic>;
pub fn compile(source: &str) -> Result<ir::Program> {
    compile_with_options(source, CompileOptions::default())
}
pub fn compile_with_options(source: &str, options: CompileOptions) -> Result<ir::Program> {
    compile_source(source, options, false)
}
pub(crate) fn compile_source(
    source: &str,
    options: CompileOptions,
    repl: bool,
) -> Result<ir::Program> {
    compile_source_inner(source, options, repl, false)
}
/// Parse declarations and check the entry body; defer routine bodies until used.
pub fn compile_for_execution(source: &str, options: CompileOptions) -> Result<ir::Program> {
    compile_source_inner(source, options, false, true)
}
fn compile_source_inner(
    source: &str,
    options: CompileOptions,
    repl: bool,
    deferred: bool,
) -> Result<ir::Program> {
    compile_source_configured(
        source,
        options,
        repl,
        deferred,
        &cyallo_ir::import_policy::ImportPolicy::default(),
        &cyallo_ir::core_package::CorePackage::embedded(),
    )
}
pub fn compile_with_import_policy(
    source: &str,
    options: CompileOptions,
    policy: &cyallo_ir::import_policy::ImportPolicy,
) -> Result<ir::Program> {
    compile_source_configured(
        source,
        options,
        false,
        false,
        policy,
        &cyallo_ir::core_package::CorePackage::embedded(),
    )
}
pub fn compile_with_core_package(
    source: &str,
    options: CompileOptions,
    policy: &cyallo_ir::import_policy::ImportPolicy,
    core: &cyallo_ir::core_package::CorePackage<'_>,
) -> Result<ir::Program> {
    compile_source_configured(source, options, false, false, policy, core)
}
fn compile_source_configured(
    source: &str,
    options: CompileOptions,
    repl: bool,
    deferred: bool,
    policy: &cyallo_ir::import_policy::ImportPolicy,
    core: &cyallo_ir::core_package::CorePackage<'_>,
) -> Result<ir::Program> {
    let ast = if options.dialect == Dialect::Modern {
        cyallo_parser::parse_modern_with_jobs(
            source,
            repl,
            cyallo_compile_jobs::Jobs(options.jobs),
        )?
    } else if repl {
        caie_int_parser::parse_repl(source)?
    } else {
        caie_int_parser::parse(source)?
    };
    if options.dialect == Dialect::Modern {
        let mut archive = source.to_owned();
        let core_archive = core
            .append_to(&mut archive)
            .map_err(cyallo_ir::core_package::invalid_core_source)?;
        let raw = ast.clone();
        let mut ast = crate::namespace::link_with_core(
            vec![ast],
            &["main.ya".into()],
            0,
            policy,
            core,
            core_archive.base,
        )?;
        ast.source_files = std::iter::once(cyallo_ir::module_graph::SourceFile {
            id: cyallo_ir::module_graph::SourceFileId {
                path: "main.ya".into(),
                trust: SourceTrust::User,
            },
            span: Span::new(0, source.len() as u32),
        })
        .chain(
            core_archive
                .ranges
                .iter()
                .map(|r| cyallo_ir::module_graph::SourceFile {
                    id: cyallo_ir::module_graph::SourceFileId {
                        path: r.path.clone(),
                        trust: SourceTrust::Core,
                    },
                    span: r.span,
                }),
        )
        .collect();
        let source_files = ast.source_files.clone();
        let authorization = ast.native.import_policy.clone();
        let mut program = compile_resolved(&archive, ast, options, deferred)?;
        let mut ranges = vec![crate::modules::SourceRange {
            path: None,
            span: Span::new(0, source.len() as u32),
        }];
        ranges.extend(
            core_archive
                .ranges
                .into_iter()
                .map(|range| crate::modules::SourceRange {
                    path: Some(range.path),
                    span: range.span,
                }),
        );
        let mut plan = crate::dynamic_modules::CompilePlan::new(
            crate::dynamic_modules::CompileSources {
                source_files,
                asts: vec![raw],
                paths: vec!["main.ya".into()],
                entry: 0,
                archive,
                ranges: ranges.clone(),
                core_base: core_archive.base,
            },
            options,
            policy,
            core,
        );
        plan.authorization = authorization;
        program.dynamic_plan = Some(std::sync::Arc::new(plan));
        program.source_modules = ranges.into_boxed_slice();
        Ok(program)
    } else {
        compile_ast(source, ast, options, deferred)
    }
}
pub(crate) fn compile_ast(
    source: &str,
    mut ast: Ast,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    cyallo_ir::generics::specialize(&mut ast, options)?;
    if options.dialect == Dialect::Modern {
        crate::constants::evaluate(source, &mut ast, options)?;
    }
    compile_ast_unfolded(source, ast, options, deferred)
}
/// Constant evaluation uses the same checked lowering and interpreter, without
/// recursively starting another constant-evaluation pass.
pub(crate) fn compile_ast_unfolded(
    source: &str,
    ast: Ast,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    let checked = cyallo_ir::analysis::analyze(source.len(), ast, options, deferred)?;
    Ok(package(source, checked, options))
}

pub(crate) fn compile_resolved(
    source: &str,
    graph: cyallo_ir::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    compile_resolved_with_cache(
        source,
        graph,
        options,
        deferred,
        std::sync::Arc::new(cyallo_ir::module_cache::ModuleCache::default()),
    )
}
pub(crate) fn compile_resolved_with_cache(
    source: &str,
    mut graph: cyallo_ir::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
    cache: std::sync::Arc<cyallo_ir::module_cache::ModuleCache>,
) -> Result<ir::Program> {
    let native = graph.native.clone();
    graph.with_sources(|asts| cyallo_ir::generics::specialize_modules(asts, &native, options))?;
    crate::constants::evaluate_modules(source, &mut graph, options)?;
    compile_resolved_unfolded_cached(source, graph, options, deferred, cache)
}
pub(crate) fn compile_resolved_unfolded(
    source: &str,
    graph: cyallo_ir::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
) -> Result<ir::Program> {
    compile_resolved_unfolded_cached(
        source,
        graph,
        options,
        deferred,
        std::sync::Arc::new(cyallo_ir::module_cache::ModuleCache::default()),
    )
}
fn compile_resolved_unfolded_cached(
    source: &str,
    mut graph: cyallo_ir::module_graph::ResolvedProgram,
    options: CompileOptions,
    deferred: bool,
    cache: std::sync::Arc<cyallo_ir::module_cache::ModuleCache>,
) -> Result<ir::Program> {
    let checked = cyallo_ir::analysis::analyze_modules_cached(
        source.len(),
        &mut graph,
        options,
        deferred,
        &cache,
    )?;
    let units = checked.modules;
    let mut program = package(source, checked.executable, options);
    program.module_units = units.into();
    program.module_cache = cache;
    Ok(program)
}
fn package(source: &str, checked: cyallo_ir::ir::Module, options: CompileOptions) -> ir::Program {
    let bytecode =
        crate::bytecode::Module::lower(&checked.functions, cyallo_compile_jobs::Jobs(options.jobs));
    ir::Program {
        module_units: Default::default(),
        module_cache: Default::default(),
        dynamic_plan: None,
        module_loader: None,
        parent: None,
        records: checked.records,
        requires_tracing: checked.requires_tracing,
        jobs: crate::Jobs(options.jobs),
        inspection: None,
        snapshot_id: 0,
        source_modules: Box::default(),
        services: checked.services,
        source: source.into(),
        functions: checked.functions,
        bytecode,
        code_versions: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
    }
}
