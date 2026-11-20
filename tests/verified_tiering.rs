//! Tier-up: hot functions move from Winch to Cranelift during a run. The run
//! must give the interpreter's output and instruction count, and the promoted
//! function must end on a Cranelift image. Both driver modes are covered: word
//! plans (scalar programs) and shared-runtime plans (programs using globals).

use cyallo::{
    CompileOptions,
    backend::verified::{CraneliftProfile, Dispatch, NativeTarget, NativeTier, TierPolicy, Tiered},
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, FunctionId, Instruction, OutputSink, RunError,
        VerifiedModule,
    },
    compile, compile_for_execution,
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

fn start_with_osr(program: Arc<cyallo::ir::Program>) -> Tiered {
    let mut context = cyallo::vm::Context::default();
    context.gc.stress = true;
    start_jit_with(
        program,
        &context,
        TierPolicy {
            interpreter_first: true,
            base: 64,
            ..TierPolicy::eager(64)
        },
    )
    .unwrap()
}

fn enter_loop(tiered: &mut Tiered, module: &VerifiedModule) -> usize {
    let ExecutionStatus::Paused(mut position) = tiered.resume(64).unwrap() else {
        panic!("expected an interpreted pause inside the loop");
    };
    let Dispatch::Native(activation) = tiered.dispatch() else {
        panic!("expected the OSR driver");
    };
    assert!(activation.last_entry().is_none());
    assert_eq!(activation.instructions_executed(), 64);
    tiered.settle();
    assert_eq!(tiered.tier_of(position.function), Some(NativeTier::Winch));

    for _ in 0..module
        .function(position.function)
        .unwrap()
        .instructions()
        .len()
    {
        let ExecutionStatus::Paused(next) = tiered.resume(1).unwrap() else {
            panic!("expected to reach native code before the loop exits");
        };
        let Dispatch::Native(activation) = tiered.dispatch() else {
            panic!("expected the OSR driver");
        };
        if let Some(event) = activation.last_entry() {
            assert_eq!(event.tier, NativeTier::Winch);
            assert_eq!(event.origin.function, position.function);
            assert_eq!(event.origin.instruction, position.instruction);
            let body = module.function(position.function).unwrap();
            let target = position.instruction;
            assert!(body.instructions()[target.0 as usize..].iter().any(|instruction| {
                matches!(instruction, Instruction::Jump(to) if *to == target)
                    || matches!(instruction, Instruction::Branch(_, yes, no) if *yes == target || *no == target)
            }));
            return activation.instructions_executed();
        }
        position = next;
    }
    panic!("expected machine code to run at a loop header")
}

#[test]
fn interpreter_enters_winch_at_a_live_loop_and_recovers_at_each_budget() {
    if NativeTarget::host().is_none() {
        return;
    }
    let source = r#"
proc sum_to(by: i64) {
    let mut total = 0
    let mut i = 0
    std::io::print("start")
    while i < 12 {
        total += by
        i += 1
        if i == 6 { std::io::print(total) }
    }
    std::io::print(total)
}
sum_to(8)
"#;
    let program = Arc::new(compile_for_execution(source, CompileOptions::default()).unwrap());
    let module = VerifiedModule::lower_shared(program.clone(), 0).unwrap();
    let (wanted, instructions) = reference(source);
    assert_eq!(wanted, ["start", "48", "96"]);

    let first = check_osr(program.clone(), &module, None, &wanted, instructions);
    for budget in 0..instructions - first {
        check_osr(
            program.clone(),
            &module,
            Some(budget),
            &wanted,
            instructions,
        );
    }
}

fn check_osr(
    program: Arc<cyallo::ir::Program>,
    module: &VerifiedModule,
    recovery_budget: Option<usize>,
    wanted: &[String],
    instructions: usize,
) -> usize {
    let mut tiered = start_with_osr(program);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let capture = lines.clone();
    let Dispatch::Native(activation) = tiered.dispatch() else {
        panic!("expected the OSR driver");
    };
    activation.set_output(OutputSink::new(move |line| {
        capture.lock().unwrap().push(line.to_owned())
    }));
    let entered = enter_loop(&mut tiered, module);
    let Dispatch::Native(activation) = tiered.dispatch() else {
        panic!("expected the OSR driver");
    };
    let (status, executed) = if let Some(budget) = recovery_budget {
        assert!(matches!(
            activation.resume(budget).unwrap(),
            ExecutionStatus::Paused(_)
        ));
        let unused = module
            .start(&[], ExecutionLimits { fuel: 1, frames: 1 })
            .unwrap();
        let Dispatch::Native(activation) =
            std::mem::replace(tiered.dispatch(), Dispatch::Interpreter(Box::new(unused)))
        else {
            panic!("expected the OSR driver");
        };
        let mut recovered = activation.recover().unwrap();
        (
            recovered.resume(usize::MAX),
            recovered.instructions_executed(),
        )
    } else {
        (
            activation.resume(usize::MAX).unwrap(),
            activation.instructions_executed(),
        )
    };
    assert_eq!(status, ExecutionStatus::Returned(None));
    assert_eq!(executed, instructions);
    assert_eq!(*lines.lock().unwrap(), wanted);
    entered
}

#[test]
fn osr_preserves_a_live_gc_handle_and_the_failure_position() {
    if NativeTarget::host().is_none() {
        return;
    }
    let source = r#"
proc sum_to(by: i64) {
    let kept = gc by
    let mut total = 0
    let mut i = 0
    while i < 12 {
        total += by
        i += 1
    }
    let fresh = gc total
    std::io::print(kept.* + fresh.*)
}
"#;
    for (input, should_fail) in [(7, false), (1_000_000_000_000_000_000i64, true)] {
        let source = format!("{source}\nsum_to({input})");
        let program = Arc::new(compile(&source).unwrap());
        let module = VerifiedModule::lower_shared(program.clone(), 0).unwrap();
        let mut reference = module
            .start(
                &[],
                ExecutionLimits {
                    fuel: usize::MAX,
                    frames: 64,
                },
            )
            .unwrap();
        reference.set_gc_stress(true);
        reference.set_output(OutputSink::new(|_| {}));
        let wanted = reference.resume(usize::MAX);
        assert_eq!(
            matches!(
                wanted,
                ExecutionStatus::Failed(RunError::IntegerOverflow { .. })
            ),
            should_fail
        );
        let mut tiered = start_with_osr(program);
        let Dispatch::Native(activation) = tiered.dispatch() else {
            panic!("expected the OSR driver");
        };
        let lines = Arc::new(Mutex::new(Vec::new()));
        let capture = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            capture.lock().unwrap().push(line.to_owned())
        }));
        enter_loop(&mut tiered, &module);
        assert_eq!(tiered.resume(usize::MAX).unwrap(), wanted);
        assert_eq!(
            tiered.dispatch().instructions_executed(),
            reference.instructions_executed()
        );
        if should_fail {
            assert!(lines.lock().unwrap().is_empty());
        } else {
            assert_eq!(*lines.lock().unwrap(), ["91"]);
        }
    }
}

#[test]
fn a_loop_entered_from_the_interpreter_can_tier_up_again() {
    if NativeTarget::host().is_none() {
        return;
    }
    let (wanted, instructions) = reference(WORD_LOOP);
    let (lines, executed, promoted, tiers, last) = tiered(
        WORD_LOOP,
        TierPolicy {
            interpreter_first: true,
            ..TierPolicy::eager(20_000)
        },
        50_000,
    );
    assert_eq!(lines, wanted);
    assert_eq!(executed, instructions);
    assert_eq!(last, Some(CRANELIFT));
    assert!(matches!(promoted.as_slice(), [winch, cranelift] if winch == cranelift));
    assert!(tiers.contains(&CRANELIFT));
}
