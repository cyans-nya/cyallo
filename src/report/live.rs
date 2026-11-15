//! Live status block for `--jit-trace`. It is redrawn in place on stderr at
//! execution safepoints and erased before any program output or notice, so
//! the block never mixes with stdout.
use super::{
    Look, Timings, human,
    model::{Align, Block, Cell, Column, Document, Section, Table, Tone, engine_tone, tier_engine},
};
use crate::Compiled;
use cyallo::{Session, backend::TierInfo, ir::Program};
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

/// Routines shown in the block. The rest are summarized in one line.
const LIVE_ROUTINES: usize = 8;
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// Owns the on-screen block. Callers erase it with [`Live::suspend`] before
/// writing anything else, and [`Live::refresh`] redraws it when due.
pub(crate) struct Live {
    enabled: bool,
    lines: usize,
    last: Option<Instant>,
}

impl Live {
    /// `enabled` must already reflect the terminal check made by the caller.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            lines: 0,
            last: None,
        }
    }

    /// Erase the block so the caller can write normal output.
    pub(crate) fn suspend(&mut self) {
        if self.lines == 0 {
            return;
        }
        let mut err = io::stderr().lock();
        // Move to the first line of the block and clear to the end of screen.
        let _ = write!(err, "\x1b[{}F\x1b[J", self.lines);
        let _ = err.flush();
        self.lines = 0;
    }

    /// Redraw when `force` is set or the redraw interval has passed.
    pub(crate) fn refresh(&mut self, force: bool, frame: impl FnOnce() -> String) {
        if !self.enabled {
            return;
        }
        let due = self.last.is_none_or(|at| at.elapsed() >= REDRAW_INTERVAL);
        if !force && !due {
            return;
        }
        self.suspend();
        let text = frame();
        let mut err = io::stderr().lock();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
        self.lines = text.lines().count();
        self.last = Some(Instant::now());
    }

    /// Erase the block for good; later refreshes do nothing.
    pub(crate) fn finish(&mut self) {
        self.suspend();
        self.enabled = false;
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.suspend();
    }
}

/// The live frame: elapsed time, instructions, compiler state and the busiest
/// routines with their current tier.
pub(crate) fn live_frame(
    artifact: &Option<Compiled>,
    program: &Program,
    session: &Session,
    origin: Instant,
    timings: &Timings,
    look: Look,
) -> String {
    let instructions = session
        .instruction_count()
        .saturating_sub(timings.counters.instructions);
    let tiers: Vec<TierInfo> = match artifact {
        Some(Compiled::Tiered(tiered)) => tiered.tiers(),
        _ => Vec::new(),
    };
    let compiling = tiers.iter().filter(|info| info.compiling).count();
    let installed = tiers.iter().filter(|info| info.tier.is_some()).count();

    let mut routines: Vec<(u64, String, u64, Cell)> = program
        .functions()
        .iter()
        .enumerate()
        .filter_map(|(fi, function)| {
            let (before_work, before_calls) = timings
                .counters
                .functions
                .get(fi)
                .copied()
                .unwrap_or_default();
            let work = session
                .function_instruction_count(fi)
                .saturating_sub(before_work);
            let calls = session.function_call_count(fi).saturating_sub(before_calls);
            if work == 0 && calls == 0 {
                return None;
            }
            let info = tiers.iter().find(|info| info.function == fi);
            Some((
                work,
                function.name.to_string(),
                if fi == 0 && work != 0 {
                    calls.max(1)
                } else {
                    calls
                },
                state_cell(info),
            ))
        })
        .collect();
    routines.sort_by_key(|row| std::cmp::Reverse(row.0));
    let hidden = routines.len().saturating_sub(LIVE_ROUTINES);

    let rows = routines
        .iter()
        .take(LIVE_ROUTINES)
        .map(|(work, name, calls, state)| {
            vec![
                Cell::text(name.clone()),
                state.clone(),
                Cell::int(*work),
                Cell::int(*calls),
            ]
        })
        .collect();
    let mut blocks = vec![Block::Fields(vec![
        ("elapsed", Cell::ms(origin.elapsed())),
        ("instructions", Cell::int(instructions)),
        (
            "compiler",
            Cell::text(match artifact {
                Some(Compiled::Tiered(_)) => {
                    format!("{installed} installed · {compiling} compiling")
                }
                _ => "direct engine".to_owned(),
            }),
        ),
    ])];
    blocks.push(Block::Table(Table {
        columns: vec![
            Column {
                head: "routine",
                align: Align::Left,
                bar: false,
            },
            Column {
                head: "state",
                align: Align::Left,
                bar: false,
            },
            Column {
                head: "instructions",
                align: Align::Right,
                bar: false,
            },
            Column {
                head: "calls",
                align: Align::Right,
                bar: false,
            },
        ],
        rows,
    }));
    if hidden > 0 {
        blocks.push(Block::Note(format!("… {hidden} more routines")));
    }
    let document = Document {
        title: "JIT live".to_owned(),
        trace: None,
        sections: vec![Section { title: "", blocks }],
    };
    human::render(&document, look)
}

/// What a routine is running right now: its installed tier, or the tier it
/// is compiling toward.
fn state_cell(info: Option<&TierInfo>) -> Cell {
    let Some(info) = info else {
        return Cell::text("interpreted").toned(Tone::Dim);
    };
    let installed = info.tier.map(|tier| tier_engine(tier).name());
    if info.compiling {
        let from = installed.unwrap_or("interpreted");
        let to = info
            .requested
            .map_or("compile", |tier| tier_engine(tier).name());
        return Cell::text(format!("{from} -> {to}…")).toned(Tone::Accent);
    }
    match info.tier {
        Some(tier) => Cell::text(tier_engine(tier).name()).toned(engine_tone(tier_engine(tier))),
        None => Cell::text("interpreted").toned(Tone::Dim),
    }
}
