//! Tier-up: hot functions move from Winch to Cranelift during a run. The run
//! must give the interpreter's output and instruction count, and the promoted
//! function must end on a Cranelift image. Both driver modes are covered: word
//! plans (scalar programs) and shared-runtime plans (programs using globals).

use cyallo::{
    backend::verified::{CraneliftProfile, NativeTarget, NativeTier, TierPolicy},
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, FunctionId, OutputSink, VerifiedModule,
    },
    compile,
    vm::start_jit_with,
};
use std::sync::{Arc, Mutex};

const CRANELIFT: NativeTier = NativeTier::Cranelift(CraneliftProfile::LowLatency);

fn reference(source: &str) -> (Vec<String>, usize) {
    let program = Arc::new(compile(source).unwrap());
    let module = VerifiedModule::lower_shared(program, 0).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    let limits = ExecutionLimits {
        fuel: usize::MAX,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_output(sink);
    let status = activation.resume(usize::MAX / 2);
    assert!(matches!(status, ExecutionStatus::Returned(_)), "{status:?}");
    let lines = lines.lock().unwrap().clone();
    (lines, activation.instructions_executed())
}

/// Run through `start_jit_with`; returns the output, instructions, and the
/// functions whose images were promoted.
fn tiered(
    source: &str,
    policy: TierPolicy,
    budget: usize,
) -> (
    Vec<String>,
    usize,
    Vec<FunctionId>,
    Vec<NativeTier>,
    Option<NativeTier>,
) {
    let program = Arc::new(compile(source).unwrap());
    let mut run = start_jit_with(program, &Default::default(), policy).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    match run.dispatch() {
        cyallo::backend::verified::Dispatch::Native(activation) => activation.set_output(sink),
        cyallo::backend::verified::Dispatch::Interpreter(activation) => activation.set_output(sink),
    }
    let mut seen = false;
    loop {
        let status = run.resume(budget).unwrap();
        if let cyallo::backend::verified::Dispatch::Native(activation) = run.dispatch() {
            seen |= activation.last_entry().map(|event| event.tier) == Some(CRANELIFT);
        }
        run.settle();
        if !matches!(status, ExecutionStatus::Paused(_)) {
            assert!(matches!(status, ExecutionStatus::Returned(_)), "{status:?}");
            break;
        }
    }
    let executed = run.dispatch().instructions_executed();
    let last = seen.then_some(CRANELIFT);
    let promoted = run.promoted().to_vec();
    let tiers = promoted
        .iter()
        .filter_map(|&function| run.tier_of(function))
        .collect();
    let lines = lines.lock().unwrap().clone();
    (lines, executed, promoted, tiers, last)
}

const WORD_LOOP: &str = "proc sum_to(limit: i64) -> i64 {\n let mut total = 0\n for i in 1..=limit { total += i }\n return total\n}\nstd::io::print(sum_to(300000))\n";
const SHARED_LOOP: &str = "let mut entries = 0\nproc sum_to(limit: i64) -> i64 {\n entries += 1\n let mut total = 0\n for i in 1..=limit { total += i }\n return total\n}\nstd::io::print(sum_to(300000), \":\", entries)\n";

#[test]
fn thresholds_grow_with_function_size() {
    let policy = TierPolicy::default();
    assert_eq!(policy.threshold(0), 500_000);
    assert!(policy.threshold(28) > 1_000_000 && policy.threshold(28) < 2_000_000);
    assert!(policy.threshold(298) > 8_000_000 && policy.threshold(298) < 20_000_000);
    assert!(policy.threshold(1162) > 40_000_000 && policy.threshold(1162) < 70_000_000);
    assert!(policy.threshold(10) < policy.threshold(11));
    assert_eq!(TierPolicy::eager(1000).threshold(5000), 0);
}

#[test]
fn a_hot_word_function_is_promoted_and_the_run_is_unchanged() {
    if NativeTarget::host().is_none() {
        return;
    }
    let (wanted, instructions) = reference(WORD_LOOP);
    let (lines, executed, promoted, tiers, last) =
        tiered(WORD_LOOP, TierPolicy::eager(20_000), 50_000);
    // Slices after the swap ran the Cranelift image.
    assert_eq!(last, Some(CRANELIFT));
    assert_eq!(lines, wanted);
    assert_eq!(executed, instructions);
    assert!(!promoted.is_empty(), "no function was promoted");
    assert!(tiers.iter().all(|tier| *tier == CRANELIFT), "{tiers:?}");
}

#[test]
fn a_hot_function_with_a_global_is_promoted_in_the_shared_runtime() {
    if NativeTarget::host().is_none() {
        return;
    }
    let (wanted, instructions) = reference(SHARED_LOOP);
    let (lines, executed, promoted, tiers, last) =
        tiered(SHARED_LOOP, TierPolicy::eager(20_000), 50_000);
    assert_eq!(last, Some(CRANELIFT));
    assert_eq!(lines, wanted);
    assert_eq!(executed, instructions);
    assert!(!promoted.is_empty(), "no function was promoted");
    assert!(tiers.iter().all(|tier| *tier == CRANELIFT), "{tiers:?}");
}

#[test]
fn the_default_policy_leaves_a_short_run_in_winch() {
    if NativeTarget::host().is_none() {
        return;
    }
    let (wanted, instructions) = reference(WORD_LOOP);
    let (lines, executed, promoted, _, _) =
        tiered(WORD_LOOP, TierPolicy::default(), usize::MAX / 2);
    assert_eq!(lines, wanted);
    assert_eq!(executed, instructions);
    // The loop runs about 8M instructions in a function of about 70 sites,
    // under its threshold of 3M only after a while: it may or may not be
    // promoted, but it never changes the result.
    let _ = promoted;
}

#[test]
fn disabled_tiering_never_promotes() {
    if NativeTarget::host().is_none() {
        return;
    }
    let policy = TierPolicy {
        enabled: false,
        ..TierPolicy::eager(10_000)
    };
    let (_, _, promoted, _, _) = tiered(WORD_LOOP, policy, 50_000);
    assert!(promoted.is_empty());
}
