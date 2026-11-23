//! Package linking and lexical extension imports. Imports never execute plugin code.
use crate::{
    CompileOptions, Diagnostic, Span,
    dynamic_modules::{ArchivedSyntax, SourceTrees},
};
use cyallo_ir::module_graph::{ModuleError, module_diagnostic};
pub use cyallo_modules::SourceModule;
use cyallo_modules::loader::ParsedModule;
use std::{path::Path, sync::Arc};
#[derive(Debug, Clone)]
pub struct SourceRange {
    /// None uses the entry name supplied by the diagnostic caller.
    pub path: Option<Box<str>>,
    pub span: Span,
}
pub fn source_ranges(sources: &[SourceModule]) -> Vec<SourceRange> {
    source_context(ModuleInputs::Text(sources)).1
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
    inputs: ModuleInputs<'_>,
    options: CompileOptions,
    deferred: bool,
    policy: &cyallo_ir::import_policy::ImportPolicy,
    core: &cyallo_ir::core_package::CorePackage<'_>,
    cache: std::sync::Arc<cyallo_ir::module_cache::ModuleCache>,
) -> Result<crate::ir::Program, Diagnostic> {
    let sources = inputs.sources();
    if entry >= sources.len() {
        return Err(module_diagnostic(ModuleError::InvalidEntry {
            index: entry,
            count: sources.len(),
        }));
    }
    let mut source = String::new();
    let mut offsets = Vec::with_capacity(sources.len());
    for (path, text) in &sources {
        source.push_str(&format!("// module: {path}\n"));
        let offset = u32::try_from(source.len()).map_err(|_| {
            module_diagnostic(ModuleError::SourceTooLarge {
                bytes: source.len(),
            })
        })?;
        offsets.push(offset);
        source.push_str(text);
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
    let trees = inputs.archive_syntax(&offsets, cyallo_compile_jobs::Jobs(options.jobs))?;
    let paths: Vec<_> = sources.iter().map(|&(path, _)| path.to_owned()).collect();
    let (linker_trees, indexed_bodies) =
        trees.materialize_with_indexed(entry, options.dialect == crate::Dialect::Modern)?;
    let mut ast = crate::namespace::link_program(
        linker_trees,
        &paths,
        entry,
        policy,
        core,
        core_archive.base,
        options.entry,
        indexed_bodies,
    )?;
    ast.source_files = sources
        .iter()
        .zip(&offsets)
        .map(
            |(&(path, text), &offset)| cyallo_ir::module_graph::SourceFile {
                id: cyallo_ir::module_graph::SourceFileId {
                    path: path.into(),
                    trust: cyallo_ast::SourceTrust::User,
                },
                span: Span::new(offset, offset + text.len() as u32),
            },
        )
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
        .map(|(&(path, text), offset)| SourceRange {
            path: Some(path.into()),
            span: Span::new(offset, offset + text.len() as u32),
        })
        .collect();
    ranges.extend(core_archive.ranges.into_iter().map(|range| SourceRange {
        path: Some(range.path),
        span: range.span,
    }));
    let mut plan = crate::dynamic_modules::CompilePlan::new(
        crate::dynamic_modules::CompileSources {
            source_files,
            trees,
            paths,
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

enum ModuleInputs<'a> {
    Text(&'a [SourceModule]),
    Parsed(&'a [ParsedModule]),
}

impl ModuleInputs<'_> {
    fn sources(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Text(files) => files
                .iter()
                .map(|f| (f.path.as_str(), f.source.as_str()))
                .collect(),
            Self::Parsed(files) => files
                .iter()
                .map(|f| (f.path.as_str(), f.file.source().text()))
                .collect(),
        }
    }

    fn archive_syntax(
        &self,
        offsets: &[u32],
        jobs: cyallo_compile_jobs::Jobs,
    ) -> Result<SourceTrees, Diagnostic> {
        let files = match self {
            Self::Text(files) => {
                let texts: Vec<_> = files.iter().map(|f| f.source.as_str()).collect();
                cyallo_parser::parse_modern_snapshots(&texts, jobs)
                    .into_iter()
                    .zip(offsets.iter().copied())
                    .map(|(file, offset)| {
                        let file = match file {
                            Ok(file) => file,
                            Err(mut diagnostic) => {
                                cyallo_ast::spans::shift_diagnostic(&mut diagnostic, offset)?;
                                return Err(diagnostic);
                            }
                        };
                        Ok(ArchivedSyntax {
                            file: Arc::new(file),
                            offset,
                        })
                    })
                    .collect::<Result<_, Diagnostic>>()?
            }
            Self::Parsed(files) => files
                .iter()
                .zip(offsets.iter().copied())
                .map(|(module, offset)| ArchivedSyntax {
                    file: Arc::clone(&module.file),
                    offset,
                })
                .collect(),
        };
        Ok(SourceTrees::new(files))
    }
}

/// Compile snapshots produced during file discovery, reusing the original parse.
pub fn compile_parsed_modules(
    entry: usize,
    sources: &[ParsedModule],
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    compile_modules_cached(
        entry,
        ModuleInputs::Parsed(sources),
        options,
        deferred,
        &cyallo_ir::import_policy::ImportPolicy::default(),
        &cyallo_ir::core_package::CorePackage::embedded(),
        Default::default(),
    )
}

pub fn compile_path(
    path: &Path,
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    let loader = crate::module_loader::ModuleLoader::for_entry(path)
        .map_err(crate::module_loader::module_load_diagnostic)?;
    let sources = loader
        .load_entry(
            path,
            crate::Jobs(options.jobs),
            options.entry == cyallo_ir::EntryMode::Main,
        )
        .map_err(crate::module_loader::module_load_diagnostic)?;
    Ok(
        compile_parsed_modules(sources.len() - 1, &sources, options, deferred)?
            .with_module_loader(loader),
    )
}
pub fn load_path(path: &Path) -> Result<Vec<SourceModule>, Diagnostic> {
    crate::module_loader::load_path(path)
}

pub fn source_archive(sources: &[SourceModule]) -> String {
    source_context(ModuleInputs::Text(sources)).0
}

/// Source text and coordinate ranges for rendering diagnostics from parsed modules.
pub fn parsed_source_context(sources: &[ParsedModule]) -> (String, Vec<SourceRange>) {
    source_context(ModuleInputs::Parsed(sources))
}

fn source_context(inputs: ModuleInputs<'_>) -> (String, Vec<SourceRange>) {
    let mut archive = String::new();
    let mut ranges = vec![];
    for (path, text) in inputs.sources() {
        archive.push_str(&format!("// module: {path}\n"));
        let start = archive.len();
        archive.push_str(text);
        if let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(archive.len())) {
            ranges.push(SourceRange {
                path: Some(path.into()),
                span: Span::new(start, end),
            });
        }
        archive.push('\n');
    }
    // Compilation reports layout failures through the checked package API;
    // diagnostics can still render the user archive if its suffix cannot fit.
    if let Ok(core) = cyallo_ir::core_package::CorePackage::embedded().append_to(&mut archive) {
        ranges.extend(core.ranges.into_iter().map(|range| SourceRange {
            path: Some(range.path),
            span: range.span,
        }));
    }
    (archive, ranges)
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
            ModuleInputs::Text(sources),
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
        ModuleInputs::Text(sources),
        options,
        deferred,
        policy,
        core,
        Default::default(),
    )
}
