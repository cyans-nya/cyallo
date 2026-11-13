//! Persistent submissions; old statements are never replayed.
use crate::runtime::ResourceLimit;
use crate::{CompileOptions, Diagnostic, Dialect, Session, Status, compiler::compile_source};
use std::sync::Arc;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {
    Incomplete,
    Ready,
}
pub struct Repl {
    inspection: Option<Arc<crate::inspection::Inspection>>,
    options: CompileOptions,
    initial_context: crate::Context,
    source: String,
    committed: usize,
    error_source: Option<Box<str>>,
    session: Session,
}
impl Repl {
    pub fn new(options: CompileOptions) -> Result<Self, Diagnostic> {
        Self::with_context(options, crate::Context::default())
    }
    pub fn with_context(
        options: CompileOptions,
        context: crate::Context,
    ) -> Result<Self, Diagnostic> {
        let program = Arc::new(compile_source("", options, true)?);
        let mut session = Session::with_context(program, context.clone())?;
        session.run(1);
        Ok(Self {
            inspection: None,
            options,
            initial_context: context,
            source: String::new(),
            committed: 0,
            error_source: None,
            session,
        })
    }
    pub fn simplified() -> Result<Self, Diagnostic> {
        Self::new(CompileOptions {
            dialect: Dialect::Simplified,
            ..Default::default()
        })
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
    pub fn source(&self) -> &str {
        self.error_source.as_deref().unwrap_or(&self.source)
    }
    pub fn is_pending(&self) -> bool {
        self.source.len() > self.committed
    }
    pub fn reset(&mut self) -> Result<(), Diagnostic> {
        let inspection = self.inspection.clone();
        *self = Self::with_context(self.options, self.initial_context.clone())?;
        self.inspection = inspection;
        Ok(())
    }
    pub fn set_inspection(&mut self, inspection: Option<Arc<crate::inspection::Inspection>>) {
        self.inspection = inspection;
    }
    /// Buffer an incomplete block, or check a complete entry and prepare it to run.
    /// Static errors leave existing state untouched; runtime errors retain writes already made.
    pub fn push_line(&mut self, line: &str) -> Result<Submission, Diagnostic> {
        if matches!(
            self.session.status(),
            Status::Paused | Status::WaitingForInput
        ) {
            return Err(Diagnostic::new(
                "E202",
                "finish or interrupt the current entry before submitting more code",
                self.session.current_span().unwrap_or_default(),
            ));
        }
        self.error_source = None;
        self.source.push_str(line);
        if !line.ends_with('\n') {
            self.source.push('\n');
        }
        let program = match compile_source(&self.source, self.options, true) {
            Ok(program) => Arc::new(if let Some(observer) = &self.inspection {
                program.with_inspection(Arc::new(observer.window()))
            } else {
                program
            }),
            Err(d) if d.code == "E002" && d.span.start as usize == self.source.len() => {
                return Ok(Submission::Incomplete);
            }
            Err(d) => {
                self.reject();
                return Err(d);
            }
        };
        if let Err(d) = self.session.continue_with(program, self.committed as u32) {
            self.reject();
            return Err(d);
        }
        self.committed = self.source.len();
        Ok(Submission::Ready)
    }
    fn reject(&mut self) {
        self.error_source = Some(self.source.clone().into_boxed_str());
        self.source.truncate(self.committed);
    }
    pub fn run(&mut self, budget: usize) -> &Status {
        self.session.run(budget)
    }
    /// Abort the current computation while preserving prior writes, then permit another entry.
    pub fn interrupt(&mut self) {
        let span = self.session.current_span().unwrap_or_default();
        self.session.fail(ResourceLimit::Instructions, span);
    }
    pub fn discard_pending(&mut self) {
        self.source.truncate(self.committed);
        self.error_source = None;
    }
}
