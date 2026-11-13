use cyallo::{
    Session, Status,
    backend::{
        CompilationScope, CraneliftBackend, CraneliftConfig, CraneliftSpeedBackend, Executable,
        NativeBackend, OptimizationLevel, ResumeStrategy, Tier, TierPolicy, TieredBackend,
    },
    compile,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn parity(a: &Session, b: &Session) {
    assert_eq!(a.status(), b.status());
    assert_eq!(a.instruction_count(), b.instruction_count());
    assert_eq!(a.context.output, b.context.output);
    assert_eq!(a.current_span(), b.current_span());
    assert_eq!(a.stack(), b.stack());
    assert_eq!(a.variables(), b.variables());
    assert_eq!(a.globals(), b.globals());
    for fi in 0..a.program().functions().len() {
        assert_eq!(
            a.function_instruction_count(fi),
            b.function_instruction_count(fi)
        );
    }
}

#[test]
fn configured_cranelift_layouts_preserve_each_budget_and_resume() {
    let source = "let mut n = 0\nproc add(a: &mut i64) { let b = a; *a += 3; *b += *a }\nfn fact(n: i64) -> i64 { if n <= 1 { return 1 }; return n * fact(n-1) }\nfor i in 1..=4 { add(&mut n) }\nstd::io::print(n, \":\", fact(6))\n";
    let program = Arc::new(compile(source).unwrap());
    for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
        for resume in [ResumeStrategy::AllPcs, ResumeStrategy::LoopHeaders] {
            for optimization in [
                OptimizationLevel::None,
                OptimizationLevel::SpeedAndSize,
                OptimizationLevel::Speed,
            ] {
                let config = CraneliftConfig {
                    scope,
                    resume,
                    optimization,
                    ..Default::default()
                };
                let artifact = config.compile(program.clone()).unwrap();
                assert_eq!(artifact.configuration(), config);
                assert!(artifact.code_size() > 0);
                for budget in [1, 31, 100_000] {
                    let mut a = Session::new(program.clone()).unwrap();
                    let mut b = Session::new(program.clone()).unwrap();
                    while *a.status() != Status::Finished {
                        a.run(budget);
                        artifact.run(&mut b, budget).unwrap();
                        assert_eq!(
                            a.instruction_count(),
                            b.instruction_count(),
                            "{config:?}; budget={budget}"
                        );
                        parity(&a, &b);
                    }
                }
            }
        }
    }
}

#[test]
fn clean_native_call_returns_skip_materialization_but_dirty_yields_do_not() {
    let source = "fn square(n: i64) -> i64 { return n * n }\nfn cube(n: i64) -> i64 { return square(n) * n }\nlet mut total = 0\nfor i in 1..=100 { total += cube(i) }\nstd::io::print(total)\n";
    let program = Arc::new(compile(source).unwrap());
    for tiers in [
        [Tier::Winch, Tier::Winch, Tier::Winch],
        [
            Tier::CraneliftFast,
            Tier::CraneliftFast,
            Tier::CraneliftFast,
        ],
        [
            Tier::CraneliftSpeed,
            Tier::CraneliftSpeed,
            Tier::CraneliftSpeed,
        ],
        [Tier::Winch, Tier::CraneliftSpeed, Tier::CraneliftFast],
    ] {
        let artifact = TieredBackend::new(TierPolicy {
            warm_instructions: u64::MAX,
            hot_instructions: u64::MAX,
            ..Default::default()
        })
        .compile(program.clone())
        .unwrap();
        for (fi, tier) in tiers.into_iter().enumerate() {
            artifact.precompile(fi, tier).unwrap();
        }
        assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
        for budget in [7, 100_000] {
            let mut a = Session::new(program.clone()).unwrap();
            let mut b = Session::new(program.clone()).unwrap();
            b.enable_reporting(Instant::now());
            while *a.status() != Status::Finished {
                a.run(budget);
                artifact.run(&mut b, budget).unwrap();
                parity(&a, &b);
            }
            let boundaries = &b.execution_report().unwrap().boundaries;
            assert!(boundaries.flush > 0);
            assert!(
                boundaries.flush_skipped > 0,
                "normal native returns are already synchronized"
            );
            if budget == 100_000 {
                assert_eq!(boundaries.flush, boundaries.flush_skipped);
            } else {
                assert!(
                    boundaries.flush > boundaries.flush_skipped,
                    "dirty suspended callees still need synchronization"
                );
            }
        }
    }
}

#[test]
fn every_instruction_can_handoff_between_native_and_interpreter() {
    let source = "let mut x: f64 = 2.0\nlet mut n = 0\nproc add(a: &mut i64) { let b = a; *a += 3; *b += *a }\nfor i in 1..=8 { add(&mut n); x = x * 1.5; if n > 100 { n -= 7 } }\nlet mut a = [n, 2, 3]\nstd::io::print(a[0], x, '🐈' > 'a', !false)\n";
    let program = Arc::new(compile(source).unwrap());
    let baseline = TieredBackend::new(TierPolicy {
        hot_instructions: u64::MAX,
        warm_instructions: u64::MAX,
        ..Default::default()
    })
    .compile(program.clone())
    .unwrap();
    for fi in 0..program.functions().len() {
        baseline.precompile(fi, Tier::Winch).unwrap();
    }
    assert!(baseline.wait_for_compilation(Duration::from_secs(30)));
    let artifacts: Vec<Box<dyn Executable>> = vec![
        Box::new(CraneliftBackend.compile(program.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(program.clone()).unwrap()),
        Box::new(baseline),
    ];
    for native in artifacts {
        let mut a = Session::new(program.clone()).unwrap();
        let mut b = Session::new(program.clone()).unwrap();
        for i in 0..10_000 {
            a.run(1);
            if i % 3 == 0 {
                b.run(1);
            } else {
                native.run(&mut b, 1).unwrap();
            }
            parity(&a, &b);
            if *a.status() == Status::Finished {
                break;
            }
        }
        assert_eq!(b.status(), &Status::Finished);
    }
}

#[test]
fn native_calls_preserve_recursive_frames_aliases_and_budgets() {
    let source = "let mut total = 0\nproc work(a: &mut i64, depth: i64) { let b = a; *a += 1; if depth > 0 { work(a, depth-1) }; *b += 2 }\nfn fact(n: i64) -> i64 { if n <= 1 { return 1 }; return n * fact(n-1) }\nwork(&mut total, 300)\nstd::io::print(total, \":\", fact(10))\n";
    let program = Arc::new(compile(source).unwrap());
    let native = TieredBackend::default().compile(program.clone()).unwrap();
    for fi in 0..program.functions().len() {
        native.precompile(fi, Tier::CraneliftSpeed).unwrap();
    }
    assert!(native.wait_for_compilation(Duration::from_secs(30)));
    // A second request handles a baseline job already in flight on entry.
    for fi in 0..program.functions().len() {
        native.precompile(fi, Tier::CraneliftSpeed).unwrap();
    }
    assert!(native.wait_for_compilation(Duration::from_secs(30)));
    for info in native.tiers() {
        assert_eq!(info.tier, Some(Tier::CraneliftSpeed), "{info:?}");
    }
    let artifacts: Vec<Box<dyn Executable>> = vec![
        Box::new(native),
        Box::new(CraneliftBackend.compile(program.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(program.clone()).unwrap()),
    ];
    for native in artifacts {
        for budget in [1, 7, 127, 100_000] {
            let mut a = Session::new(program.clone()).unwrap();
            let mut b = Session::new(program.clone()).unwrap();
            while *a.status() != Status::Finished {
                a.run(budget);
                native.run(&mut b, budget).unwrap();
                parity(&a, &b);
            }
            assert_eq!(b.context.output, "903:3628800\n");
        }
    }
}

#[test]
fn native_errors_materialize_prior_writes_and_preserve_source_spans() {
    for expression in [
        "9223372036854775807 + 1",
        "(-9223372036854775808) - 1",
        "9223372036854775807 * 2",
        "-(-9223372036854775808)",
        "(-9223372036854775808) / -1",
        "1 / 0",
        "1 % 0",
        "1.5 / 0.0 > 0.0 && 1 / 0 == 0",
        "[0][1]",
    ] {
        let source = format!("let mut result = 0\nresult = 42\nstd::io::print({expression})\n");
        let program = Arc::new(compile(&source).unwrap());
        let native = CraneliftBackend.compile(program.clone()).unwrap();
        let mut a = Session::new(program.clone()).unwrap();
        let mut b = Session::new(program).unwrap();
        a.run(1000);
        native.run(&mut b, 1000).unwrap();
        // Failed interpreter instructions advance PC internally; diagnostics
        // and all externally visible storage/counters must still be identical.
        parity(&a, &b);
        assert!(matches!(b.status(), Status::Failed(_)), "{expression}");
        assert!(b.globals().contains(&("result".into(), "42".into())));
    }
}

#[test]
fn native_sparse_allocation_and_complex_helpers_observe_scalar_writes() {
    let source = "let mut values = std::collections::Vec::from(0..0)\nlet mut current = 0\nfor i in 0..=600 { current += 1; values.push(current) }\nlet copy = std::mem::clone(&values)\nvalues[0] = 999\nstd::io::print(std::array::sum(copy), \":\", copy[0], \":\", values[0])\nstd::io::print((-9223372036854775808) % -1, 1.0 + 2.5, 2.5 > 2.0, '🐈' == '🐈')\n";
    let program = Arc::new(compile(source).unwrap());
    let native = CraneliftBackend.compile(program.clone()).unwrap();
    let mut a = Session::new(program.clone()).unwrap();
    let mut b = Session::new(program).unwrap();
    a.run(100_000);
    native.run(&mut b, 100_000).unwrap();
    parity(&a, &b);
    assert_eq!(b.status(), &Status::Finished);
    assert_eq!(b.context.output, "180901:1:999\n03.5truetrue\n");
}

#[test]
fn returned_frames_reuse_their_local_array_cells_without_gc() {
    let source = "fn work() -> i64 { let local = [1,2,3,4,5,6,7,8,9]; return 1 }\nlet mut total = 0\nlet mut i = 0\nwhile i < 200000 { total += work(); i += 1 }\nlet mut values = std::collections::Vec::from(0..0)\nproc add_global() { values.push(42) }\nadd_global()\nstd::io::print(total, \":\", values[0])\n";
    let program = Arc::new(compile(source).unwrap());
    let mut session = Session::new(program.clone()).unwrap();
    session.run(20_000_000);
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "200000:42\n");
    assert!(
        session.heap_cell_count() < 64,
        "frame-local cells accumulated: {}",
        session.heap_cell_count()
    );

    let native = CraneliftBackend.compile(program.clone()).unwrap();
    let mut native_session = Session::new(program).unwrap();
    native.run(&mut native_session, 20_000_000).unwrap();
    assert_eq!(native_session.status(), &Status::Finished);
    assert_eq!(native_session.context.output, "200000:42\n");
    assert!(
        native_session.heap_cell_count() < 64,
        "native frame-local cells accumulated: {}",
        native_session.heap_cell_count()
    );
}
