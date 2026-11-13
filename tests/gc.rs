use cyallo::runtime::{GcError, ResourceLimit, RuntimeError};
use cyallo::{
    Context, Session, Status,
    backend::{
        CompilationScope, CraneliftBackend, CraneliftConfig, CraneliftSpeedBackend, Executable,
        NativeBackend, ResumeStrategy, Tier, TierPolicy, TieredBackend,
    },
    compile,
    runtime::gc::GcConfig,
};
use std::{sync::Arc, time::Duration};
fn context() -> Context {
    Context {
        max_cells: 128,
        gc: GcConfig::stress(),
        ..Default::default()
    }
}
fn engines(p: &Arc<cyallo::ir::Program>) -> Vec<Box<dyn Executable>> {
    let baseline = TieredBackend::new(TierPolicy {
        hot_instructions: u64::MAX,
        warm_instructions: u64::MAX,
        ..Default::default()
    })
    .compile(p.clone())
    .unwrap();
    for fi in 0..p.functions().len() {
        baseline.precompile(fi, Tier::Winch).unwrap();
    }
    assert!(baseline.wait_for_compilation(Duration::from_secs(30)));
    vec![
        Box::new(baseline),
        Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
        Box::new(
            CraneliftConfig {
                scope: CompilationScope::Combined,
                resume: ResumeStrategy::LoopHeaders,
                ..CraneliftConfig::speed()
            }
            .compile(p.clone())
            .unwrap(),
        ),
        Box::new(
            CraneliftConfig {
                scope: CompilationScope::Combined,
                ..CraneliftConfig::speed()
            }
            .compile(p.clone())
            .unwrap(),
        ),
    ]
}
#[test]
fn recursive_managed_graphs_retain_live_cycles_and_collect_unreachable_cycles() {
    let program = Arc::new(compile(include_str!("../examples/gc-cycles.ya")).unwrap());
    for engine in std::iter::once(None).chain(engines(&program).into_iter().map(Some)) {
        let mut session = Session::with_context(program.clone(), context()).unwrap();
        for step in 0..100_000 {
            if let Some(engine) = &engine
                && step % 3 != 0
            {
                engine.run(&mut session, 1).unwrap();
            } else {
                session.run(1);
            }
            if session.status() != &Status::Paused {
                break;
            }
        }
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(
            session.context.output,
            "cycle: 10 -> 20 -> 10\ntemporary cycles: 128\n"
        );
        session.collect_garbage().unwrap();
        assert!(session.gc_stats().reclaimed_cells > 1000);
        assert!(session.heap_cell_count() < 128);
    }
}
#[test]
fn repeated_calls_collect_inside_construction_and_copy_helpers() {
    let p=Arc::new(compile("let mut saved = Vec[0]\nproc work(n: i64) -> i64 { let a = [n, n+1]; let b = a; saved[0] = b[1]; return b[0] + b[1] }\nlet mut total = 0\nfor i in 0..=200 { total += work(i) }\nstd::io::print(total)\n").unwrap());
    let mut reference = Session::new(p.clone()).unwrap();
    reference.run(100000);
    assert_eq!(reference.context.output, "40401\n");
    let mut interpreted = Session::with_context(p.clone(), context()).unwrap();
    interpreted.run(100000);
    assert_eq!(interpreted.status(), reference.status());
    assert_eq!(interpreted.context.output, reference.context.output);
    assert!(interpreted.gc_stats().reclaimed_cells > 1000);
    // Other tests keep live roots across movement; here all invocation temporaries die together.
    assert!(interpreted.heap_cell_count() < 128);
    for engine in engines(&p) {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), reference.status());
        assert_eq!(s.context.output, reference.context.output);
        assert_eq!(s.instruction_count(), reference.instruction_count());
        assert!(s.gc_stats().collections > 100);
        assert!(s.gc_stats().reclaimed_cells > 1000);
    }
}
#[test]
fn aliases_and_sparse_nested_values_survive_every_allocation() {
    let p = Arc::new(
        compile(
            r#"
proc change(a: &mut i64) { let b = a; let temp = [1,2,3]; *a += temp[0]; *b += 2 }
proc compute() -> i64 {
    let mut n = 0
    for i in 1..=20 { change(&mut n) }
    return n
}
let mut n = compute()
let mut Grid = [[n]]
let copy = std::mem::clone(&Grid)
Grid[0][0] = 999
std::io::print(n, ":", copy[0][0], ":", Grid[0][0])
"#,
        )
        .unwrap(),
    );
    for engine in engines(&p) {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "60:60:999\n");
        s.collect_garbage().unwrap();
        assert!(s.globals().iter().any(|(n, v)| n == "n" && v == "60"));
    }
}

#[test]
fn frame_scalars_run_with_a_zero_cell_budget() {
    let program = Arc::new(
        cyallo::compile_for_execution(
            r#"
fn sum(n: i64, acc: f64) -> f64 {
    if n == 0 { return acc }
    let subtotal = acc + (n as f64)
    return sum(n - 1, subtotal)
}
proc read_amount(base: f64) -> f64 {
    let mut value: f64 = 0.0
    std::runtime::caie::input(&mut value)
    return value + sum(4, base)
}
std::io::print(read_amount(2.0))
"#,
            Default::default(),
        )
        .unwrap(),
    );
    for engine in std::iter::once(None).chain(engines(&program).into_iter().map(Some)) {
        for gc in [GcConfig::default(), GcConfig::stress()] {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc,
                    max_cells: 0,
                    ..Context::default()
                },
            )
            .unwrap();
            let run_to_boundary = |session: &mut Session| {
                for _ in 0..1000 {
                    let before = session.instruction_count();
                    if let Some(engine) = &engine {
                        engine.run(session, 1).unwrap();
                    } else {
                        session.run(1);
                    }
                    assert!(session.instruction_count() - before <= 1);
                    if session.status() != &Status::Paused {
                        break;
                    }
                }
            };
            run_to_boundary(&mut session);
            assert_eq!(session.status(), &Status::WaitingForInput);
            let vars = session.variables();
            assert_eq!(
                vars,
                [("base".into(), "2".into()), ("value".into(), "0".into())]
            );
            if gc.enabled {
                session.collect_garbage().unwrap();
                assert_eq!(session.variables(), vars);
            }
            session.context.input.push_back("1.5".into());
            run_to_boundary(&mut session);
            assert_eq!(session.status(), &Status::Finished);
            assert_eq!(session.context.output, "13.5\n");
            assert_eq!(session.gc_stats().allocated_cells, 0);
        }
    }
}
#[test]
fn input_suspension_and_small_budgets_preserve_inspectable_frames() {
    let p = Arc::new(
        compile(
            "proc work() { let a = [7,8]; let mut n = 0; std::runtime::caie::input(&mut n); std::io::print(a[0], \":\", n) }\nwork()\n",
        )
        .unwrap(),
    );
    for (engine_index, engine) in engines(&p).into_iter().enumerate() {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        while s.status() != &Status::WaitingForInput {
            engine.run(&mut s, 1).unwrap();
        }
        let before = s.instruction_count();
        for _ in 0..3 {
            s.collect_garbage().unwrap();
        }
        assert_eq!(s.instruction_count(), before);
        assert!(
            s.variables()
                .iter()
                .any(|(n, v)| n == "a" && v.contains('7'))
        );
        s.context.input.push_back("42".into());
        for _ in 0..10 {
            engine.run(&mut s, 1000).unwrap();
            if s.status() == &Status::Finished {
                break;
            }
        }
        assert_eq!(
            s.status(),
            &Status::Finished,
            "engine={engine_index}; stack={:?}; output={:?}; cells={}",
            s.stack(),
            s.context.output,
            s.heap_cell_count()
        );
        assert_eq!(s.context.output, "7:42\n");
    }
}
#[test]
fn repl_replacement_keeps_global_roots_and_discards_old_frame_roots() {
    let mut repl = cyallo::repl::Repl::with_context(Default::default(), context()).unwrap();
    for line in [
        "let mut a = [1,2]",
        "fn work(n: i64) -> i64 { let temp = [n,n+1]; return temp[1] }",
        "for i in 1..=100 { a[0] = work(i) }",
        "std::io::print(a[0])",
    ] {
        repl.push_line(line).unwrap();
        repl.run(100000);
        assert_eq!(repl.session().status(), &Status::Finished);
        repl.session_mut().collect_garbage().unwrap();
    }
    assert_eq!(repl.session().context.output, "101\n");
    repl.reset().unwrap();
    repl.session_mut().collect_garbage().unwrap();
}

#[test]
fn host_roots_survive_movement_and_release_on_drop() {
    let p = Arc::new(compile("").unwrap());
    let mut s = Session::with_context(p.clone(), context()).unwrap();
    let mut gc = s.gc_scope();
    let dead = gc.allocate_integer(3).unwrap();
    let live = gc.allocate_integer(7).unwrap();
    drop(dead);
    gc.collect().unwrap();
    assert_eq!(gc.no_gc().get(&live).unwrap(), &cyallo::Value::Integer(7));
    assert_eq!(s.heap_cell_count(), 1);
    assert!(s.gc_stats().moved_cells > 0);
    let mut other = Session::with_context(p, context()).unwrap();
    assert_eq!(
        other.gc_scope().no_gc().get(&live).unwrap_err(),
        RuntimeError::Gc(GcError::ForeignRoot)
    );
    drop(live);
    s.collect_garbage().unwrap();
    assert_eq!(s.heap_cell_count(), 0);
}

#[test]
fn cell_limit_collects_before_failing_and_keeps_failed_locals_inspectable() {
    let p = Arc::new(
        compile(
            "let mut data = std::collections::Vec::from(0..0)\nfor i in 0..=100 { data.push(i) }\n",
        )
        .unwrap(),
    );
    for engine in engines(&p) {
        let mut s = Session::with_context(
            p.clone(),
            Context {
                max_cells: 16,
                gc: GcConfig {
                    enabled: true,
                    allocation_interval: usize::MAX,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert!(matches!(
            s.status(),
            Status::Failed(failure) if failure.error == RuntimeError::Limit(ResourceLimit::Storage)
        ));
        assert!(s.gc_stats().collections > 0);
        assert_eq!(s.heap_cell_count(), 16);
        let before = s.globals();
        s.collect_garbage().unwrap();
        assert_eq!(s.globals(), before);
    }
}

#[test]
fn metadata_distinguishes_integer_values_from_integer_locations() {
    use cyallo::{gc_metadata::ReferenceKind, ir::Op};
    let p = cyallo::compile_for_execution(
        "let mut a = std::collections::Vec::from(0..0)\nproc work(n: &mut i64) { *n += 1 }\na[10] = 7\nwork(&mut a[10])\n",
        Default::default(),
    )
    .unwrap();
    for f in p.functions() {
        assert_eq!(f.gc.instructions.len(), f.code.len());
        for (pc, inst) in f.code.iter().enumerate() {
            match inst.op {
                Op::Address(r, _) | Op::Index(r, ..) => {
                    assert_eq!(f.gc.registers[r], ReferenceKind::Location)
                }
                Op::Call(..) => {
                    assert!(f.gc.instructions[pc].may_collect && f.gc.instructions[pc].may_reenter)
                }
                _ => {}
            }
        }
    }
}

#[test]
fn dead_temporaries_release_replaced_array_cells_in_a_suspended_frame() {
    let p = Arc::new(compile("let mut a = std::collections::Vec[1,2,3]\nlet mut n = a[1]\na = std::collections::Vec::from(0..0)\nstd::runtime::caie::input(&mut n)\n").unwrap());
    let expected = p.functions()[0].locals.len();
    for engine in engines(&p) {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        for _ in 0..10 {
            engine.run(&mut s, 1000).unwrap();
            if s.status() == &Status::WaitingForInput {
                break;
            }
        }
        assert_eq!(s.status(), &Status::WaitingForInput);
        s.collect_garbage().unwrap();
        assert_eq!(s.heap_cell_count(), expected);
        assert!(
            s.variables()
                .iter()
                .any(|(name, value)| name == "n" && value == "2")
        );
    }
}

#[test]
fn native_tiers_keep_their_continuations_through_collection_and_budget_exits() {
    use cyallo::metrics::ExecutionEngine;
    let p=Arc::new(compile("fn work(n: i64) -> i64 { let a = [n,n+1]; return a[0]+a[1] }\nlet mut total = 0\nfor i in 0..=12 { total += work(i) }\nstd::io::print(total)\n").unwrap());
    let mut reference = Session::new(p.clone()).unwrap();
    reference.run(100000);
    for (engine, expected) in engines(&p).into_iter().zip([
        ExecutionEngine::Winch,
        ExecutionEngine::Cranelift,
        ExecutionEngine::CraneliftSpeed,
    ]) {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        s.enable_reporting(std::time::Instant::now());
        for _ in 0..10000 {
            let before = s.instruction_count();
            engine.run(&mut s, 1).unwrap();
            assert!(s.instruction_count() - before <= 1);
            s.collect_garbage().unwrap();
            if s.status() == &Status::Finished {
                break;
            }
        }
        assert_eq!(s.status(), reference.status());
        assert_eq!(s.context.output, reference.context.output);
        assert_eq!(s.instruction_count(), reference.instruction_count());
        let report = s.execution_report().unwrap();
        assert!(
            report
                .functions
                .iter()
                .any(|f| f.instructions[expected as usize] > 0)
        );
        assert!(
            report.functions.iter().all(|f| ExecutionEngine::ALL
                .into_iter()
                .filter(|&e| e != expected)
                .all(|e| f.instructions[e as usize] == 0)),
            "unexpected tier in {:?}",
            report.functions
        );
    }
}

#[test]
fn compatibility_dense_arrays_are_rooted_during_construction_and_byval_copy() {
    let p=Arc::new(cyallo::compile_with_options("DECLARE a : ARRAY[1:2] OF INTEGER\na[1] <- 10\na[2] <- 20\nPROCEDURE Change(BYREF x : INTEGER, BYREF y : INTEGER, BYVAL b : ARRAY[1:2] OF INTEGER)\n x <- x + 1\n y <- y + 2\n b[1] <- 999\nENDPROCEDURE\nDECLARE i : INTEGER\nFOR i <- 1 TO 30\n CALL Change(a[1], a[1], a)\nNEXT i\nOUTPUT a[1], \":\", a[2]\n",cyallo::CompileOptions{dialect:cyallo::Dialect::Compatibility,..Default::default()}).unwrap());
    let mut reference = Session::new(p.clone()).unwrap();
    reference.run(100000);
    for engine in engines(&p) {
        let mut s = Session::with_context(p.clone(), context()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), reference.status());
        assert_eq!(s.context.output, "100:20\n");
        assert_eq!(s.instruction_count(), reference.instruction_count());
        assert!(s.gc_stats().reclaimed_cells > 60);
    }
}
