use cyallo_ast::Span;
pub use cyallo_ir::ir::*;
#[derive(Debug, Clone)]
pub struct Program {
    pub(crate) dynamic_plan: Option<std::sync::Arc<crate::dynamic_modules::CompilePlan>>,
    pub(crate) module_loader: Option<cyallo_modules::ModuleLoader>,
    pub(crate) parent: Option<std::sync::Arc<Program>>,
    pub(crate) records: std::sync::Arc<std::collections::HashMap<String, cyallo_ast::Type>>,
    pub(crate) requires_tracing: bool,
    pub(crate) jobs: crate::Jobs,
    pub(crate) inspection: Option<std::sync::Arc<crate::inspection::Inspection>>,
    pub(crate) snapshot_id: u64,
    pub(crate) source_modules: Box<[crate::modules::SourceRange]>,
    pub(crate) services: std::sync::Arc<crate::services::Metadata>,
    pub(crate) source: Box<str>,
    pub(crate) functions: Vec<Routine>,
    pub(crate) bytecode: crate::bytecode::Module,
    /// Identities of native code compiled for this program. Shared by clones so
    /// versions stay unique among every unit that can run against its layout.
    pub(crate) code_versions: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
impl Program {
    pub fn with_module_loader(mut self, loader: cyallo_modules::ModuleLoader) -> Self {
        self.module_loader = Some(loader);
        self
    }
    pub(crate) fn static_global_count(&self) -> usize {
        let mut base = self;
        while let Some(parent) = &base.parent {
            base = parent;
        }
        base.functions[0].locals.len()
    }
    pub(crate) fn extends(&self, ancestor: &std::sync::Arc<Program>) -> bool {
        let mut current = self.parent.as_ref();
        while let Some(parent) = current {
            if std::sync::Arc::ptr_eq(parent, ancestor) {
                return true;
            }
            current = parent.parent.as_ref();
        }
        false
    }
    /// A fresh native code version; zero is reserved for "no native frame".
    pub(crate) fn next_code_version(&self) -> u64 {
        self.code_versions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
    pub fn with_inspection(
        mut self,
        inspection: std::sync::Arc<crate::inspection::Inspection>,
    ) -> Self {
        self.snapshot_id = inspection.next_snapshot();
        self.inspection = Some(inspection);
        self
    }
    pub fn inspection(&self) -> Option<&std::sync::Arc<crate::inspection::Inspection>> {
        self.inspection.as_ref()
    }
    pub fn emit_inspection(&self) {
        if let Some(observer) = &self.inspection {
            observer.emit_program(self);
        }
    }
    pub(crate) fn inline_site(&self, site: crate::inspection::InlineSite) {
        if let Some(observer) = &self.inspection {
            observer.inline_site(site);
        }
    }
    pub(crate) fn notice(
        &self,
        category: &'static str,
        function: usize,
        message: impl Into<String>,
    ) {
        if let Some(observer) = &self.inspection {
            observer.notice(
                category,
                &format!(
                    "p{}:f{function} {}",
                    self.snapshot_id, self.functions[function].name
                ),
                message,
            );
        }
    }
    pub(crate) fn codegen_jobs(&self) -> crate::Jobs {
        crate::Jobs(self.jobs.workers().min(3))
    }
    pub fn source_modules(&self) -> &[crate::modules::SourceRange] {
        &self.source_modules
    }
    pub fn services(&self) -> &crate::services::Metadata {
        &self.services
    }
    pub fn bytecode(&self) -> &crate::bytecode::Module {
        &self.bytecode
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn functions(&self) -> &[Routine] {
        &self.functions
    }
    pub fn validate_all(&self) -> Result<(), crate::Diagnostic> {
        self.validate_all_with_jobs(self.jobs)
    }
    pub fn validate_all_with_jobs(
        &self,
        jobs: cyallo_compile_jobs::Jobs,
    ) -> Result<(), crate::Diagnostic> {
        let checked = jobs.map(&self.functions, 4, |_, _, r| {
            r.checked().map(|_| ()).map_err(Clone::clone)
        });
        for result in checked {
            result?;
        }
        Ok(())
    }
    pub fn compilation_units(&self) -> impl ExactSizeIterator<Item = CompilationUnit<'_>> {
        self.functions
            .iter()
            .zip(self.bytecode.functions.iter())
            .enumerate()
            .map(|(id, (routine, bytecode))| CompilationUnit {
                id,
                routine,
                bytecode,
            })
    }
    /// Prepare selected units without checking or translating unrelated cold bodies.
    pub fn prepare_units(
        &self,
        units: &[usize],
        jobs: cyallo_compile_jobs::Jobs,
    ) -> Result<(), crate::Diagnostic> {
        let prepared = jobs.map(units, 2, |_, _, &id| {
            let routine = self.functions.get(id).ok_or_else(|| {
                crate::Diagnostic::new("E153", "invalid compilation unit", Span::default())
            })?;
            routine.checked().map_err(Clone::clone)?;
            let _ = &*self.bytecode.functions[id];
            Ok(())
        });
        for result in prepared {
            result?;
        }
        Ok(())
    }
}

/// An independently checked and translated routine in an immutable linked program.
#[derive(Clone, Copy)]
pub struct CompilationUnit<'a> {
    pub id: usize,
    pub routine: &'a Routine,
    bytecode: &'a crate::bytecode::Routine,
}
impl<'a> CompilationUnit<'a> {
    pub fn checked_ir(self) -> Result<&'a Function, &'a crate::Diagnostic> {
        self.routine.checked()
    }
    pub fn bytecode(self) -> Result<&'a crate::bytecode::Function, &'a crate::Diagnostic> {
        self.routine.checked()?;
        Ok(self.bytecode)
    }
}
