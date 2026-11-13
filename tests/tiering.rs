use cyallo::{
    Session, Status,
    backend::{Executable, NativeBackend, Tier, TierEventKind, TierPolicy, TieredBackend},
    compile,
    ir::RoutineShape,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn native_fast_request(
    source: &str,
    target: usize,
    caller_tier: Tier,
    budget: usize,
    reporting: bool,
) {
    let program = Arc::new(compile(source).unwrap());
    let origin = Instant::now();
    let artifact = TieredBackend::new(TierPolicy {
        warm_instructions: 5_000,
        first_call_wait: Duration::ZERO,
        ..Default::default()
    })
    .compile_with_reporting(program.clone(), origin)
    .unwrap();
    for fi in 0..program.functions().len() {
        artifact
            .precompile(
                fi,
                if fi == target {
                    Tier::Winch
                } else {
                    caller_tier
                },
            )
            .unwrap();
    }
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    let mut native = Session::new(program.clone()).unwrap();
    let mut interpreter = Session::new(program).unwrap();
    if reporting {
        native.enable_reporting(origin);
    }
    while native.instruction_count() < 100_000 {
        let remaining = 100_000 - native.instruction_count() as usize;
        artifact.run(&mut native, budget.min(remaining)).unwrap();
    }
    interpreter.run(100_000);
    assert_eq!(native.status(), interpreter.status());
    assert_eq!(native.instruction_count(), interpreter.instruction_count());
    assert_eq!(native.globals(), interpreter.globals());
    let requests: Vec<_> = artifact
        .events()
        .into_iter()
        .filter_map(|event| {
            if event.function == target
                && let TierEventKind::Requested {
                    tier: Tier::CraneliftFast,
                    executed,
                } = event.kind
            {
                executed
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        requests.len(),
        1,
        "native routine must request fast exactly once; {:?}",
        artifact.tiers()
    );
    assert!(
        (5_000..=5_064).contains(&requests[0]),
        "request must follow native poll spacing, not the outer budget: {:?}",
        requests
    );
    if target != 0 && caller_tier == Tier::CraneliftFast && !reporting {
        assert!(
            native.inlined_call_count() > 0,
            "promotion must also observe frameless work"
        );
    }
}

#[test]
fn native_winch_requests_fast_at_the_warm_threshold() {
    for reporting in [false, true] {
        for budget in [128, 1_000_000] {
            native_fast_request(
                "let mut total = 0\nfor i = 1 to 100000 { total += i }\nstd::io::print(total)\n",
                0,
                Tier::Winch,
                budget,
                reporting,
            );
        }
    }
}

#[test]
fn native_callees_request_fast_without_returning_to_the_driver() {
    let source = "fn leaf(n: i64) -> i64 { return n + 1 }\nfn middle(n: i64) -> i64 { return leaf(n) }\nlet mut total = 0\nfor i = 1 to 100000 { total += middle(i) }\nstd::io::print(total)\n";
    for caller_tier in [Tier::Winch, Tier::CraneliftFast] {
        for reporting in [false, true] {
            for target in [1, 2] {
                native_fast_request(source, target, caller_tier, 1_000_000, reporting);
            }
        }
    }
}

#[test]
fn rejected_speed_size_does_not_repeatedly_yield_native_code() {
    let program = Arc::new(compile("let mut n = 0\nwhile true { n += 1 }\n").unwrap());
    let artifact = TieredBackend::new(TierPolicy {
        hot_instructions: 64,
        speed_size_limit: 0,
        ..Default::default()
    })
    .compile_with_reporting(program.clone(), Instant::now())
    .unwrap();
    artifact.precompile(0, Tier::CraneliftFast).unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    let mut session = Session::new(program).unwrap();
    session.enable_reporting(Instant::now());
    artifact.run(&mut session, 100_000).unwrap();
    assert_eq!(session.instruction_count(), 100_000);
    assert_eq!(session.status(), &Status::Paused);
    assert_eq!(session.execution_report().unwrap().native_exits[4], 0);
    assert_eq!(artifact.tiers()[0].requested, Some(Tier::CraneliftFast));
}
#[test]
fn policy_selects_three_tiers() {
    let p = TierPolicy::default();
    assert_eq!(p.select(10, 0), Tier::Winch);
    assert_eq!(p.select(100, 0), Tier::CraneliftFast);
    assert_eq!(p.select(1000, 0), Tier::CraneliftSpeed);
    assert_eq!(p.select(43, 1_900_030), Tier::Winch);
    assert_eq!(p.select(100, 1_900_030), Tier::CraneliftFast);
    assert_eq!(p.select(10, p.hot_instructions), Tier::CraneliftSpeed);
}
#[test]
fn work_based_promotion_waits_for_warm_work_and_loop_shape() {
    let p = TierPolicy::default();
    let looping = RoutineShape {
        loops: true,
        calls: false,
    };
    let calling = RoutineShape {
        loops: true,
        calls: true,
    };
    let leaf = RoutineShape::default();
    let c = p.winch_calls;
    assert_eq!(p.promote(looping, 10, 0, 0), None);
    assert_eq!(p.promote(leaf, 10, 0, 0), None);
    // Winch goes in as soon as the routine is called often enough.
    assert_eq!(p.promote(leaf, 10, 0, c - 1), None);
    assert_eq!(p.promote(leaf, 10, 0, c), Some(Tier::Winch));
    assert_eq!(p.promote(looping, 10, 0, c), Some(Tier::Winch));
    // Cranelift fast replaces Winch once the work justifies its compile cost.
    assert_eq!(
        p.promote(looping, 10, p.warm_instructions, c),
        Some(Tier::CraneliftFast)
    );
    assert_eq!(
        p.promote(leaf, 10, p.warm_instructions, c),
        Some(Tier::CraneliftFast)
    );
    assert_eq!(
        p.promote(looping, 10, p.hot_instructions, c),
        Some(Tier::CraneliftSpeed)
    );
    assert_eq!(
        p.promote(calling, 10, p.hot_instructions, c),
        Some(Tier::CraneliftFast)
    );
    assert_eq!(
        p.promote(looping, p.speed_size_limit + 1, p.hot_instructions, c),
        Some(Tier::CraneliftFast)
    );
}
#[test]
fn lower_requests_cannot_downgrade_installed_optimized_code() {
    let p = Arc::new(compile("std::io::print(1)\n").unwrap());
    let artifact = TieredBackend::default().compile(p.clone()).unwrap();
    artifact.precompile(0, Tier::CraneliftSpeed).unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    artifact.precompile(0, Tier::Winch).unwrap();
    artifact.precompile(0, Tier::CraneliftFast).unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    assert_eq!(artifact.tiers()[0].tier, Some(Tier::CraneliftSpeed));
    let mut s = Session::new(p).unwrap();
    artifact.run(&mut s, 100).unwrap();
    assert_eq!(s.context.output, "1\n");
}
#[test]
fn compilation_runs_on_dedicated_worker_and_all_tiers_execute() {
    for tier in [Tier::Winch, Tier::CraneliftFast, Tier::CraneliftSpeed] {
        let program = Arc::new(
            compile("let mut n = 0\nfor i = 1 to 10 { n += i }\nstd::io::print(n)\n").unwrap(),
        );
        let artifact = TieredBackend::default().compile(program.clone()).unwrap();
        assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
        artifact.precompile(0, tier).unwrap();
        assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
        let info = &artifact.tiers()[0];
        assert_ne!(info.compiler_thread, Some(std::thread::current().id()));
        assert!(info.error.is_none(), "{:?}", info);
        if tier == Tier::Winch && info.tier != Some(Tier::Winch) {
            panic!("Winch failed on this host: {:?}", info.fallback_reason);
        }
        if tier != Tier::Winch {
            assert_eq!(info.tier, Some(tier));
        }
        let mut session = Session::new(program).unwrap();
        artifact.run(&mut session, 10_000).unwrap();
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "55\n");
    }
}
#[test]
fn tiers_preserve_calls_input_and_errors() {
    let program=Arc::new(compile("fn twice(n: i64) -> i64 { return n * 2 }\nlet mut n = 0\nstd::runtime::caie::input(&mut n)\nstd::io::print(twice(n))\nstd::io::print(1 div 0)\n").unwrap());
    let artifact = TieredBackend::default().compile(program.clone()).unwrap();
    for function in 0..program.functions().len() {
        artifact.precompile(function, Tier::CraneliftSpeed).unwrap();
    }
    artifact.wait_for_compilation(Duration::from_secs(30));
    let mut a = Session::new(program.clone()).unwrap();
    let mut b = Session::new(program).unwrap();
    a.context.input.push_back("21".into());
    b.context.input.push_back("21".into());
    a.run(10000);
    artifact.run(&mut b, 10000).unwrap();
    assert_eq!(a.context.output, b.context.output);
    assert_eq!(a.status(), b.status());
}
#[test]
fn hot_loop_promotes_at_budget_boundary() {
    let program = Arc::new(compile("let mut n = 0\nwhile true { n += 1 }\n").unwrap());
    let artifact = TieredBackend::new(TierPolicy {
        hot_instructions: 50,
        warm_instructions: 0,
        execution_quantum: 16,
        ..Default::default()
    })
    .compile(program.clone())
    .unwrap();
    artifact.wait_for_compilation(Duration::from_secs(30));
    let mut session = Session::new(program).unwrap();
    artifact.run(&mut session, 1000).unwrap();
    artifact.wait_for_compilation(Duration::from_secs(30));
    artifact.run(&mut session, 1000).unwrap();
    assert_eq!(artifact.tiers()[0].tier, Some(Tier::CraneliftSpeed));
    assert_eq!(session.status(), &Status::Paused);
    assert_eq!(session.instruction_count(), 2000);
}
#[test]
fn osr_promotes_a_running_invocation_without_restarting_it() {
    let source = "let mut calls = 0\nproc work(limit: i64) -> i64 {\n calls += 1\n let mut progress = 0\n while progress < limit { progress += 1 }\n return progress\n}\nlet result = work(10000)\nstd::io::print(result, \":\", calls)\n";
    let program = Arc::new(compile(source).unwrap());
    let artifact = TieredBackend::new(TierPolicy {
        hot_instructions: 64,
        warm_instructions: 0,
        execution_quantum: 16,
        ..Default::default()
    })
    .compile(program.clone())
    .unwrap();
    let mut session = Session::new(program).unwrap();
    artifact.run(&mut session, 300).unwrap();
    assert_eq!(session.stack().last().map(String::as_str), Some("work"));
    for _ in 0..4 {
        artifact.wait_for_compilation(Duration::from_secs(30));
        artifact.run(&mut session, 64).unwrap();
        if artifact.tiers()[1].tier == Some(Tier::CraneliftSpeed) {
            break;
        }
    }
    assert_eq!(artifact.tiers()[1].tier, Some(Tier::CraneliftSpeed));
    assert_eq!(
        session.stack().last().map(String::as_str),
        Some("work"),
        "promotion must happen before this invocation returns"
    );
    assert!(session.globals().contains(&("calls".into(), "1".into())));
    assert!(
        session
            .variables()
            .iter()
            .any(|(name, value)| name == "progress" && value.parse::<i64>().is_ok_and(|n| n > 0))
    );
    artifact.run(&mut session, 500_000).unwrap();
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "10000:1\n");
    assert_ne!(
        artifact.tiers()[1].compiler_thread,
        Some(std::thread::current().id())
    );
}
#[test]
fn winch_checked_arithmetic_and_input_match_interpreter() {
    for source in [
        "let x = 9223372036854775807\nstd::io::print(x + 1)\n",
        "std::io::print(-9223372036854775808 - 1)\n",
        "let x = 1.5\nstd::io::print(x * 2.0 + 3.0)\n",
        "let mut n = 0\nstd::runtime::caie::input(&mut n)\nstd::io::print(n + 1)\n",
    ] {
        let program = Arc::new(compile(source).unwrap());
        let artifact = TieredBackend::default().compile(program.clone()).unwrap();
        artifact.precompile(0, Tier::Winch).unwrap();
        assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
        assert_eq!(artifact.tiers()[0].tier, Some(Tier::Winch));
        let mut a = Session::new(program.clone()).unwrap();
        let mut b = Session::new(program).unwrap();
        a.context.input.push_back("41".into());
        b.context.input.push_back("41".into());
        a.run(1000);
        artifact.run(&mut b, 1000).unwrap();
        assert_eq!(a.context.output, b.context.output);
        assert_eq!(a.status(), b.status());
        assert_eq!(a.instruction_count(), b.instruction_count());
    }
}
#[test]
fn osr_keeps_byref_aliases_to_caller_storage() {
    let source = "let mut n = 0\nproc work(left: &mut i64) { let right = left; while *left < 1000 { *left += 1; *right += 1 } }\nwork(&mut n)\nstd::io::print(n)\n";
    let program = Arc::new(compile(source).unwrap());
    let artifact = TieredBackend::new(TierPolicy {
        hot_instructions: 32,
        warm_instructions: 0,
        execution_quantum: 16,
        ..Default::default()
    })
    .compile(program.clone())
    .unwrap();
    let mut session = Session::new(program).unwrap();
    artifact.run(&mut session, 200).unwrap();
    for _ in 0..4 {
        artifact.wait_for_compilation(Duration::from_secs(30));
        artifact.run(&mut session, 32).unwrap();
        if artifact.tiers()[1].tier == Some(Tier::CraneliftSpeed) {
            break;
        }
    }
    assert_eq!(artifact.tiers()[1].tier, Some(Tier::CraneliftSpeed));
    assert_eq!(session.stack().last().map(String::as_str), Some("work"));
    artifact.run(&mut session, 100_000).unwrap();
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "1000\n");
}
