use cyallo::{
    CompileOptions, Session, Status,
    backend::{
        EagerDecision, EagerPolicy, Executable, NativeBackend, Tier, TierPolicy, TieredBackend,
    },
    compile_for_execution,
};
use cyallo_ast::CompileHint;
use std::{sync::Arc, time::Duration};

#[test]
fn eager_window_is_half_open_and_force_retains_a_hard_limit() {
    let p = EagerPolicy::default();
    for (words, allowed) in [(15, false), (16, true), (127, true), (128, false)] {
        assert_eq!(p.size_reason(words, CompileHint::Auto).is_none(), allowed);
        assert_eq!(p.size_reason(words, CompileHint::Eager).is_none(), allowed);
    }
    assert!(p.size_reason(1, CompileHint::AlwaysEager).is_none());
    assert!(p.size_reason(4096, CompileHint::AlwaysEager).is_none());
    assert!(p.size_reason(4097, CompileHint::AlwaysEager).is_some());
    for hint in [CompileHint::Lazy, CompileHint::AlwaysLazy] {
        assert!(p.size_reason(32, hint).is_some());
    }
}

#[test]
fn call_soon_coalesces_and_lazy_remains_eligible_for_observed_tiering() {
    let source = "#[opt(always_eager)]\nfn early(n: i64) -> i64 { return n + 1 }\n#[opt(lazy)]\nfn later(n: i64) -> i64 { return n + 2 }\n#[opt(always_lazy)]\nfn interpreted(n: i64) -> i64 { return n + 3 }\nstd::io::print(early(later(interpreted(1))))\n";
    let p = Arc::new(compile_for_execution(source, Default::default()).unwrap());
    let jit = TieredBackend::default().compile(p.clone()).unwrap();
    assert_eq!(
        jit.hint_call_soon(1).unwrap(),
        EagerDecision::AlreadyRequested
    );
    assert!(matches!(
        jit.hint_call_soon(2).unwrap(),
        EagerDecision::Deferred(_)
    ));
    assert!(matches!(
        jit.hint_call_soon(3).unwrap(),
        EagerDecision::Deferred(_)
    ));
    assert!(jit.hint_call_soon(99).is_err());
    jit.precompile(2, Tier::CraneliftFast).unwrap();
    assert!(jit.wait_for_compilation(Duration::from_secs(30)));
    let tiers = jit.tiers();
    assert!(tiers[1].tier.is_some());
    assert_eq!(tiers[2].tier, Some(Tier::CraneliftFast));
    assert!(tiers[3].tier.is_none());
    let mut s = Session::new(p).unwrap();
    jit.run(&mut s, 1000).unwrap();
    assert_eq!(s.context.output, "7\n");
}

#[test]
fn speculative_work_is_bounded_and_lowered_rejections_wake_waiters() {
    let source = "fn work(n: i64) -> i64 { let mut i = 0; while i < n { i += 1 }; return i }\nstd::io::print(work(2))\n";
    let p = Arc::new(compile_for_execution(source, CompileOptions::default()).unwrap());
    // Deliberately admit the syntax estimate while rejecting the actual bytecode.
    let estimate = p.functions()[1].estimated_words;
    let actual = p.functions()[1].checked().unwrap().code.len();
    assert_ne!(estimate, actual);
    // Use another deferred Program, so the worker performs the final size check.
    let p = Arc::new(compile_for_execution(source, Default::default()).unwrap());
    let eager = EagerPolicy {
        min_words: estimate,
        max_words: estimate + 1,
        ..Default::default()
    };
    let jit = TieredBackend::new(TierPolicy {
        eager,
        ..Default::default()
    })
    .compile(p.clone())
    .unwrap();
    let _ = jit.hint_call_soon(1).unwrap();
    assert!(jit.wait_for_compilation(Duration::from_secs(30)));
    assert!(jit.tiers()[1].tier.is_none());
    jit.precompile(1, Tier::Winch).unwrap();
    assert!(jit.wait_for_compilation(Duration::from_secs(30)));
    assert!(jit.tiers()[1].tier.is_some());
    let eager = EagerPolicy {
        max_total_words: 0,
        ..Default::default()
    };
    let jit = TieredBackend::new(TierPolicy {
        eager,
        ..Default::default()
    })
    .compile(p.clone())
    .unwrap();
    assert!(matches!(
        jit.hint_call_soon(1).unwrap(),
        EagerDecision::Deferred(_)
    ));
}

#[test]
fn eager_semantic_errors_wait_until_body_entry() {
    let source = "#[opt(always_eager)]\nproc bad() { std::io::print(99); std::io::print(missing) }\nif false { bad() }\nstd::io::print(42)\n";
    let p = Arc::new(compile_for_execution(source, Default::default()).unwrap());
    let jit = TieredBackend::default().compile(p.clone()).unwrap();
    assert!(jit.wait_for_compilation(Duration::from_secs(30)));
    let mut s = Session::new(p).unwrap();
    jit.run(&mut s, 1000).unwrap();
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "42\n");
}

#[test]
fn syntax_lookahead_does_not_treat_shadowed_callables_as_direct_calls() {
    let source = "fn target(n: i64) -> i64 { return missing }\nproc caller(target: fn(i64) -> i64) -> i64 { return target(1) }\nstd::io::print(42)\n";
    let p = compile_for_execution(source, Default::default()).unwrap();
    assert!(p.functions()[2].direct_calls.is_empty());
    assert!(!p.functions()[1].is_analyzed());
}
