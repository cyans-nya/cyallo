use cyallo::{
    Session, Status,
    backend::{
        CraneliftSpeedBackend, Executable, NativeBackend, Tier, TierEventKind, TierPolicy,
        TieredBackend,
    },
    compile,
    metrics::ExecutionEngine,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn assert_counts(session: &Session) {
    let report = session.execution_report().unwrap();
    assert_eq!(report.instruction_count(), session.instruction_count());
    for (fi, metrics) in report.functions.iter().enumerate() {
        assert_eq!(
            metrics.instructions.iter().sum::<u64>(),
            session.function_instruction_count(fi)
        );
    }
}
fn backend() -> TieredBackend {
    TieredBackend {
        policy: TierPolicy {
            small_limit: usize::MAX - 1,
            heavy_limit: usize::MAX,
            hot_instructions: u64::MAX,
            warm_instructions: u64::MAX,
            ..Default::default()
        },
    }
}

#[test]
fn osr_report_tracks_same_invocation_and_executed_tiers() {
    let program = Arc::new(compile("let mut calls = 0\nproc work(limit: i64) -> i64 { calls += 1; let mut n = 0; while n < limit { n += 1 }; return n }\nstd::io::print(work(10000), \":\", calls)\n").unwrap());
    let origin = Instant::now();
    let artifact = backend()
        .compile_with_reporting(program.clone(), origin)
        .unwrap();
    artifact.precompile(1, Tier::Winch).unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    let mut session = Session::new(program).unwrap();
    session.enable_reporting(origin);
    session.run(128);
    artifact.run(&mut session, 128).unwrap();
    artifact.precompile(1, Tier::CraneliftSpeed).unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    artifact.run(&mut session, 128).unwrap();
    let report = session.execution_report().unwrap();
    let events: Vec<_> = report
        .events
        .iter()
        .filter(|e| e.function == 1 && e.osr)
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].from, Some(ExecutionEngine::Interpreter));
    assert_eq!(events[0].to, ExecutionEngine::Winch);
    assert_eq!(events[1].from, Some(ExecutionEngine::Winch));
    assert_eq!(events[1].to, ExecutionEngine::CraneliftSpeed);
    assert_eq!(events[0].invocation, events[1].invocation);
    for event in events {
        assert_ne!(event.pc, 0);
        assert_eq!(event.depth, 2);
    }
    for tier in [
        ExecutionEngine::Interpreter,
        ExecutionEngine::Winch,
        ExecutionEngine::CraneliftSpeed,
    ] {
        assert!(report.functions[1].instructions[tier as usize] > 0);
    }
    artifact.run(&mut session, 500_000).unwrap();
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "10000:1\n");
    assert_counts(&session);
    let events = artifact.events();
    assert!(events.iter().any(|e|matches!(e.kind,TierEventKind::CompileStarted{worker,..} if worker!=std::thread::current().id())));
    assert!(events.iter().any(|e| matches!(
        e.kind,
        TierEventKind::Installed {
            previous: Some(Tier::Winch),
            tier: Tier::CraneliftSpeed
        }
    )));
    assert_eq!(
        session.execution_report().unwrap().functions[1].invocations,
        1
    );
}

#[test]
fn native_callees_are_charged_to_their_own_tier_and_caller_is_restored() {
    let program = Arc::new(compile("fn inner(n: i64) -> i64 { return n * 2 }\nfn outer(n: i64) -> i64 { return inner(n) + 1 }\nstd::io::print(outer(7))\n").unwrap());
    let origin = Instant::now();
    let artifact = backend()
        .compile_with_reporting(program.clone(), origin)
        .unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    for (fi, tier) in [
        (0, Tier::CraneliftFast),
        (1, Tier::CraneliftSpeed),
        (2, Tier::CraneliftFast),
    ] {
        artifact.precompile(fi, tier).unwrap();
    }
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    let mut session = Session::new(program).unwrap();
    session.enable_reporting(origin);
    artifact.run(&mut session, 1000).unwrap();
    assert_eq!(session.context.output, "15\n");
    assert_counts(&session);
    let report = session.execution_report().unwrap();
    for (fi, engine) in [
        (0, ExecutionEngine::Cranelift),
        (1, ExecutionEngine::CraneliftSpeed),
        (2, ExecutionEngine::Cranelift),
    ] {
        let metrics = &report.functions[fi];
        assert_eq!(metrics.invocations, 1);
        assert_eq!(metrics.osr_transitions, 0);
        assert_eq!(
            metrics.instructions[engine as usize],
            session.function_instruction_count(fi)
        );
        assert!(metrics.instructions[engine as usize] > 0);
    }
}

#[test]
fn reports_are_optional_and_failed_native_work_is_counted() {
    let program = Arc::new(
        compile(
            "let mut n = 40\nn += 2\nstd::io::print(n)\nstd::io::print(9223372036854775807 + 1)\n",
        )
        .unwrap(),
    );
    let native = CraneliftSpeedBackend.compile(program.clone()).unwrap();
    let mut session = Session::new(program).unwrap();
    assert!(session.execution_report().is_none());
    session.enable_reporting(Instant::now());
    native.run(&mut session, 1000).unwrap();
    assert!(matches!(session.status(), Status::Failed(_)));
    assert_eq!(session.context.output, "42\n");
    assert_counts(&session);
    assert_eq!(
        session.execution_report().unwrap().functions[0].osr_transitions,
        0
    );
}
#[test]
fn native_scalar_loop_has_no_per_instruction_runtime_boundaries() {
    let program = Arc::new(
        compile("let mut n = 0\nwhile n < 10000 { n += 1 }\nstd::io::print(n)\n").unwrap(),
    );
    let native = backend().compile(program.clone()).unwrap();
    native.precompile(0, Tier::Winch).unwrap();
    assert!(native.wait_for_compilation(Duration::from_secs(30)));
    let mut s = Session::new(program).unwrap();
    s.enable_reporting(Instant::now());
    native.run(&mut s, 500000).unwrap();
    let report = s.execution_report().unwrap();
    assert_eq!(s.context.output, "10000\n");
    assert!(report.functions[0].instructions[ExecutionEngine::Winch as usize] > 100000);
    assert_eq!(
        report.boundaries.step, 2,
        "only output and return use helpers"
    );
    assert_eq!(
        report.boundaries.state + report.boundaries.get + report.boundaries.put,
        0
    );
    assert_counts(&s);
}
#[test]
fn optimized_budget_resumes_stay_in_speed_without_repeat_promotions() {
    let program=Arc::new(compile("fn work(n: i64) -> i64 { let mut i = 0; while i < n { i += 1 }; return i }\nstd::io::print(work(100))\n").unwrap());
    let native = CraneliftSpeedBackend.compile(program.clone()).unwrap();
    let mut session = Session::new(program).unwrap();
    session.enable_reporting(Instant::now());
    while *session.status() != Status::Finished {
        native.run(&mut session, 37).unwrap();
    }
    assert_eq!(session.context.output, "100\n");
    assert_counts(&session);
    let report = session.execution_report().unwrap();
    assert_eq!(
        report.functions[1].instructions[ExecutionEngine::Interpreter as usize],
        0,
        "speed budget resumes must not interpret"
    );
    assert_eq!(
        report.functions[1].osr_transitions, 0,
        "resuming the same optimized version is not another promotion"
    );
    assert_eq!(
        report.functions[1].instructions[ExecutionEngine::CraneliftSpeed as usize],
        session.function_instruction_count(1)
    );
    assert_eq!(report.events.iter().filter(|e| e.function == 1).count(), 1);
}
