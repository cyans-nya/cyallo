//! Package linking and lexical extension imports. Imports never execute plugin code.
use crate::{CompileOptions, Diagnostic, Span};
use cyallo_ir::module_graph::{ModuleError, module_diagnostic};
pub use cyallo_modules::SourceModule;
use std::path::Path;
#[derive(Debug, Clone)]
pub struct SourceRange {
    /// None uses the entry name supplied by the diagnostic caller.
    pub path: Option<Box<str>>,
    pub span: Span,
}
pub fn source_ranges(sources: &[SourceModule]) -> Vec<SourceRange> {
    let mut offset = 0;
    let mut ranges: Vec<_> = sources
        .iter()
        .map(|m| {
            offset += format!("// module: {}\n", m.path).len() as u32;
            let span = Span::new(offset, offset + m.source.len() as u32);
            offset = span.end + 1;
            SourceRange {
                path: Some(m.path.clone().into()),
                span,
            }
        })
        .collect();
    let mut archive = user_source_archive(sources);
    if let Ok(core) = cyallo_ir::core_package::CorePackage::embedded().append_to(&mut archive) {
        ranges.extend(core.ranges.into_iter().map(|range| SourceRange {
            path: Some(range.path),
            span: range.span,
        }));
    }
    ranges
}
pub fn diagnostic_context<'a>(
    source: &'a str,
    ranges: &'a [SourceRange],
    diagnostic: &Diagnostic,
) -> Option<(&'a str, &'a str, Diagnostic)> {
    diagnostic_context_with_name("<source>", source, ranges, diagnostic)
}
pub fn diagnostic_context_with_name<'a>(
    name: &'a str,
    source: &'a str,
    ranges: &'a [SourceRange],
    diagnostic: &Diagnostic,
) -> Option<(&'a str, &'a str, Diagnostic)> {
    let range = ranges
        .iter()
        .find(|r| diagnostic.span.start >= r.span.start && diagnostic.span.end <= r.span.end)?;
    let mut local = diagnostic.clone();
    local.span.start -= range.span.start;
    local.span.end -= range.span.start;
    local.related.retain_mut(|(span, _)| {
        if span.start >= range.span.start && span.end <= range.span.end {
            span.start -= range.span.start;
            span.end -= range.span.start;
            true
        } else {
            false
        }
    });
    Some((
        range.path.as_deref().unwrap_or(name),
        &source[range.span.start as usize..range.span.end as usize],
        local,
    ))
}
pub fn compile_modules(
    entry: usize,
    sources: &[SourceModule],
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    compile_modules_with_policy(
        entry,
        sources,
        options,
        deferred,
        &cyallo_ir::import_policy::ImportPolicy::default(),
    )
}
pub fn compile_modules_with_policy(
    entry: usize,
    sources: &[SourceModule],
    options: CompileOptions,
    deferred: bool,
    policy: &cyallo_ir::import_policy::ImportPolicy,
) -> Result<crate::ir::Program, Diagnostic> {
    compile_modules_with_core(
        entry,
        sources,
        options,
        deferred,
        policy,
        &cyallo_ir::core_package::CorePackage::embedded(),
    )
}
fn compile_modules_cached(
    entry: usize,
    sources: &[SourceModule],
    options: CompileOptions,
    deferred: bool,
    policy: &cyallo_ir::import_policy::ImportPolicy,
    core: &cyallo_ir::core_package::CorePackage<'_>,
    cache: std::sync::Arc<cyallo_ir::module_cache::ModuleCache>,
) -> Result<crate::ir::Program, Diagnostic> {
    if entry >= sources.len() {
        return Err(module_diagnostic(ModuleError::InvalidEntry {
            index: entry,
            count: sources.len(),
        }));
    }
    let mut source = String::new();
    let mut offsets = Vec::with_capacity(sources.len());
    for module in sources {
        source.push_str(&format!("// module: {}\n", module.path));
        let offset = u32::try_from(source.len()).map_err(|_| {
            module_diagnostic(ModuleError::SourceTooLarge {
                bytes: source.len(),
            })
        })?;
        offsets.push(offset);
        source.push_str(&module.source);
        source.push('\n');
    }
    u32::try_from(source.len()).map_err(|_| {
        module_diagnostic(ModuleError::SourceTooLarge {
            bytes: source.len(),
        })
    })?;
    let core_archive = core
        .append_to(&mut source)
        .map_err(cyallo_ir::core_package::invalid_core_source)?;
    let jobs = cyallo_compile_jobs::Jobs(options.jobs);
    let indexed = jobs.map(sources, 2, |_, _, module| {
        cyallo_parser::index_modern(&module.source)
    });
    let work: Vec<_> = indexed
        .iter()
        .enumerate()
        .flat_map(|(file, index)| {
            (0..index.as_ref().map_or(0, |index| index.units().len())).map(move |unit| (file, unit))
        })
        .collect();
    let parsed = jobs.map(&work, 4, |_, _, &(file, unit)| {
        indexed[file]
            .as_ref()
            .expect("indexed file")
            .parse_unit(unit, false)
    });
    let mut per_file: Vec<Vec<_>> = (0..sources.len()).map(|_| Vec::new()).collect();
    for ((file, _), unit) in work.into_iter().zip(parsed) {
        per_file[file].push(unit);
    }
    let mut asts = Vec::with_capacity(sources.len());
    for (i, index) in indexed.into_iter().enumerate() {
        // Keep lexical and syntax diagnostics in file order, including when a
        // later file fails lexing before an earlier parse job completes.
        let mut ast = index
            .and_then(|index| index.assemble(std::mem::take(&mut per_file[i])))
            .map_err(|mut d| {
                d.span.start += offsets[i];
                d.span.end += offsets[i];
                for (span, _) in &mut d.related {
                    span.start += offsets[i];
                    span.end += offsets[i];
                }
                d
            })?;
        cyallo_ast::spans::shift_ast(&mut ast, offsets[i])?;
        asts.push(ast);
    }
    let raw = asts.clone();
    let mut ast = crate::namespace::link_with_core(
        asts,
        &sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
        entry,
        policy,
        core,
        core_archive.base,
    )?;
    ast.source_files = sources
        .iter()
        .zip(&offsets)
        .map(|(m, &offset)| cyallo_ir::module_graph::SourceFile {
            id: cyallo_ir::module_graph::SourceFileId {
                path: m.path.clone().into(),
                trust: cyallo_ast::SourceTrust::User,
            },
            span: Span::new(offset, offset + m.source.len() as u32),
        })
        .chain(
            core_archive
                .ranges
                .iter()
                .map(|r| cyallo_ir::module_graph::SourceFile {
                    id: cyallo_ir::module_graph::SourceFileId {
                        path: r.path.clone(),
                        trust: cyallo_ast::SourceTrust::Core,
                    },
                    span: r.span,
                }),
        )
        .collect();
    let source_files = ast.source_files.clone();
    let authorization = ast.native.import_policy.clone();
    let mut program =
        crate::compiler::compile_resolved_with_cache(&source, ast, options, deferred, cache)?;
    let mut ranges: Vec<_> = sources
        .iter()
        .zip(offsets)
        .map(|(module, offset)| SourceRange {
            path: Some(module.path.clone().into()),
            span: Span::new(offset, offset + module.source.len() as u32),
        })
        .collect();
    ranges.extend(core_archive.ranges.into_iter().map(|range| SourceRange {
        path: Some(range.path),
        span: range.span,
    }));
    let mut plan = crate::dynamic_modules::CompilePlan::new(
        crate::dynamic_modules::CompileSources {
            source_files,
            asts: raw,
            paths: sources.iter().map(|s| s.path.clone()).collect(),
            entry,
            archive: source,
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
}
pub fn compile_path(
    path: &Path,
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    let loader = crate::module_loader::ModuleLoader::for_entry(path)
        .map_err(crate::module_loader::module_load_diagnostic)?;
    let sources = loader
        .load(path)
        .map_err(crate::module_loader::module_load_diagnostic)?;
    Ok(compile_modules(sources.len() - 1, &sources, options, deferred)?.with_module_loader(loader))
}
fn user_source_archive(sources: &[SourceModule]) -> String {
    sources
        .iter()
        .map(|m| format!("// module: {}\n{}\n", m.path, m.source))
        .collect()
}
pub fn load_path(path: &Path) -> Result<Vec<SourceModule>, Diagnostic> {
    crate::module_loader::load_path(path)
}

pub fn source_archive(sources: &[SourceModule]) -> String {
    let mut archive = user_source_archive(sources);
    // Compilation reports layout failures through the checked package API;
    // diagnostics can still render the user archive if its suffix cannot fit.
    let _ = cyallo_ir::core_package::CorePackage::embedded().append_to(&mut archive);
    archive
}

/// Owns reusable checked module artifacts. Sessions own all runtime state.
#[derive(Debug, Default)]
pub struct Compiler {
    cache: std::sync::Arc<cyallo_ir::module_cache::ModuleCache>,
}
impl Compiler {
    pub fn compile(
        &self,
        entry: usize,
        sources: &[SourceModule],
        options: CompileOptions,
        deferred: bool,
    ) -> Result<crate::ir::Program, Diagnostic> {
        self.compile_with_policy(
            entry,
            sources,
            options,
            deferred,
            &cyallo_ir::import_policy::ImportPolicy::default(),
        )
    }
    pub fn compile_with_policy(
        &self,
        entry: usize,
        sources: &[SourceModule],
        options: CompileOptions,
        deferred: bool,
        policy: &cyallo_ir::import_policy::ImportPolicy,
    ) -> Result<crate::ir::Program, Diagnostic> {
        compile_modules_cached(
            entry,
            sources,
            options,
            deferred,
            policy,
            &cyallo_ir::core_package::CorePackage::embedded(),
            self.cache.clone(),
        )
    }
}
pub fn compile_modules_with_core(
    entry: usize,
    sources: &[SourceModule],
    options: CompileOptions,
    deferred: bool,
    policy: &cyallo_ir::import_policy::ImportPolicy,
    core: &cyallo_ir::core_package::CorePackage<'_>,
) -> Result<crate::ir::Program, Diagnostic> {
    compile_modules_cached(
        entry,
        sources,
        options,
        deferred,
        policy,
        core,
        Default::default(),
    )
}
