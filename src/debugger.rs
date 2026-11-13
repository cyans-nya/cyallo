//! Rendering-independent source debugger with bounded execution.
use crate::{Session, SourceFile, Status};
use std::collections::BTreeSet;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Paused,
    Continue,
    Into,
    Over(usize),
    Out(usize),
}
pub struct Debugger {
    pub session: Session,
    pub breakpoints: BTreeSet<usize>,
    pub mode: Mode,
    source: SourceFile,
    started: bool,
}
impl Debugger {
    pub fn new(session: Session) -> Self {
        let source = SourceFile::new(session.program().source().into()).expect("validated source");
        Self {
            session,
            breakpoints: BTreeSet::new(),
            mode: Mode::Paused,
            source,
            started: false,
        }
    }
    pub fn line(&self) -> Option<usize> {
        self.session
            .current_span()
            .and_then(|s| self.source.location(s.start).map(|p| p.0))
    }
    pub fn command(&mut self, mode: Mode) {
        self.mode = mode;
        self.started = false;
    }
    pub fn restart(&mut self) -> Result<(), crate::Diagnostic> {
        self.session.restart()?;
        self.mode = Mode::Paused;
        self.started = false;
        Ok(())
    }
    pub fn tick(&mut self, budget: usize) {
        if self.mode == Mode::Paused {
            return;
        }
        for _ in 0..budget {
            if self.session.at_statement() && self.started {
                let depth = self.session.depth();
                let stop = self.line().is_some_and(|l| self.breakpoints.contains(&l))
                    || match self.mode {
                        Mode::Into => true,
                        Mode::Over(d) => depth <= d,
                        Mode::Out(d) => depth < d,
                        _ => false,
                    };
                if stop {
                    self.mode = Mode::Paused;
                    return;
                }
            }
            self.started = true;
            if !self.session.step() {
                if !matches!(self.session.status(), Status::Paused) {
                    self.mode = Mode::Paused;
                }
                return;
            }
        }
    }
}
