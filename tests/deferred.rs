use cyallo::runtime::RuntimeError;
use cyallo::{
    CompileOptions, Session, Status,
    backend::{Executable, NativeBackend, TierPolicy, TieredBackend},
    compile, compile_for_execution,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn unused_bodies_remain_unchecked_but_check_reports_them() {
    let source = "fn unused() -> i64 { return missing }\nstd::io::print(42)\n";
    let expected = compile(source).unwrap_err();
    let program = Arc::new(compile_for_execution(source, CompileOptions::default()).unwrap());
    assert!(program.functions()[0].is_analyzed());
    assert!(!program.functions()[1].is_analyzed());
    let jit = TieredBackend::default().compile(program.clone()).unwrap();
    let mut s = Session::new(program.clone()).unwrap();
    jit.run(&mut s, 1000).unwrap();
    assert_eq!(s.context.output, "42\n");
    assert_eq!(s.status(), &Status::Finished);
    assert!(!program.functions()[1].is_analyzed());
    assert_eq!(program.validate_all().unwrap_err(), expected);
}
#[test]
fn deferred_errors_keep_the_original_diagnostic_before_body_effects() {
    for source in [
        "proc bad() -> i64 { std::io::print(99); return missing }\nstd::io::print(bad())\n",
        "fn bad() -> i64 { std::io::print(99); return 1 }\nstd::io::print(bad())\n",
        "fn bad() -> i64 { return true }\nstd::io::print(bad())\n",
        "fn bad() -> i64 { let x = 1 }\nstd::io::print(bad())\n",
    ] {
        let expected = compile(source).unwrap_err();
        let program = Arc::new(compile_for_execution(source, CompileOptions::default()).unwrap());
        let jit = TieredBackend::default().compile(program.clone()).unwrap();
        let mut s = Session::new(program).unwrap();
        jit.run(&mut s, 1000).unwrap();
        let Status::Failed(failure) = s.status() else {
            panic!("expected a deferred check failure: {source}")
        };
        assert_eq!(failure.error, RuntimeError::DeferredCheck(expected.clone()));
        assert_eq!(failure.diagnostic(), expected);
        assert!(s.context.output.is_empty());
    }
}
#[test]
fn declaration_errors_are_eager_and_analysis_is_shared_between_workers() {
    assert!(
        compile_for_execution(
            "fn a(x: i64, x: i64) -> i64 { return x }\n",
            CompileOptions::default()
        )
        .is_err()
    );
    let p = Arc::new(compile_for_execution("fn work(n: i64) -> i64 { let mut i = 0; while i < n { i += 1 }; return i }\nstd::io::print(work(10))\n",CompileOptions::default()).unwrap());
    assert!(p.functions()[1].eager_candidate);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| p.functions()[1].checked().unwrap() as *const _ as usize);
        let b = scope.spawn(|| p.functions()[1].checked().unwrap() as *const _ as usize);
        assert_eq!(a.join().unwrap(), b.join().unwrap());
    });
}
#[test]
fn small_loop_gets_compiled_before_its_first_body_instruction() {
    let p=Arc::new(compile_for_execution("fn work(n: i64) -> i64 { let mut i = 0; while i < n { i += 1 }; return i }\nfn cold() -> i64 { return missing }\nstd::io::print(work(2000))\n",CompileOptions::default()).unwrap());
    let jit = TieredBackend::new(TierPolicy {
        first_call_wait: Duration::from_secs(5),
        hot_instructions: u64::MAX,
        warm_instructions: 0,
        ..Default::default()
    })
    .compile(p.clone())
    .unwrap();
    let mut s = Session::new(p.clone()).unwrap();
    s.enable_reporting(Instant::now());
    jit.run(&mut s, 100000).unwrap();
    assert_eq!(s.context.output, "2000\n");
    let report = s.execution_report().unwrap();
    assert_eq!(report.functions[1].instructions[0], 0);
    assert_eq!(report.first_call_admissions, 1);
    assert!(!p.functions()[2].is_analyzed());
}
