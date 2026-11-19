//! Report data model. The human and JSON renderers both read this structure,
//! so the two formats cannot disagree about what was measured.
use super::Timings;
use crate::{Compiled, Engine};
use cyallo::{
    Session, Status,
    backend::{CraneliftConfig, Tier, TierEvent, TierEventKind, TierPolicy},
    ir::Program,
    metrics::{ExecutionEngine, ExecutionEvent},
};
use std::time::Duration;

/// Presentation hint. Plain text never depends on it; the human renderer maps
/// tones to ANSI color only when the look enables color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Plain,
    Strong,
    Dim,
    Good,
    Warn,
    Bad,
    Accent,
    Osr,
    Speed,
    Title,
}

#[derive(Clone, Debug)]
pub(crate) enum Value {
    Int(u64),
    Ms(f64),
    Pct(f64),
    Text(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug)]
pub(crate) struct Cell {
    pub value: Value,
    pub tone: Tone,
}

impl Cell {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            value: Value::Text(text.into()),
            tone: Tone::Plain,
        }
    }
    pub(crate) fn int(count: u64) -> Self {
        Self {
            value: Value::Int(count),
            tone: Tone::Plain,
        }
    }
    pub(crate) fn ms(duration: Duration) -> Self {
        Self {
            value: Value::Ms(duration.as_secs_f64() * 1000.0),
            tone: Tone::Plain,
        }
    }
    pub(crate) fn pct(percent: f64) -> Self {
        Self {
            value: Value::Pct(percent),
            tone: Tone::Plain,
        }
    }
    pub(crate) fn toned(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Align {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub(crate) struct Column {
    pub head: &'static str,
    pub align: Align,
    /// Draw a share bar before a percentage cell.
    pub bar: bool,
}

pub(crate) struct Table {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
}

pub(crate) enum Block {
    Fields(Vec<(&'static str, Cell)>),
    Table(Table),
    Note(String),
}

pub(crate) struct Section {
    pub title: &'static str,
    pub blocks: Vec<Block>,
}

pub(crate) struct Document {
    pub title: String,
    pub sections: Vec<Section>,
    pub trace: Option<TraceDoc>,
}

fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Jit => "jit",
        Engine::Interpreter => "interpreter",
        Engine::Verified => "verified",
        Engine::Cranelift => "cranelift",
        Engine::CraneliftSpeed => "cranelift_speed",
    }
}

pub(crate) fn tier_engine(tier: Tier) -> ExecutionEngine {
    match tier {
        Tier::Winch => ExecutionEngine::Winch,
        Tier::CraneliftFast => ExecutionEngine::Cranelift,
        Tier::CraneliftSpeed => ExecutionEngine::CraneliftSpeed,
    }
}

fn tier_name(tier: Tier) -> &'static str {
    tier_engine(tier).name()
}

pub(crate) fn engine_tone(engine: ExecutionEngine) -> Tone {
    match engine {
        ExecutionEngine::Interpreter => Tone::Dim,
        ExecutionEngine::Winch => Tone::Warn,
        ExecutionEngine::Cranelift => Tone::Accent,
        ExecutionEngine::CraneliftSpeed => Tone::Speed,
    }
}

fn tier_for(engine: ExecutionEngine) -> Option<Tier> {
    match engine {
        ExecutionEngine::Interpreter => None,
        ExecutionEngine::Winch => Some(Tier::Winch),
        ExecutionEngine::Cranelift => Some(Tier::CraneliftFast),
        ExecutionEngine::CraneliftSpeed => Some(Tier::CraneliftSpeed),
    }
}

/// One routine's logical work for the Routines table and the trace.
pub(crate) struct RoutineRow {
    pub index: usize,
    pub name: String,
    pub calls: u64,
    pub work: u64,
    pub osr: Option<u64>,
    pub exec: Option<Duration>,
    /// Instructions per engine, in `ExecutionEngine::ALL` order. Only the full
    /// trace observes this; summaries leave it unknown.
    pub engines: Option<[u64; 4]>,
}

/// A timeline entry, from either the compiler or the execution observer.
/// `kind` and `attrs` are the structured form used by the JSON trace; `label`
/// and `detail` are the human form.
#[derive(Clone)]
pub(crate) struct Row {
    pub at: Duration,
    pub function: usize,
    pub routine: String,
    pub kind: &'static str,
    pub label: &'static str,
    pub tone: Tone,
    pub detail: String,
    pub attrs: Vec<(&'static str, Value)>,
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn compile_row(program: &Program, event: &TierEvent) -> Row {
    let function = event.function;
    let routine = program.functions()[function].name.to_string();
    let (kind, label, tone, detail, attrs) = match &event.kind {
        TierEventKind::Requested { tier, executed } => {
            let trigger = executed.map_or("manual request".to_owned(), |count| {
                format!("after {count} instructions")
            });
            let size = program.functions()[function].code.len();
            (
                "request",
                "request",
                Tone::Accent,
                format!("{} {trigger}; {size} bytecodes", tier_name(*tier)),
                vec![
                    ("tier", text(tier_name(*tier))),
                    ("executed", executed.map_or(Value::Null, Value::Int)),
                    ("size", Value::Int(size as u64)),
                ],
            )
        }
        TierEventKind::Superseded { previous, tier } => (
            "supersede",
            "supersede",
            Tone::Warn,
            format!("{} -> {}", tier_name(*previous), tier_name(*tier)),
            vec![
                ("from", text(tier_name(*previous))),
                ("to", text(tier_name(*tier))),
            ],
        ),
        TierEventKind::CompileStarted {
            tier,
            worker,
            queue_time,
        } => (
            "compile_start",
            "compile",
            Tone::Dim,
            format!(
                "{} on {worker:?}; queued {:.3} ms",
                tier_name(*tier),
                millis(*queue_time)
            ),
            vec![
                ("tier", text(tier_name(*tier))),
                ("worker", Value::Text(format!("{worker:?}"))),
                ("queued_ms", Value::Ms(millis(*queue_time))),
            ],
        ),
        TierEventKind::CompileFinished {
            tier,
            duration,
            fallback: None,
        } => (
            "compile_end",
            "compiled",
            Tone::Good,
            format!("{} in {:.3} ms", tier_name(*tier), millis(*duration)),
            vec![
                ("tier", text(tier_name(*tier))),
                ("duration_ms", Value::Ms(millis(*duration))),
                ("fallback", Value::Null),
            ],
        ),
        TierEventKind::CompileFinished {
            tier,
            duration,
            fallback: Some(reason),
        } => (
            "compile_end",
            "compiled",
            Tone::Warn,
            format!(
                "{} in {:.3} ms; fallback: {reason}",
                tier_name(*tier),
                millis(*duration)
            ),
            vec![
                ("tier", text(tier_name(*tier))),
                ("duration_ms", Value::Ms(millis(*duration))),
                ("fallback", Value::Text(reason.to_string())),
            ],
        ),
        TierEventKind::Installed { previous, tier } => (
            "install",
            "install",
            Tone::Good,
            format!(
                "{} -> {}",
                previous.map_or("uncompiled", tier_name),
                tier_name(*tier)
            ),
            vec![
                ("from", previous.map_or(Value::Null, |p| text(tier_name(p)))),
                ("tier", text(tier_name(*tier))),
            ],
        ),
        TierEventKind::Failed { error } => (
            "failed",
            "failed",
            Tone::Bad,
            error.to_string(),
            vec![("error", Value::Text(error.to_string()))],
        ),
        TierEventKind::FirstCall { waited, compiled } => (
            "admit",
            "admit",
            Tone::Accent,
            format!(
                "first call waited {:.3} ms; compiled={compiled}",
                millis(*waited)
            ),
            vec![
                ("waited_ms", Value::Ms(millis(*waited))),
                ("compiled", Value::Bool(*compiled)),
            ],
        ),
        TierEventKind::Discarded { tier, installed } => (
            "discard",
            "discard",
            Tone::Warn,
            format!(
                "{} ({} already installed)",
                tier_name(*tier),
                tier_name(*installed)
            ),
            vec![
                ("tier", text(tier_name(*tier))),
                ("installed", text(tier_name(*installed))),
            ],
        ),
    };
    Row {
        at: event.elapsed,
        function,
        routine,
        kind,
        label,
        tone,
        detail,
        attrs,
    }
}

fn execution_row(program: &Program, compiles: &[TierEvent], event: &ExecutionEvent) -> Row {
    let function = event.function;
    let routine = program.functions()[function].name.to_string();
    let from = event.from.map_or("entry", ExecutionEngine::name);
    let (kind, label, tone) = if event.osr {
        ("osr", "OSR", Tone::Osr)
    } else if event.from.is_some() {
        ("handoff", "handoff", Tone::Accent)
    } else {
        ("enter", "enter", Tone::Dim)
    };
    let mut detail = format!(
        "{from} -> {}; invocation {} pc {} depth {} version {}",
        event.to.name(),
        event.invocation,
        event.pc,
        event.depth,
        event.code_version
    );
    let mut attrs = vec![
        ("from", event.from.map_or(Value::Null, |e| text(e.name()))),
        ("to", text(event.to.name())),
        ("invocation", Value::Int(event.invocation)),
        ("pc", Value::Int(event.pc as u64)),
        ("depth", Value::Int(event.depth as u64)),
        ("version", Value::Int(event.code_version)),
    ];
    if event.to == ExecutionEngine::CraneliftSpeed
        && let Some(entry) = program.functions()[function]
            .resume
            .entries
            .iter()
            .find(|entry| entry.pc == event.pc)
    {
        detail.push_str(&format!("; live slots {}", entry.values.len()));
        attrs.push(("live_slots", Value::Int(entry.values.len() as u64)));
    }
    if event.osr
        && let Some(target) = tier_for(event.to)
        && let Some(ready) = compiles
            .iter()
            .filter(|e| {
                e.function == function
                    && e.elapsed <= event.elapsed
                    && matches!(e.kind, TierEventKind::CompileFinished { tier, .. } if tier == target)
            })
            .map(|e| e.elapsed)
            .max()
    {
        let waited = event.elapsed - ready;
        detail.push_str(&format!("; ready-to-entry {:.3} ms", millis(waited)));
        attrs.push(("ready_ms", Value::Ms(millis(waited))));
    }
    Row {
        at: event.elapsed,
        function,
        routine,
        kind,
        label,
        tone,
        detail,
        attrs,
    }
}

/// A compile-time inline decision, placed on the routine's installed code.
pub(crate) struct InlineDoc {
    pub routine: usize,
    pub routine_name: String,
    pub pc: usize,
    pub callee: usize,
    pub callee_name: String,
    pub decision: &'static str,
    pub cost: usize,
    pub depth: usize,
}

/// Structured trace: the same facts as the human report, typed for tools.
pub(crate) struct TraceDoc {
    pub mode: &'static str,
    pub engine: &'static str,
    pub status: &'static str,
    pub wall: Duration,
    pub routines: Vec<RoutineRow>,
    pub events: Vec<Row>,
    pub inlines: Vec<InlineDoc>,
    pub dropped_events: u64,
}

fn eager_text(policy: TierPolicy) -> String {
    let eager = policy.eager;
    format!(
        "{} · words [{}, {}) · {} outstanding jobs · {} speculative words · budget {} · first-call wait {} · hot /2, cold x4",
        if eager.enabled { "on" } else { "off" },
        eager.min_words,
        eager.max_words,
        eager.max_routines,
        eager.max_total_words,
        ms_text(eager.predicted_budget),
        ms_text(policy.first_call_wait),
    )
}

fn ms_text(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1000.0)
}

fn fields(rows: Vec<(&'static str, Cell)>) -> Block {
    Block::Fields(rows)
}

fn note(text: impl Into<String>) -> Block {
    Block::Note(text.into())
}

/// Maximum compiler rows shown in a summary; the trace shows every event.
const SUMMARY_TIMELINE_ROWS: usize = 64;

/// Build the report document for one execution. A session with a full
/// execution report renders the trace; otherwise only logical counters and
/// compiler events are available.
pub(crate) fn build(
    artifact: &Option<Compiled>,
    program: &Program,
    session: &Session,
    engine: Engine,
    timings: &Timings,
    inlining: bool,
) -> Document {
    let full = session.execution_report();
    let status = match session.status() {
        Status::Finished => "Finished",
        Status::Paused => "Paused",
        Status::WaitingForInput => "WaitingForInput",
        Status::Waiting => "Waiting",
        Status::Failed(_) => "Failed",
    };
    let gc = session.gc_stats();
    let gc_during = match full {
        Some(report) => gc.pause_time.saturating_sub(report.gc_pause_at_start),
        None => gc.pause_time.saturating_sub(timings.counters.gc_pause),
    };
    let execution = match full {
        Some(report) => timings
            .execution
            .saturating_sub(report.first_call_wait)
            .saturating_sub(report.body_analysis_time)
            .saturating_sub(gc_during),
        None => timings.execution,
    };
    let instructions = match full {
        Some(report) => report.instruction_count(),
        None => session
            .instruction_count()
            .saturating_sub(timings.counters.instructions),
    };
    let mut sections = Vec::new();

    // Overview
    let mode = if full.is_some() {
        "trace (full observation; timings include observer overhead)"
    } else {
        "summary (counters only; optimizations unchanged)"
    };
    let mut overview = vec![
        ("mode", Cell::text(mode)),
        (
            "inlining",
            Cell::text(if inlining {
                "on"
            } else {
                "off (--jit-no-inline)"
            }),
        ),
    ];
    if let Some(frontend) = timings.frontend {
        overview.push(("frontend/read", Cell::ms(frontend)));
    }
    sections.push(Section {
        title: "Overview",
        blocks: vec![fields(overview)],
    });

    // Timing
    let timing = vec![
        ("backend setup", Cell::ms(timings.backend)),
        ("session setup", Cell::ms(timings.session)),
        ("VM execution", Cell::ms(execution).toned(Tone::Strong)),
        ("input wait", Cell::ms(timings.input_wait)),
        ("run wall", Cell::ms(timings.wall).toned(Tone::Strong)),
    ];
    sections.push(Section {
        title: "Timing",
        blocks: vec![
            fields(timing),
            note(if full.is_some() {
                "VM execution excludes first-call waits, body checks and GC; run wall includes setup and console I/O"
            } else {
                "VM execution includes deferred checks, compiler waits and GC; run wall excludes frontend, startup and report rendering"
            }),
        ],
    });

    // Work
    let inlined = session
        .inlined_call_count()
        .saturating_sub(timings.counters.inlined);
    let embedded = session
        .body_inlined_call_count()
        .saturating_sub(timings.counters.procedures);
    let mut work = vec![
        ("bytecode instructions", Cell::int(instructions)),
        ("frameless calls", Cell::int(inlined)),
        ("embedded calls", Cell::int(embedded)),
    ];
    if full.is_some() && execution.as_secs_f64() > 0.0 {
        let rate = instructions as f64 / execution.as_secs_f64() / 1_000_000.0;
        work.insert(1, ("throughput", Cell::text(format!("{rate:.2} M/s"))));
    }
    if session.gc_config().enabled {
        work.push((
            "GC",
            Cell::text(format!(
                "{} collections · {} allocated · {} reclaimed · {} moved · {}/{} live/capacity cells · pauses {} ({} during execution)",
                gc.collections,
                gc.allocated_cells,
                gc.reclaimed_cells,
                gc.moved_cells,
                gc.occupied_cells,
                gc.arena_capacity,
                ms_text(gc.pause_time),
                ms_text(gc_during),
            )),
        ));
    }
    let services = session.service_stats();
    if services.provisions != 0 {
        work.push((
            "services",
            Cell::text(format!(
                "slot-hits {} · resolutions {} · target-hits {} · native-target-hits {} · target-misses {} · provisions {} · withdrawals {} · released {}",
                services.slot_hits,
                services.resolutions,
                services.target_hits,
                services.native_target_hits,
                services.target_misses,
                services.provisions,
                services.withdrawals,
                services.released,
            )),
        ));
    }
    sections.push(Section {
        title: "Work",
        blocks: vec![fields(work)],
    });

    // Compiler and timeline sources
    let mut compile_events: Vec<TierEvent> = Vec::new();
    let mut compiler_fields: Vec<(&'static str, Cell)> = Vec::new();
    let mut compiler_notes = Vec::new();
    if let Some(Compiled::Tiered(tiered)) = artifact {
        let policy = tiered.policy();
        compiler_fields.push(("workers", Cell::int(tiered.compiler_workers() as u64)));
        compiler_fields.push(("eager", Cell::text(eager_text(policy))));
        compiler_fields.push((
            "tiers",
            Cell::text(format!(
                "interp -> winch at {} calls or {} instructions; fast >= {}; speed >= {} for loop-only routines <= {} bytecodes",
                policy.winch_calls,
                policy.winch_instructions,
                policy.warm_instructions,
                policy.hot_instructions,
                policy.speed_size_limit,
            )),
        ));
        compiler_fields.push((
            "notifications",
            Cell::text(format!(
                "every {} instructions; promotions at native safepoints",
                policy.execution_quantum
            )),
        ));
        if full.is_some() {
            compiler_fields.push((
                "fast config",
                Cell::text(format!("{:?}", CraneliftConfig::tiered(false))),
            ));
            compiler_fields.push((
                "speed config",
                Cell::text(format!("{:?}", CraneliftConfig::tiered(true))),
            ));
        }
        let (tiers, events) = tiered.reporting_snapshot();
        let completed: Duration = events
            .iter()
            .filter_map(|event| match event.kind {
                TierEventKind::CompileFinished { duration, .. } => Some(duration),
                _ => None,
            })
            .sum();
        compiler_fields.push(("completed compile work", Cell::ms(completed)));
        compiler_notes.push("compile work overlaps execution; it is not additive".to_owned());
        for info in tiers.iter().filter(|info| info.compiling) {
            compiler_notes.push(format!(
                "{} compiling {}; the final report does not wait",
                program.functions()[info.function].name,
                info.requested.map_or("compilation", tier_name)
            ));
        }
        compile_events = events;
    } else {
        let direct = match engine {
            Engine::Cranelift => Some(CraneliftConfig::fast()),
            Engine::CraneliftSpeed => Some(CraneliftConfig::speed()),
            _ => None,
        };
        if let Some(config) = direct {
            compiler_fields.push(("native config", Cell::text(format!("{config:?}"))));
        }
    }
    if !compiler_fields.is_empty() {
        let mut blocks = vec![fields(compiler_fields)];
        for text in compiler_notes {
            blocks.push(note(text));
        }
        sections.push(Section {
            title: "Compiler",
            blocks,
        });
    }

    // Timeline
    let mut rows: Vec<Row> = compile_events
        .iter()
        .map(|event| compile_row(program, event))
        .collect();
    let mut dropped = 0;
    if let Some(report) = full {
        rows.extend(
            report
                .events
                .iter()
                .map(|event| execution_row(program, &compile_events, event)),
        );
        dropped = report.dropped_events;
    }
    rows.sort_by_key(|row| row.at);
    let all_rows = rows.clone();
    let total_rows = rows.len();
    if full.is_none() && total_rows > SUMMARY_TIMELINE_ROWS {
        rows.truncate(SUMMARY_TIMELINE_ROWS);
    }
    if !rows.is_empty() {
        let table = Table {
            columns: vec![
                Column {
                    head: "at",
                    align: Align::Right,
                    bar: false,
                },
                Column {
                    head: "routine",
                    align: Align::Left,
                    bar: false,
                },
                Column {
                    head: "event",
                    align: Align::Left,
                    bar: false,
                },
                Column {
                    head: "detail",
                    align: Align::Left,
                    bar: false,
                },
            ],
            rows: rows
                .iter()
                .map(|row| {
                    vec![
                        Cell::ms(row.at),
                        Cell::text(row.routine.clone()),
                        Cell::text(row.label).toned(row.tone),
                        Cell::text(row.detail.clone()),
                    ]
                })
                .collect(),
        };
        let mut blocks = vec![
            note("times are relative to backend setup"),
            Block::Table(table),
        ];
        if full.is_none() && total_rows > SUMMARY_TIMELINE_ROWS {
            blocks.push(note(format!(
                "{} compiler events omitted; use --jit-trace for the full timeline",
                total_rows - SUMMARY_TIMELINE_ROWS
            )));
        }
        if dropped != 0 {
            blocks.push(note(format!(
                "{dropped} further execution events omitted; counts remain complete"
            )));
        }
        sections.push(Section {
            title: "Timeline",
            blocks,
        });
    }

    // Routines
    let routines: Vec<RoutineRow> = match full {
        Some(report) => report
            .functions
            .iter()
            .enumerate()
            .filter_map(|(fi, metrics)| {
                let work: u64 = metrics.instructions.iter().sum();
                (work != 0 || metrics.invocations != 0).then(|| RoutineRow {
                    index: fi,
                    name: program.functions()[fi].name.to_string(),
                    calls: metrics.invocations,
                    work,
                    osr: Some(metrics.osr_transitions),
                    exec: Some(metrics.execution_time.iter().copied().sum()),
                    engines: Some(metrics.instructions),
                })
            })
            .collect(),
        None => program
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
                (work != 0 || calls != 0).then(|| RoutineRow {
                    index: fi,
                    name: function.name.to_string(),
                    engines: None,
                    calls: if fi == 0 && work != 0 {
                        calls.max(1)
                    } else {
                        calls
                    },
                    work,
                    osr: None,
                    exec: None,
                })
            })
            .collect(),
    };
    let routines_for_trace: Vec<RoutineRow> = routines
        .iter()
        .map(|row| RoutineRow {
            index: row.index,
            name: row.name.clone(),
            calls: row.calls,
            work: row.work,
            osr: row.osr,
            exec: row.exec,
            engines: row.engines,
        })
        .collect();
    let mut routine_columns = vec![
        Column {
            head: "routine",
            align: Align::Left,
            bar: false,
        },
        Column {
            head: "calls",
            align: Align::Right,
            bar: false,
        },
        Column {
            head: "instructions",
            align: Align::Right,
            bar: false,
        },
        Column {
            head: "share",
            align: Align::Right,
            bar: true,
        },
    ];
    if full.is_some() {
        routine_columns.push(Column {
            head: "OSR",
            align: Align::Right,
            bar: false,
        });
        routine_columns.push(Column {
            head: "exec",
            align: Align::Right,
            bar: false,
        });
    }
    let routine_rows = routines
        .iter()
        .map(|row| {
            let mut cells = vec![
                Cell::text(row.name.clone()),
                Cell::int(row.calls),
                Cell::int(row.work),
                Cell::pct(percent(row.work, instructions)),
            ];
            if full.is_some() {
                cells.push(Cell::int(row.osr.unwrap_or(0)));
                cells.push(row.exec.map_or(Cell::text("-"), Cell::ms));
            }
            cells
        })
        .collect();
    sections.push(Section {
        title: "Routines",
        blocks: vec![Block::Table(Table {
            columns: routine_columns,
            rows: routine_rows,
        })],
    });

    if let Some(report) = full {
        // Executed by tier
        let tier_rows = ExecutionEngine::ALL
            .iter()
            .map(|which| {
                let index = *which as usize;
                let count: u64 = report.functions.iter().map(|f| f.instructions[index]).sum();
                let elapsed: Duration = report
                    .functions
                    .iter()
                    .map(|f| f.execution_time[index])
                    .sum();
                vec![
                    Cell::text(which.name()).toned(engine_tone(*which)),
                    Cell::int(count),
                    Cell::pct(percent(count, instructions)),
                    Cell::ms(elapsed),
                ]
            })
            .collect();
        sections.push(Section {
            title: "Tiers",
            blocks: vec![
                note("helper work is charged to its calling tier"),
                Block::Table(Table {
                    columns: vec![
                        Column {
                            head: "tier",
                            align: Align::Left,
                            bar: false,
                        },
                        Column {
                            head: "instructions",
                            align: Align::Right,
                            bar: false,
                        },
                        Column {
                            head: "share",
                            align: Align::Right,
                            bar: true,
                        },
                        Column {
                            head: "exec",
                            align: Align::Right,
                            bar: false,
                        },
                    ],
                    rows: tier_rows,
                }),
            ],
        });

        // Runtime
        let b = &report.boundaries;
        let osr: u64 = report.functions.iter().map(|f| f.osr_transitions).sum();
        sections.push(Section {
            title: "Runtime",
            blocks: vec![fields(vec![
                (
                    "native yields",
                    Cell::text(format!(
                        "return {} · budget {} · notification {} · OSR {} · hot {} · error {} · continuation {}",
                        report.native_exits[0],
                        report.native_exits[1],
                        report.native_exits[2],
                        report.native_exits[3],
                        report.native_exits[4],
                        report.native_exits[5],
                        report.native_exits[6],
                    )),
                ),
                (
                    "runtime boundaries",
                    Cell::text(format!(
                        "step {} · flush {} (work {}, skipped {}) · state {} · get {} · put {}",
                        b.step,
                        b.flush,
                        b.flush - b.flush_skipped,
                        b.flush_skipped,
                        b.state,
                        b.get,
                        b.put,
                    )),
                ),
                ("native prepare", Cell::ms(report.native_prepare_time)),
                ("native materialize", Cell::ms(report.native_materialize_time)),
                ("first-call admissions", Cell::int(report.first_call_admissions)),
                ("first-call wait", Cell::ms(report.first_call_wait)),
                ("deferred body checks", Cell::int(report.deferred_bodies)),
                ("deferred body time", Cell::ms(report.body_analysis_time)),
                ("OSR transitions", Cell::int(osr)),
                ("peak frames", Cell::int(report.peak_frames as u64)),
                ("peak cells", Cell::int(report.peak_cells as u64)),
            ])],
        });
    }

    // Notes
    let notes = if full.is_some() {
        "observation is opt-in; its timings include reporting overhead; first-call waits are reported separately"
    } else {
        "use --jit-trace for per-invocation and tier timings; it observes every entry"
    };
    sections.push(Section {
        title: "Notes",
        blocks: vec![note(notes)],
    });

    let inlines = program
        .inspection()
        .map(|observer| observer.inline_sites())
        .unwrap_or_default()
        .into_iter()
        .map(|site| InlineDoc {
            routine: site.routine,
            routine_name: program.functions()[site.routine].name.to_string(),
            pc: site.pc,
            callee: site.callee,
            callee_name: program.functions()[site.callee].name.to_string(),
            decision: site.decision.name(),
            cost: site.cost,
            depth: site.depth,
        })
        .collect();
    let trace = TraceDoc {
        mode: if full.is_some() { "trace" } else { "summary" },
        engine: engine_name(engine),
        status,
        wall: timings.wall,
        routines: routines_for_trace,
        events: all_rows,
        inlines,
        dropped_events: dropped,
    };
    Document {
        title: format!("Execution report: {} ({status})", engine_name(engine)),
        sections,
        trace: Some(trace),
    }
}

pub(crate) fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}
