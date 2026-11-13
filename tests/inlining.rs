use cyallo::backend::{
    CompilationScope, CraneliftConfig, Executable, NativeBackend, OptimizationLevel, ResumeStrategy,
};
use cyallo::{Context, Session, Status, compile};
use std::sync::Arc;

const CHAIN: &str = "fn square(n: i64) -> i64 { return n * n }\nfn cube(n: i64) -> i64 { let value = square(n); return value * n }\nproc scalar(n: i64) -> i64 { let value = n + 1; return cube(value) }\nlet mut total = 0\nfor i in 1..=20 { total += scalar(i) }\nstd::io::print(total)\n";
fn same(a: &Session, b: &Session) {
    assert_eq!(a.status(), b.status());
    assert_eq!(a.instruction_count(), b.instruction_count());
    assert_eq!(a.context.output, b.context.output);
    assert_eq!(a.stack(), b.stack());
    assert_eq!(a.variables(), b.variables());
    for fi in 0..a.program().functions().len() {
        assert_eq!(
            a.function_instruction_count(fi),
            b.function_instruction_count(fi),
            "work for {fi}"
        );
        assert_eq!(
            a.function_call_count(fi),
            b.function_call_count(fi),
            "calls for {fi}"
        );
    }
}
#[test]
fn nested_functions_and_procedures_inline_with_exact_logical_work() {
    let p = Arc::new(compile(CHAIN).unwrap());
    let mut reference = Session::new(p.clone()).unwrap();
    reference.run(100_000);
    assert_eq!(reference.status(), &Status::Finished);
    for optimization in [OptimizationLevel::None, OptimizationLevel::Speed] {
        for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
            for resume in [ResumeStrategy::AllPcs, ResumeStrategy::LoopHeaders] {
                let config = CraneliftConfig {
                    optimization,
                    scope,
                    resume,
                    ..Default::default()
                };
                let artifact = config.compile(p.clone()).unwrap();
                let mut s = Session::new(p.clone()).unwrap();
                artifact.run(&mut s, 100_000).unwrap();
                same(&reference, &s);
                assert!(s.inlined_call_count() >= 60, "{config:?}");
                let mut s = Session::new(p.clone()).unwrap();
                let off = CraneliftConfig {
                    inlining: false,
                    ..config
                }
                .compile(p.clone())
                .unwrap();
                off.run(&mut s, 100_000).unwrap();
                same(&reference, &s);
                assert_eq!(s.inlined_call_count(), 0);
            }
        }
    }
}
#[test]
fn every_budget_and_interpreter_handoff_preserves_canonical_frames() {
    let p = Arc::new(compile(CHAIN).unwrap());
    let artifact = CraneliftConfig::speed().compile(p.clone()).unwrap();
    for budget in 1..160 {
        let mut a = Session::new(p.clone()).unwrap();
        let mut b = Session::new(p.clone()).unwrap();
        a.run(budget);
        artifact.run(&mut b, budget).unwrap();
        same(&a, &b);
        for i in 0..10000 {
            a.run(17);
            if i % 2 == 0 {
                b.run(17);
            } else {
                artifact.run(&mut b, 17).unwrap();
            }
            same(&a, &b);
            if a.status() != &Status::Paused {
                break;
            }
        }
        assert_eq!(b.status(), &Status::Finished);
    }
}
#[test]
fn speculative_overflow_and_depth_limits_fall_back_before_commit() {
    for source in [
        "fn square(n: i64) -> i64 { return n * n }\nfn outer(n: i64) -> i64 { return square(n) + 1 }\nstd::io::print(outer(9223372036854775807))\n",
        "fn negate(n: i64) -> i64 { return -n }\nstd::io::print(negate(-9223372036854775808))\n",
        CHAIN,
    ] {
        let p = Arc::new(compile(source).unwrap());
        let artifact = CraneliftConfig::speed().compile(p.clone()).unwrap();
        for max_frames in 1..5 {
            let context = Context {
                max_frames,
                ..Default::default()
            };
            let mut a = Session::with_context(p.clone(), context.clone()).unwrap();
            let mut b = Session::with_context(p.clone(), context).unwrap();
            a.run(100_000);
            artifact.run(&mut b, 100_000).unwrap();
            same(&a, &b);
        }
    }
}
#[test]
fn reports_use_real_frames_and_dynamic_targets_remain_dynamic() {
    let source = "fn first(n: i64) -> i64 { return n + 1 }\nfn second(n: i64) -> i64 { return n + 2 }\nlet mut f = first\nstd::io::print(f(10))\nf = second\nstd::io::print(f(10))\n";
    let p = Arc::new(compile(source).unwrap());
    let artifact = CraneliftConfig::speed().compile(p.clone()).unwrap();
    for reporting in [false, true] {
        let mut s = Session::new(p.clone()).unwrap();
        if reporting {
            s.enable_reporting(std::time::Instant::now());
        }
        artifact.run(&mut s, 10000).unwrap();
        assert_eq!(s.context.output, "11\n12\n");
        assert_eq!(s.inlined_call_count(), 0);
    }
    let p = Arc::new(compile(CHAIN).unwrap());
    let artifact = CraneliftConfig::speed().compile(p.clone()).unwrap();
    let mut s = Session::new(p).unwrap();
    s.enable_reporting(std::time::Instant::now());
    artifact.run(&mut s, 100_000).unwrap();
    assert_eq!(s.inlined_call_count(), 0);
    assert_eq!(s.function_call_count(1), 20);
}
#[test]
fn reported_procedures_keep_caller_accounting_with_embedded_code_available() {
    let p = Arc::new(
        compile(
            "proc bump(n: &mut i64) { *n += 1 }\nlet mut n = 0\nfor i in 1..=3 { bump(&mut n) }\nstd::io::print(n)\n",
        )
        .unwrap(),
    );
    for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
        let config = CraneliftConfig {
            scope,
            procedure_inlining: true,
            ..CraneliftConfig::speed()
        };
        let artifact = config.compile(p.clone()).unwrap();
        let mut s = Session::new(p.clone()).unwrap();
        s.enable_reporting(std::time::Instant::now());
        artifact.run(&mut s, 1000).unwrap();
        assert_eq!(s.context.output, "3\n");
        assert_eq!(s.body_inlined_call_count(), 0);
        for (fi, metrics) in s.execution_report().unwrap().functions.iter().enumerate() {
            assert_eq!(
                metrics.instructions.iter().sum::<u64>(),
                s.function_instruction_count(fi)
            );
        }
    }
}

#[test]
fn effectful_procedure_bodies_keep_frames_aliases_gc_and_exact_once_effects() {
    let source = "proc bump(n: &mut i64) { *n += 1; std::io::print(*n) }\nproc edit(a: &mut std::collections::Vec<i64>) { let copy = [10,20]; a[0] += copy[1]; bump(&mut a[0]) }\nlet mut a = std::collections::Vec[1]\nedit(&mut a)\nstd::io::print(a[0])\n";
    let p = Arc::new(compile(source).unwrap());
    for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
        for resume in [ResumeStrategy::AllPcs, ResumeStrategy::LoopHeaders] {
            let config = CraneliftConfig {
                scope,
                resume,
                optimization: OptimizationLevel::Speed,
                procedure_inlining: true,
                ..Default::default()
            };
            let artifact = config.compile(p.clone()).unwrap();
            let mut whole = Session::with_context(
                p.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            artifact.run(&mut whole, 1000).unwrap();
            assert!(whole.body_inlined_call_count() >= 2, "{config:?}");
            for budget in 1..90 {
                let context = Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                };
                let mut a = Session::with_context(p.clone(), context.clone()).unwrap();
                let mut b = Session::with_context(p.clone(), context).unwrap();
                a.run(budget);
                artifact.run(&mut b, budget).unwrap();
                same(&a, &b);
                a.run(1000);
                artifact.run(&mut b, 1000).unwrap();
                same(&a, &b);
                assert_eq!(b.context.output, "22\n22\n");
                assert!(
                    b.body_inlined_call_count() > 0,
                    "{config:?}, budget {budget}"
                );
            }
        }
    }
}

#[test]
fn procedure_input_suspends_at_current_pc_without_replaying_output() {
    let p = Arc::new(compile("proc take_input(n: &mut i64) { std::io::print(1); std::runtime::caie::input(&mut *n); std::io::print(*n) }\nlet mut n = 0\ntake_input(&mut n)\n").unwrap());
    let artifact = CraneliftConfig {
        procedure_inlining: true,
        ..CraneliftConfig::speed()
    }
    .compile(p.clone())
    .unwrap();
    let mut a = Session::new(p.clone()).unwrap();
    let mut b = Session::new(p).unwrap();
    a.run(1000);
    artifact.run(&mut b, 1000).unwrap();
    same(&a, &b);
    assert_eq!(b.status(), &Status::WaitingForInput);
    assert_eq!(b.context.output, "1\n");
    a.context.input.push_back("42".into());
    b.context.input.push_back("42".into());
    a.run(1000);
    artifact.run(&mut b, 1000).unwrap();
    same(&a, &b);
    assert_eq!(b.context.output, "1\n42\n");
    assert!(b.body_inlined_call_count() > 0);
}
#[test]
fn semantic_replacement_requires_a_new_artifact_including_folded_dependencies() {
    let source =
        "const offset = 1\nfn add(n: i64) -> i64 { return n + offset }\nstd::io::print(add(10))\n";
    let old = Arc::new(compile(source).unwrap());
    let artifact = CraneliftConfig::speed().compile(old).unwrap();
    let replacement = Arc::new(compile(&source.replace("offset = 1", "offset = 2")).unwrap());
    let mut s = Session::new(replacement.clone()).unwrap();
    assert!(artifact.run(&mut s, 1000).is_err());
    assert_eq!(s.instruction_count(), 0);
    CraneliftConfig::speed()
        .compile(replacement)
        .unwrap()
        .run(&mut s, 1000)
        .unwrap();
    assert_eq!(s.context.output, "12\n");
    assert_eq!(s.inlined_call_count(), 1);
}

#[test]
fn procedure_errors_preserve_prior_effects_and_loop_continuations() {
    for source in [
        "proc work(n: &mut i64) { *n += 1; std::io::print(*n); let bad = 9223372036854775807 + *n }\nlet mut n = 0\nwork(&mut n)\n",
        "proc work(n: &mut i64) { while *n < 3 { *n += 1; std::io::print(*n) } }\nlet mut n = 0\nwork(&mut n)\n",
    ] {
        let p = Arc::new(compile(source).unwrap());
        let artifact = CraneliftConfig {
            procedure_inlining: true,
            ..CraneliftConfig::speed()
        }
        .compile(p.clone())
        .unwrap();
        for budget in 1..100 {
            let mut a = Session::new(p.clone()).unwrap();
            let mut b = Session::new(p.clone()).unwrap();
            a.run(budget);
            artifact.run(&mut b, budget).unwrap();
            same(&a, &b);
            a.run(1000);
            artifact.run(&mut b, 1000).unwrap();
            same(&a, &b);
            assert!(b.body_inlined_call_count() > 0);
        }
    }
}
