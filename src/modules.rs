//! Package linking and lexical extension imports. Imports never execute plugin code.
use crate::{CompileOptions, Diagnostic, Span};
use cyallo_ast::*;
use std::path::Path;
#[derive(Debug, Clone)]
pub struct SourceModule {
    pub path: String,
    pub source: String,
}
#[derive(Debug, Clone)]
pub struct SourceRange {
    pub path: Box<str>,
    pub span: Span,
}
pub fn source_ranges(sources: &[SourceModule]) -> Vec<SourceRange> {
    let mut offset = 0;
    sources
        .iter()
        .map(|m| {
            offset += format!("// module: {}\n", m.path).len() as u32;
            let span = Span::new(offset, offset + m.source.len() as u32);
            offset = span.end + 1;
            SourceRange {
                path: m.path.clone().into(),
                span,
            }
        })
        .collect()
}
pub fn diagnostic_context<'a>(
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
        &range.path,
        &source[range.span.start as usize..range.span.end as usize],
        local,
    ))
}
impl SourceModule {
    pub fn new(path: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: source.into(),
        }
    }
}
fn shift_ast(ast: &mut Ast, offset: u32) -> Result<(), Diagnostic> {
    fn span(s: &mut Span, o: u32) {
        s.start += o;
        s.end += o;
    }
    fn block(body: &mut [Stmt], o: u32) -> Result<(), Diagnostic> {
        fn statements(body: &mut [Stmt], o: u32) {
            for s in body {
                span(&mut s.span, o);
                match &mut s.kind {
                    StmtKind::If(_, a, b) => {
                        statements(a, o);
                        statements(b, o);
                    }
                    StmtKind::While(_, b)
                    | StmtKind::Repeat(b, _)
                    | StmtKind::For(_, _, _, _, b) => statements(b, o),
                    StmtKind::Case(_, arms, b) => {
                        for (_, b) in arms {
                            statements(b, o);
                        }
                        statements(b, o);
                    }
                    _ => {}
                }
            }
        }
        statements(body, o);
        cyallo_ast::visit::visit_block(
            body,
            &mut |e| {
                span(&mut e.span, o);
                if let ExprKind::Closure(f, _) = &mut e.kind {
                    function(f, o)?;
                }
                Ok(())
            },
            &mut |_, _| Ok(()),
        )
    }
    fn function(f: &mut Function, o: u32) -> Result<(), Diagnostic> {
        span(&mut f.span, o);
        for attribute in [
            &mut f.attributes.heat_span,
            &mut f.attributes.compilation_span,
            &mut f.attributes.inline_span,
        ]
        .into_iter()
        .flatten()
        {
            span(attribute, o);
        }
        for p in &mut f.params {
            span(&mut p.span, o);
        }
        block(&mut f.body, o)
    }
    block(&mut ast.body, offset)?;
    for f in &mut ast.functions {
        function(f, offset)?;
    }
    for r in &mut ast.records {
        span(&mut r.span, offset);
    }
    for t in &mut ast.traits {
        span(&mut t.span, offset);
        for f in &mut t.methods {
            function(f, offset)?;
        }
    }
    for imp in &mut ast.implementations {
        span(&mut imp.span, offset);
        for f in &mut imp.methods {
            function(f, offset)?;
        }
    }
    for p in &mut ast.properties {
        span(&mut p.span, offset);
        function(&mut p.getter, offset)?;
    }
    for i in &mut ast.imports {
        span(&mut i.span, offset);
    }
    Ok(())
}
pub fn compile_modules(
    entry: usize,
    sources: &[SourceModule],
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    if entry >= sources.len() {
        return Err(Diagnostic::new(
            "E140",
            "module entry index is invalid",
            Span::default(),
        ));
    }
    let mut source = String::new();
    let mut offsets = Vec::with_capacity(sources.len());
    for module in sources {
        source.push_str(&format!("// module: {}\n", module.path));
        let offset = u32::try_from(source.len()).map_err(|_| {
            Diagnostic::new("E140", "module source exceeds span limit", Span::default())
        })?;
        offsets.push(offset);
        source.push_str(&module.source);
        source.push('\n');
    }
    u32::try_from(source.len()).map_err(|_| {
        Diagnostic::new("E140", "module source exceeds span limit", Span::default())
    })?;
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
        shift_ast(&mut ast, offsets[i])?;
        asts.push(ast);
    }
    let ast = crate::namespace::link(
        asts,
        &sources.iter().map(|s| s.path.clone()).collect::<Vec<_>>(),
        entry,
    )?;
    let mut program = crate::compiler::compile_ast(&source, ast, options, deferred)?;
    program.source_modules = source_ranges(sources).into_boxed_slice();
    Ok(program)
}
pub fn compile_path(
    path: &Path,
    options: CompileOptions,
    deferred: bool,
) -> Result<crate::ir::Program, Diagnostic> {
    let sources = load_path(path)?;
    compile_modules(sources.len() - 1, &sources, options, deferred)
}
pub fn source_archive(sources: &[SourceModule]) -> String {
    sources
        .iter()
        .map(|m| format!("// module: {}\n{}\n", m.path, m.source))
        .collect()
}
pub fn load_path(path: &Path) -> Result<Vec<SourceModule>, Diagnostic> {
    fn declarations(
        dir: &Path,
        modules: &[ModuleDeclaration],
        out: &mut Vec<SourceModule>,
        active: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Diagnostic> {
        for module in modules {
            let child_dir = dir.join(module.name.as_ref());
            if let Some(body) = &module.body {
                declarations(&child_dir, &body.modules, out, active)?;
                continue;
            }
            let candidates = [
                dir.join(format!("{}.ya", module.name)),
                child_dir.join("mod.ya"),
            ];
            let files: Vec<_> = candidates.iter().filter(|p| p.is_file()).collect();
            if files.len() != 1 {
                return Err(Diagnostic::new(
                    "E140",
                    format!("expected exactly one source for module {}", module.name),
                    module.span,
                ));
            }
            let file = files[0];
            if std::fs::symlink_metadata(file)
                .map_err(|e| Diagnostic::new("E140", e.to_string(), module.span))?
                .file_type()
                .is_symlink()
            {
                return Err(Diagnostic::new(
                    "E140",
                    "module sources cannot be symlinks",
                    module.span,
                ));
            }
            load(file, &child_dir, out, active)?;
        }
        Ok(())
    }
    fn load(
        path: &Path,
        dir: &Path,
        out: &mut Vec<SourceModule>,
        active: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Diagnostic> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| Diagnostic::new("E140", e.to_string(), Span::default()))?;
        if !active.insert(canonical.clone()) {
            return Err(Diagnostic::new(
                "E140",
                "cyclic module source",
                Span::default(),
            ));
        }
        let text = std::fs::read_to_string(&canonical)
            .map_err(|e| Diagnostic::new("E140", e.to_string(), Span::default()))?;
        let ast = cyallo_parser::parse_modern(&text, false)?;
        declarations(dir, &ast.modules, out, active)?;
        active.remove(&canonical);
        out.push(SourceModule::new(canonical.display().to_string(), text));
        Ok(())
    }
    let entry = std::fs::canonicalize(path)
        .map_err(|e| Diagnostic::new("E140", e.to_string(), Span::default()))?;
    let mut sources = Vec::new();
    load(
        &entry,
        entry.parent().unwrap_or(Path::new(".")),
        &mut sources,
        &mut std::collections::HashSet::new(),
    )?;
    Ok(sources)
}
