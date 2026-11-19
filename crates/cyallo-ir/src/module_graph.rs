//! Resolved module identities, interfaces, and independently owned source bodies.
use cyallo_ast::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub DeclarationId);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingKind {
    Value,
    Type,
    Module,
    Property,
    World,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBinding {
    pub name: Box<str>,
    pub declaration: DeclarationId,
    pub defining_module: ModuleId,
    pub target: Box<str>,
    pub kind: BindingKind,
    pub public: bool,
}
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub id: ModuleId,
    pub path: Box<str>,
    pub source: Ast,
    /// Imports and exports retain the defining declaration's identity.
    pub bindings: Vec<ModuleBinding>,
}
#[derive(Debug, Clone)]
pub struct ResolvedProgram {
    /// Digests of the entry package's interface modules and every world.
    pub interface: crate::sdk::InterfaceDigests,
    /// Resolver-assigned identities for the entry package's bindable items.
    pub(crate) interface_declarations: Vec<(Box<str>, DeclarationId)>,
    /// Worlds with `include` expanded. Order is declaration order per module.
    pub worlds: Vec<ResolvedWorld>,
    pub source_files: Vec<SourceFile>,
    pub modules: Vec<ResolvedModule>,
    pub entry: ModuleId,
    pub native: NativeDeclarations,
    pub source_names: std::collections::BTreeMap<Box<str>, Box<str>>,
    pub exports: Vec<ResolvedModuleExports>,
}
/// A world after name resolution: its flattened imports and exports, each
/// remembering the world that declared it.
#[derive(Debug, Clone)]
pub struct ResolvedWorld {
    pub id: DeclarationId,
    pub module: ModuleId,
    /// Path of the declaring module plus the world name.
    pub path: Box<str>,
    pub includes: Vec<DeclarationId>,
    pub imports: Vec<ResolvedWorldImport>,
    pub exports: Vec<ResolvedWorldExport>,
    pub digest: crate::sdk::Digest,
}
#[derive(Debug, Clone)]
pub struct ResolvedWorldImport {
    pub origin: DeclarationId,
    /// Field name in the generated environment: the alias or last path segment.
    pub field: Box<str>,
    /// Identity of the imported module.
    pub module: ModuleId,
    pub path: Box<str>,
    /// The exact subset of the module a component may use; `None` is the whole module.
    pub members: Option<Vec<Box<str>>>,
    /// Canonical signatures of `members`, as found in the imported module.
    pub member_signatures: Vec<Box<str>>,
}
#[derive(Debug, Clone)]
pub struct ResolvedWorldExport {
    pub origin: DeclarationId,
    pub name: Box<str>,
    /// The exported signature with its types resolved in the declaring module.
    pub signature: Function,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceFileId {
    pub path: Box<str>,
    pub trust: SourceTrust,
}
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: SourceFileId,
    pub span: Span,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleError {
    MissingEntry(ModuleId),
    InvalidEntry {
        index: usize,
        count: usize,
    },
    SourceTooLarge {
        bytes: usize,
    },
    FunctionReferenceParameter {
        span: Span,
    },
    ConstantCycle {
        declarations: Vec<Box<str>>,
        span: Span,
    },
    DuplicateSymbol {
        symbol: Box<str>,
        span: Span,
    },
    MissingSymbol {
        symbol: Box<str>,
        span: Span,
    },
}
impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEntry(id) => write!(f, "missing entry module {id:?}"),
            Self::InvalidEntry { index, count } => write!(
                f,
                "module entry index {index} is invalid for {count} sources"
            ),
            Self::SourceTooLarge { bytes } => {
                write!(f, "module source exceeds span limit ({bytes} bytes)")
            }
            Self::FunctionReferenceParameter { .. } => {
                f.write_str("FUNCTION parameters must be BYVAL")
            }
            Self::ConstantCycle { declarations, .. } => write!(
                f,
                "cyclic constant initialization: {}",
                declarations
                    .iter()
                    .map(|s| source_name(s))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            Self::DuplicateSymbol { symbol, .. } => {
                write!(f, "duplicate module declaration `{}`", source_name(symbol))
            }
            Self::MissingSymbol { symbol, .. } => {
                write!(f, "unresolved linked declaration `{}`", source_name(symbol))
            }
        }
    }
}
impl std::error::Error for ModuleError {}
pub fn module_diagnostic(error: ModuleError) -> Diagnostic {
    let span = match &error {
        ModuleError::MissingEntry(_)
        | ModuleError::InvalidEntry { .. }
        | ModuleError::SourceTooLarge { .. } => Span::default(),
        ModuleError::FunctionReferenceParameter { span } => *span,
        ModuleError::ConstantCycle { span, .. }
        | ModuleError::DuplicateSymbol { span, .. }
        | ModuleError::MissingSymbol { span, .. } => *span,
    };
    let code = match error {
        ModuleError::FunctionReferenceParameter { .. } => "E102",
        _ => "E140",
    };
    Diagnostic::new(code, error.to_string(), span)
}
impl ResolvedProgram {
    pub fn entry_index(&self) -> usize {
        self.modules
            .iter()
            .position(|m| m.id == self.entry)
            .expect("resolved entry module")
    }
    pub fn functions(&self) -> impl Iterator<Item = &Function> {
        self.modules.iter().flat_map(|m| &m.source.functions)
    }
    pub fn functions_mut(&mut self) -> impl Iterator<Item = &mut Function> {
        self.modules
            .iter_mut()
            .flat_map(|m| &mut m.source.functions)
    }
    pub fn entry_mut(&mut self) -> &mut ResolvedModule {
        let index = self.entry_index();
        &mut self.modules[index]
    }
    pub fn constant_order(&self) -> Result<Vec<(usize, usize)>, ModuleError> {
        let mut positions = vec![];
        let mut names = HashMap::new();
        // Entry execution order remains source order. Imported declarations are
        // evaluated by their dependency graph, never pasted into that body.
        for (module, m) in self.modules.iter().enumerate() {
            for (statement, s) in m.source.body.iter().enumerate() {
                if let StmtKind::Binding(name, _, true, _, _) | StmtKind::Constant(name, _) =
                    &s.kind
                {
                    names.insert(name.clone(), positions.len());
                    positions.push((module, statement));
                }
            }
        }
        fn references(body: &[Stmt]) -> Vec<Box<str>> {
            let mut names = vec![];
            cyallo_ast::visit::walk_block(body, &mut |e| {
                if let ExprKind::Name(name) | ExprKind::Call(name, _) = &e.kind {
                    names.push(name.clone());
                }
            });
            names
        }
        let routines: HashMap<_, _> = self
            .functions()
            .map(|f| (f.name.clone(), references(&f.body)))
            .collect();
        let mut dependencies = vec![vec![]; positions.len()];
        for (i, &(module, statement)) in positions.iter().enumerate() {
            let mut pending = references(std::slice::from_ref(
                &self.modules[module].source.body[statement],
            ));
            let mut visited = HashSet::new();
            while let Some(name) = pending.pop() {
                if !visited.insert(name.clone()) {
                    continue;
                }
                if let Some(&dependency) = names.get(&name) {
                    dependencies[i].push(dependency);
                }
                if let Some(callees) = routines.get(&name) {
                    pending.extend(callees.iter().cloned());
                }
            }
            dependencies[i].sort_unstable();
        }
        fn visit(
            i: usize,
            deps: &[Vec<usize>],
            visiting: &mut HashSet<usize>,
            done: &mut HashSet<usize>,
            out: &mut Vec<usize>,
            positions: &[(usize, usize)],
            graph: &ResolvedProgram,
        ) -> Result<(), ModuleError> {
            if done.contains(&i) {
                return Ok(());
            }
            if !visiting.insert(i) {
                let (m, s) = positions[i];
                let statement = &graph.modules[m].source.body[s];
                let name = match &statement.kind {
                    StmtKind::Binding(n, ..) | StmtKind::Constant(n, _) => n.clone(),
                    _ => unreachable!(),
                };
                return Err(ModuleError::ConstantCycle {
                    declarations: vec![name],
                    span: statement.span,
                });
            }
            for &j in &deps[i] {
                visit(j, deps, visiting, done, out, positions, graph)?
            }
            visiting.remove(&i);
            done.insert(i);
            out.push(i);
            Ok(())
        }
        let mut out = vec![];
        let mut visiting = HashSet::new();
        let mut done = HashSet::new();
        for i in 0..positions.len() {
            visit(
                i,
                &dependencies,
                &mut visiting,
                &mut done,
                &mut out,
                &positions,
                self,
            )?
        }
        Ok(out.into_iter().map(|i| positions[i]).collect())
    }
}

impl ResolvedProgram {
    pub fn with_sources<T>(&mut self, f: impl FnOnce(&mut [Ast]) -> T) -> T {
        let mut sources: Vec<_> = self
            .modules
            .iter_mut()
            .map(|m| std::mem::take(&mut m.source))
            .collect();
        let result = f(&mut sources);
        for (module, source) in self.modules.iter_mut().zip(sources) {
            module.source = source;
        }
        result
    }
}
/// Infer declaration contracts to a fixed point across module interfaces. Only
/// typed bindings are shared: source statements and function bodies stay owned.
pub(crate) fn infer_globals(
    asts: &[Ast],
    types: &crate::declarations::TypeEnvironment,
    options: crate::CompileOptions,
) -> Result<Vec<Vec<crate::ir::Local>>, Diagnostic> {
    let mut globals: Vec<_> = asts
        .iter()
        .map(|ast| crate::analysis::collect(&ast.body, &[]))
        .collect::<Result<_, _>>()?;
    loop {
        let before: usize = globals.iter().map(Vec::len).sum();
        let imported: Vec<_> = globals.iter().flatten().cloned().collect();
        for (ast, locals) in asts.iter().zip(&mut globals) {
            let _ = crate::inference::infer(&ast.body, locals, &imported, types, options);
        }
        if before == globals.iter().map(Vec::len).sum() {
            break;
        }
    }
    Ok(globals)
}
