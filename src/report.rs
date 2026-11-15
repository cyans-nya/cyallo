//! CLI rendering lives outside the execution path.
//!
//! A report is built once as a [`model::Document`] and then rendered as
//! human text (plain, color or boxed) or as JSON. The live status block
//! reuses the same document type.
mod human;
mod json;
mod live;
mod model;

pub(crate) use live::{Live, live_frame};

use crate::{Compiled, Engine};
use cyallo::{Session, ir::Program};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct Timings {
    pub frontend: Option<Duration>,
    pub backend: Duration,
    pub session: Duration,
    pub execution: Duration,
    pub input_wait: Duration,
    pub wall: Duration,
    pub counters: Counters,
}
#[derive(Default)]
pub(crate) struct Counters {
    instructions: u64,
    functions: Vec<(u64, u64)>,
    inlined: u64,
    procedures: u64,
    gc_pause: Duration,
}
impl Counters {
    pub(crate) fn capture(session: &Session) -> Self {
        Self {
            instructions: session.instruction_count(),
            functions: (0..session.program().functions().len())
                .map(|fi| {
                    (
                        session.function_instruction_count(fi),
                        session.function_call_count(fi),
                    )
                })
                .collect(),
            inlined: session.inlined_call_count(),
            procedures: session.body_inlined_call_count(),
            gc_pause: session.gc_stats().pause_time,
        }
    }
}

/// Final report format. JSON is written to stderr with the same content.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Human,
    Json,
}

/// Human presentation. Plain is ASCII with no color; color adds ANSI tones;
/// boxed adds frames around sections and tables.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Look {
    pub color: bool,
    pub boxed: bool,
    pub ascii: bool,
}
impl Look {
    pub(crate) fn plain() -> Self {
        Self {
            color: false,
            boxed: false,
            ascii: true,
        }
    }
    pub(crate) fn color(enabled: bool) -> Self {
        Self {
            color: enabled,
            boxed: false,
            ascii: false,
        }
    }
    pub(crate) fn boxed(enabled: bool) -> Self {
        Self {
            color: enabled,
            boxed: true,
            ascii: false,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Output {
    pub format: Format,
    pub look: Look,
    /// Whether routine inlining is enabled for this run.
    pub inlining: bool,
}

/// Render the final report for one execution.
pub(crate) fn render(
    artifact: &Option<Compiled>,
    program: &Program,
    session: &Session,
    engine: Engine,
    timings: &Timings,
    output: Output,
) -> String {
    let document = model::build(artifact, program, session, engine, timings, output.inlining);
    let inspection = program.inspection().map(|observer| observer.render());
    match output.format {
        Format::Human => {
            let mut text = human::render(&document, output.look);
            if let Some(extra) = inspection {
                text.push_str(&extra);
            }
            text
        }
        Format::Json => json::render(&document, inspection.as_deref()),
    }
}
