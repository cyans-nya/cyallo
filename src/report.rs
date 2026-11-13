//! CLI rendering lives outside the execution path.
use crate::{Compiled, Engine};
use cyallo::{
    Session, Status,
    backend::{CraneliftConfig, Tier, TierEventKind},
    ir::Program,
    metrics::ExecutionEngine,
};
use std::{fmt::Write, time::Duration};

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
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
fn tier(tier: Tier) -> &'static str {
    match tier {
        Tier::Winch => "winch",
        Tier::CraneliftFast => "cranelift",
        Tier::CraneliftSpeed => "cranelift_speed",
    }
}
pub(crate) fn render(
    artifact: &Option<Compiled>,
    program: &Program,
    session: &Session,
    engine: Engine,
    timings: &Timings,
) -> String {
    let Some(report) = session.execution_report() else {
        return summary(artifact, program, session, engine, timings);
    };
    let mut text = String::new();
    let gc = session.gc_stats();
    let gc_pause = gc.pause_time.saturating_sub(report.gc_pause_at_start);
    let engine_name = match engine {
        Engine::Jit => "jit",
        Engine::Interpreter => "interpreter",
        Engine::Cranelift => "cranelift",
        Engine::CraneliftSpeed => "cranelift_speed",
    };
    let status = match session.status() {
        Status::Finished => "Finished",
        Status::Paused => "Paused",
        Status::WaitingForInput => "WaitingForInput",
        Status::Failed(_) => "Failed",
    };
    writeln!(text, "Execution report: {engine_name} ({status})").unwrap();
    writeln!(
        text,
        "  report mode: full trace; timings include observer overhead"
    )
    .unwrap();
    let config = match engine {
        Engine::Cranelift => Some(CraneliftConfig::fast()),
        Engine::CraneliftSpeed => Some(CraneliftConfig::speed()),
        _ => None,
    };
    if let Some(config) = config {
        writeln!(text, "  native configuration: {config:?}").unwrap();
    }
    if !matches!(engine, Engine::Interpreter) {
        writeln!(
            text,
            "  routine inlining: disabled by full execution reporting"
        )
        .unwrap();
    }
    if let Some(frontend) = timings.frontend {
        writeln!(text, "  frontend/read: {:.3} ms", ms(frontend)).unwrap();
    }
    writeln!(
        text,
        "  backend setup: {:.3} ms; session setup: {:.3} ms",
        ms(timings.backend),
        ms(timings.session)
    )
    .unwrap();
    writeln!(
        text,
        "  VM execution: {:.3} ms; input wait: {:.3} ms; run wall: {:.3} ms",
        ms(timings
            .execution
            .saturating_sub(report.first_call_wait)
            .saturating_sub(report.body_analysis_time)
            .saturating_sub(gc_pause)),
        ms(timings.input_wait),
        ms(timings.wall)
    )
    .unwrap();
    writeln!(
        text,
        "  run wall includes setup/console I/O; excludes frontend and process startup"
    )
    .unwrap();
    let instructions = report.instruction_count();
    let execution = timings
        .execution
        .saturating_sub(report.first_call_wait)
        .saturating_sub(report.body_analysis_time)
        .saturating_sub(gc_pause);
    let rate = if execution.is_zero() {
        0.0
    } else {
        instructions as f64 / execution.as_secs_f64() / 1_000_000.0
    };
    writeln!(
        text,
        "  bytecode instructions: {instructions}; throughput: {rate:.2} M/s"
    )
    .unwrap();
    if session.gc_config().enabled {
        writeln!(text,"  GC totals: {} collections; {} allocated, {} reclaimed, {} moved; {}/{} live/capacity cells; pauses {:.3} ms ({:.3} ms during report)",gc.collections,gc.allocated_cells,gc.reclaimed_cells,gc.moved_cells,gc.occupied_cells,gc.arena_capacity,ms(gc.pause_time),ms(gc_pause)).unwrap();
    }
    writeln!(
        text,
        "  peak frames: {}; peak VM cells: {}",
        report.peak_frames, report.peak_cells
    )
    .unwrap();
    let mut timeline = Vec::new();
    if let Some(Compiled::Tiered(artifact)) = artifact {
        writeln!(
            text,
            "  compiler workers: {}",
            cyallo::backend::compiler_worker_count()
        )
        .unwrap();
        let policy = artifact.policy();
        eager_policy(&mut text, policy);
        writeln!(
            text,
            "  tier policy: interp -> winch at {} calls or {} executed instructions; fast >= {} executed instructions; speed >= {} for loop-only routines <= {} bytecodes; notification interval {}; promotions at native safepoints",
            policy.winch_calls,
            policy.winch_instructions,
            policy.warm_instructions,
            policy.hot_instructions,
            policy.speed_size_limit,
            policy.execution_quantum
        )
        .unwrap();
        writeln!(
            text,
            "  fast configuration: {:?}; speed configuration: {:?}",
            CraneliftConfig::tiered(false),
            CraneliftConfig::tiered(true)
        )
        .unwrap();
        // Poll before snapshotting so completed-but-uninstalled jobs are visible.
        let (tiers, events) = artifact.reporting_snapshot();
        let mut compile_time = Duration::ZERO;
        for event in events {
            let name = &program.functions()[event.function].name;
            let description = match event.kind {
                TierEventKind::Superseded {
                    previous,
                    tier: next,
                } => format!(
                    "{name}: supersede queued {} with {}",
                    tier(previous),
                    tier(next)
                ),
                TierEventKind::Requested { tier: to, executed } => {
                    let work =
                        executed.map_or("manual".into(), |count| format!("{count} executed"));
                    format!(
                        "{name}: request {}; size={} bytecode, {work}",
                        tier(to),
                        program.functions()[event.function].code.len()
                    )
                }
                TierEventKind::CompileStarted {
                    tier: to,
                    worker,
                    queue_time,
                } => format!(
                    "{name}: compile {} on caie-codegen/{worker:?}; queued {:.3} ms",
                    tier(to),
                    ms(queue_time)
                ),
                TierEventKind::CompileFinished {
                    tier: to,
                    duration,
                    fallback,
                } => {
                    compile_time += duration;
                    let fallback =
                        fallback.map_or(String::new(), |reason| format!("; fallback: {reason}"));
                    format!(
                        "{name}: compile {} finished in {:.3} ms{fallback}",
                        tier(to),
                        ms(duration)
                    )
                }
                TierEventKind::Installed { previous, tier: to } => format!(
                    "{name}: install {} -> {}",
                    previous.map_or("uncompiled", tier),
                    tier(to)
                ),
                TierEventKind::Failed { error } => {
                    format!("{name}: compilation failed: {error}")
                }
                TierEventKind::FirstCall { waited, compiled } => format!(
                    "{name}: first-call admission; wait={:.3} ms; compiled={compiled}",
                    ms(waited)
                ),
                TierEventKind::Discarded {
                    tier: old,
                    installed,
                } => format!(
                    "{name}: discard {}; {} already installed",
                    tier(old),
                    tier(installed)
                ),
            };
            timeline.push((event.elapsed, description));
        }
        writeln!(
            text,
            "  completed compiler work: {:.3} ms (overlaps execution; not additive)",
            ms(compile_time)
        )
        .unwrap();
        for info in tiers {
            if info.compiling {
                writeln!(
                    text,
                    "  {}: {} pending at report; final report does not wait",
                    program.functions()[info.function].name,
                    info.requested.map_or("compilation", tier)
                )
                .unwrap();
            }
        }
    }
    for event in &report.events {
        let name = &program.functions()[event.function].name;
        let from = event.from.map_or("entry", ExecutionEngine::name);
        let action = if event.osr {
            "OSR"
        } else if event.from.is_some() {
            "handoff"
        } else {
            "enter"
        };
        let mut recovery = if event.to == ExecutionEngine::CraneliftSpeed {
            program.functions()[event.function]
                .resume
                .entries
                .iter()
                .find(|e| e.pc == event.pc)
                .map(|entry| format!("; live slots={}", entry.values.len()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        if event.osr
            && let Some(Compiled::Tiered(artifact)) = artifact
        {
            let target = match event.to {
                ExecutionEngine::Winch => Tier::Winch,
                ExecutionEngine::Cranelift => Tier::CraneliftFast,
                ExecutionEngine::CraneliftSpeed => Tier::CraneliftSpeed,
                ExecutionEngine::Interpreter => unreachable!(),
            };
            if let Some(ready) = artifact
                .events()
                .iter()
                .filter(|e| {
                    e.function == event.function
                        && e.elapsed <= event.elapsed
                        && matches!(e.kind,TierEventKind::CompileFinished {tier,..} if tier==target)
                })
                .map(|e| e.elapsed)
                .max()
            {
                recovery.push_str(&format!(
                    "; ready-to-entry={:.3} ms",
                    ms(event.elapsed - ready)
                ));
            }
        }
        timeline.push((
            event.elapsed,
            format!(
                "{name}: {action} {from} -> {}; invocation={} pc={} depth={} version={}{}",
                event.to.name(),
                event.invocation,
                event.pc,
                event.depth,
                event.code_version,
                recovery
            ),
        ));
    }
    timeline.sort_by_key(|(elapsed, _)| *elapsed);
    writeln!(text, "Timeline (relative to backend setup):").unwrap();
    for (elapsed, event) in timeline {
        writeln!(text, "  +{:8.3} ms  {event}", ms(elapsed)).unwrap();
    }
    if report.dropped_events != 0 {
        writeln!(
            text,
            "  {} further execution events omitted; counts remain complete",
            report.dropped_events
        )
        .unwrap();
    }
    writeln!(
        text,
        "Executed by tier (helper work charged to its calling tier):"
    )
    .unwrap();
    for engine in ExecutionEngine::ALL {
        let count: u64 = report
            .functions
            .iter()
            .map(|f| f.instructions[engine as usize])
            .sum();
        let elapsed: Duration = report
            .functions
            .iter()
            .map(|f| f.execution_time[engine as usize])
            .sum();
        let share = if instructions == 0 {
            0.0
        } else {
            count as f64 * 100.0 / instructions as f64
        };
        writeln!(
            text,
            "  {:17} {:10} instructions  {:6.2}%  {:8.3} ms",
            engine.name(),
            count,
            share,
            ms(elapsed)
        )
        .unwrap();
    }
    writeln!(text, "Functions:").unwrap();
    let b = &report.boundaries;
    writeln!(
        text,
        "Native yields: return={} budget={} compiler-notification={} OSR={} hot={} error={} continuation={}",
        report.native_exits[0],
        report.native_exits[1],
        report.native_exits[2],
        report.native_exits[3],
        report.native_exits[4],
        report.native_exits[5],
        report.native_exits[6]
    )
    .unwrap();
    writeln!(text,"Runtime boundaries: step={} flush={} flush-work={} flush-skipped={} state={} get={} put={}; native preparation={:.3} ms, materialization={:.3} ms",b.step,b.flush,b.flush-b.flush_skipped,b.flush_skipped,b.state,b.get,b.put,ms(report.native_prepare_time),ms(report.native_materialize_time)).unwrap();
    let services = session.service_stats();
    if services.provisions != 0 {
        writeln!(text,"Services: slot-hits={} resolutions={} target-hits={} native-target-hits={} target-misses={} provisions={} withdrawals={} released={}",services.slot_hits,services.resolutions,services.target_hits,services.native_target_hits,services.target_misses,services.provisions,services.withdrawals,services.released).unwrap();
    }
    writeln!(
        text,
        "First-call admissions: {}; compiler wait: {:.3} ms",
        report.first_call_admissions,
        ms(report.first_call_wait)
    )
    .unwrap();
    writeln!(text,"Deferred body checks: {}; {:.3} ms; VM execution above excludes body checks, compiler wait and GC pauses; per-tier phases include GC",report.deferred_bodies,ms(report.body_analysis_time)).unwrap();
    let mut osr = 0;
    for (fi, metrics) in report.functions.iter().enumerate() {
        let count: u64 = metrics.instructions.iter().sum();
        if count == 0 && metrics.invocations == 0 {
            continue;
        }
        osr += metrics.osr_transitions;
        writeln!(
            text,
            "  {}: invocations={} instructions={} OSR={} execution={:.3} ms",
            program.functions()[fi].name,
            metrics.invocations,
            count,
            metrics.osr_transitions,
            ms(metrics.execution_time.iter().copied().sum())
        )
        .unwrap();
    }
    writeln!(
        text,
        "OSR transitions: {osr} (VM-frame resume in the same invocation)"
    )
    .unwrap();
    writeln!(text,"Observation is opt-in; its timings include reporting overhead. First-call waits are reported separately.").unwrap();
    if let Some(observer) = program.inspection() {
        text.push_str(&observer.render());
    }
    text
}

/// Counter-only observation leaves the Session's full tracing hooks disabled.
/// Logical routine counters already include frameless native work and calls.
fn summary(
    artifact: &Option<Compiled>,
    program: &Program,
    session: &Session,
    engine: Engine,
    timings: &Timings,
) -> String {
    let mut text = String::new();
    let name = match engine {
        Engine::Jit => "jit",
        Engine::Interpreter => "interpreter",
        Engine::Cranelift => "cranelift",
        Engine::CraneliftSpeed => "cranelift_speed",
    };
    let status = match session.status() {
        Status::Finished => "Finished",
        Status::Paused => "Paused",
        Status::WaitingForInput => "WaitingForInput",
        Status::Failed(_) => "Failed",
    };
    writeln!(text, "Execution report: {name} ({status})").unwrap();
    writeln!(text, "  report mode: summary; optimizations unchanged").unwrap();
    if let Some(frontend) = timings.frontend {
        writeln!(text, "  frontend/read: {:.3} ms", ms(frontend)).unwrap();
    }
    writeln!(
        text,
        "  backend setup: {:.3} ms; session setup: {:.3} ms",
        ms(timings.backend),
        ms(timings.session)
    )
    .unwrap();
    writeln!(
        text,
        "  VM execution: {:.3} ms; input wait: {:.3} ms; run wall: {:.3} ms",
        ms(timings.execution),
        ms(timings.input_wait),
        ms(timings.wall)
    )
    .unwrap();
    writeln!(text, "  execution includes deferred checks, compiler waits and GC; run wall excludes frontend, startup and report rendering").unwrap();
    let count = session
        .instruction_count()
        .saturating_sub(timings.counters.instructions);
    writeln!(text, "  bytecode instructions: {count}").unwrap();
    writeln!(
        text,
        "  Inlining: frameless calls: {}; embedded procedure calls: {}",
        session
            .inlined_call_count()
            .saturating_sub(timings.counters.inlined),
        session
            .body_inlined_call_count()
            .saturating_sub(timings.counters.procedures)
    )
    .unwrap();
    let gc = session.gc_stats();
    if session.gc_config().enabled {
        writeln!(text, "  GC totals: {} collections; {} allocated, {} reclaimed, {} moved; {}/{} live/capacity cells; pauses {:.3} ms ({:.3} ms during execution)", gc.collections, gc.allocated_cells, gc.reclaimed_cells, gc.moved_cells, gc.occupied_cells, gc.arena_capacity, ms(gc.pause_time), ms(gc.pause_time.saturating_sub(timings.counters.gc_pause))).unwrap();
    }
    let services = session.service_stats();
    if services.provisions != 0 {
        writeln!(text,"Services: slot-hits={} resolutions={} target-hits={} native-target-hits={} target-misses={} provisions={} withdrawals={} released={}", services.slot_hits, services.resolutions, services.target_hits, services.native_target_hits, services.target_misses, services.provisions, services.withdrawals, services.released).unwrap();
    }
    match engine {
        Engine::Cranelift => {
            writeln!(
                text,
                "  native configuration: {:?}",
                CraneliftConfig::fast()
            )
            .unwrap();
        }
        Engine::CraneliftSpeed => {
            writeln!(
                text,
                "  native configuration: {:?}",
                CraneliftConfig::speed()
            )
            .unwrap();
        }
        _ => {}
    }
    if let Some(Compiled::Tiered(artifact)) = artifact {
        let policy = artifact.policy();
        eager_policy(&mut text, policy);
        writeln!(text, "  tier policy: interp -> winch at {} calls or {} instructions; fast >= {}; speed >= {} for eligible loop-only routines", policy.winch_calls, policy.winch_instructions, policy.warm_instructions, policy.hot_instructions).unwrap();
        let (tiers, events) = artifact.reporting_snapshot();
        let compile: Duration = events
            .iter()
            .filter_map(|event| {
                if let TierEventKind::CompileFinished { duration, .. } = event.kind {
                    Some(duration)
                } else {
                    None
                }
            })
            .sum();
        writeln!(
            text,
            "  completed compiler work: {:.3} ms (overlaps execution; not additive)",
            ms(compile)
        )
        .unwrap();
        writeln!(text, "Compiler timeline:").unwrap();
        for event in events.iter().take(64) {
            writeln!(
                text,
                "  +{:8.3} ms  {}: {:?}",
                ms(event.elapsed),
                program.functions()[event.function].name,
                event.kind
            )
            .unwrap();
        }
        if events.len() > 64 {
            writeln!(
                text,
                "  {} compiler events omitted; use --jit-trace for the full timeline",
                events.len() - 64
            )
            .unwrap();
        }
        for info in tiers.iter().filter(|info| info.compiling) {
            writeln!(
                text,
                "  {}: compilation pending; summary does not wait",
                program.functions()[info.function].name
            )
            .unwrap();
        }
    }
    writeln!(
        text,
        "Functions (logical calls/work, including inlined bodies):"
    )
    .unwrap();
    for (fi, function) in program.functions().iter().enumerate() {
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
        if work != 0 || calls != 0 {
            let calls = if fi == 0 && work != 0 {
                calls.max(1)
            } else {
                calls
            };
            writeln!(
                text,
                "  {}: calls={calls} instructions={work}",
                function.name
            )
            .unwrap();
        }
    }
    writeln!(
        text,
        "Use --jit-trace for per-invocation/tier timings and OSR tracing; it disables inlining."
    )
    .unwrap();
    if let Some(observer) = program.inspection() {
        text.push_str(&observer.render());
    }
    text
}

fn eager_policy(text: &mut String, policy: cyallo::backend::TierPolicy) {
    let eager = policy.eager;
    writeln!(text,"  eager: {}; words [{}, {}); {} outstanding jobs; {} speculative words; predicted budget {:.3} ms; shared first-call wait {:.3} ms; hot thresholds /2, cold x4",eager.enabled,eager.min_words,eager.max_words,eager.max_routines,eager.max_total_words,ms(eager.predicted_budget),ms(policy.first_call_wait)).unwrap();
}
