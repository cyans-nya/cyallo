use cyallo::bytecode::verified::{
    CallTarget, ExecutionLimits, ExecutionStatus, HostFailure, Instruction, RunError, Scalar,
    ScalarSdk, ScalarType, SdkCompileError, VerifiedModule,
};
use cyallo_ir::{
    contract::{Disposition, Effects, Operand, OperandModes, Safepoint},
    registry::{BindingContract, ReferenceLinkError, Registry},
    sdk::Digest,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = include_str!("fixtures/sdk_host.ya");
const ITEM: &str = "api::tick";
const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 200,
    frames: 2,
};
const CONTRACT: BindingContract = BindingContract {
    disposition: Disposition::HostCall,
    safepoint: Safepoint::Never,
    effects: Effects {
        writes_memory: true,
        may_fail: true,
        ..Effects::NONE
    },
    operands: OperandModes {
        receiver: Operand::Read,
        rest: Operand::Read,
        exceptions: &[],
    },
};

fn digest() -> Digest {
    let sdk = cyallo_ir::namespace::single(cyallo_parser::parse(SOURCE).unwrap())
        .unwrap()
        .interface;
    sdk.item(ITEM).unwrap().digest
}

fn install(
    entry: &str,
    contract: BindingContract,
    callback: impl Fn(&[Scalar]) -> Result<Scalar, HostFailure> + Send + Sync + 'static,
) -> Result<VerifiedModule, SdkCompileError> {
    let sdk = ScalarSdk::default().host_call(ITEM, digest(), contract, callback);
    VerifiedModule::lower_sdk(SOURCE, entry, sdk)
}

#[test]
fn installed_bound_call_commits_once_between_budget_pauses() {
    let first_count = Arc::new(AtomicUsize::new(0));
    let captured = Arc::clone(&first_count);
    let first = install("entry", CONTRACT, move |args| {
        assert_eq!(args, &[Scalar::I64(21)]);
        captured.fetch_add(1, Ordering::Relaxed);
        Ok(Scalar::I64(41))
    })
    .unwrap();
    let second = install("entry", CONTRACT, |_| Ok(Scalar::I64(42))).unwrap();
    let local = install("local", CONTRACT, |_| panic!("local call reached host")).unwrap();
    assert_eq!(
        second.run(&[Scalar::I64(21)], LIMITS),
        Ok(Some(Scalar::I64(43)))
    );
    assert_eq!(
        local.run(&[Scalar::I64(21)], LIMITS),
        Ok(Some(Scalar::I64(21)))
    );

    let mut activation = first.start(&[Scalar::I64(21)], LIMITS).unwrap();
    let body = first.function(first.entry()).unwrap();
    let call_position = loop {
        let ExecutionStatus::Paused(position) = activation.status() else {
            panic!("expected to reach the bound call")
        };
        if let Instruction::Call(site) = body.instructions()[position.instruction.0 as usize] {
            assert!(matches!(
                body.calls()[site.0 as usize].target,
                CallTarget::Bound(_)
            ));
            break position;
        }
        activation.resume(1);
    };
    assert!(first.disassemble().contains("call.bound"));
    assert_eq!(call_position.span.source_text(SOURCE), "host_tick(value)");
    assert_eq!(activation.resume(0), ExecutionStatus::Paused(call_position));
    assert_eq!(first_count.load(Ordering::Relaxed), 0);
    let before = activation.instructions_executed();
    let ExecutionStatus::Paused(next) = activation.resume(1) else {
        panic!("expected a pause after committing the bound call")
    };
    assert_eq!(next.function, call_position.function);
    assert_eq!(next.instruction.0, call_position.instruction.0 + 1);
    assert_eq!(activation.instructions_executed(), before + 1);
    assert_eq!(first_count.load(Ordering::Relaxed), 1);
    let returned = ExecutionStatus::Returned(Some(Scalar::I64(42)));
    while let ExecutionStatus::Paused(_) = activation.resume(1) {}
    assert_eq!(activation.resume(1), returned);
    assert_eq!(first_count.load(Ordering::Relaxed), 1);
    assert_eq!(
        first.run(
            &[Scalar::I64(21)],
            ExecutionLimits {
                fuel: before,
                ..LIMITS
            }
        ),
        Err(RunError::FuelExhausted),
    );
    assert_eq!(first_count.load(Ordering::Relaxed), 1);
}

#[test]
fn bound_call_failure_is_terminal_and_keeps_the_call_site() {
    let count = Arc::new(AtomicUsize::new(0));
    let captured = Arc::clone(&count);
    let module = install("entry", CONTRACT, move |_| {
        captured.fetch_add(1, Ordering::Relaxed);
        Err(HostFailure::Rejected)
    })
    .unwrap();
    let mut activation = module.start(&[Scalar::I64(21)], LIMITS).unwrap();
    let status = loop {
        match activation.resume(1) {
            ExecutionStatus::Paused(_) => continue,
            terminal => break terminal,
        }
    };
    let ExecutionStatus::Failed(RunError::BoundCallFailed {
        position,
        cause: HostFailure::Rejected,
        ..
    }) = status
    else {
        panic!("expected the bound call failure, got {status:?}")
    };
    assert_eq!(position.span.source_text(SOURCE), "host_tick(value)");
    assert_eq!(activation.resume(LIMITS.fuel), status);
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

#[test]
fn discarded_bound_result_still_runs_and_validates_the_callback() {
    for result in [
        Ok(Scalar::I64(41)),
        Ok(Scalar::Bool(true)),
        Err(HostFailure::Rejected),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&count);
        let module = install("discard", CONTRACT, move |args| {
            assert_eq!(args, &[Scalar::I64(21)]);
            captured.fetch_add(1, Ordering::Relaxed);
            result
        })
        .unwrap();
        let body = module.function(module.entry()).unwrap();
        let [call] = body.calls() else {
            panic!("expected one call")
        };
        assert!(matches!(call.target, CallTarget::Bound(_)));
        assert_eq!(call.destination, None);
        let mut activation = module.start(&[Scalar::I64(21)], LIMITS).unwrap();
        while let ExecutionStatus::Paused(_) = activation.resume(1) {}
        let status = activation.status();
        match result {
            Ok(Scalar::I64(_)) => {
                assert_eq!(status, ExecutionStatus::Returned(Some(Scalar::I64(21))))
            }
            Ok(Scalar::Bool(_) | Scalar::F64(_) | Scalar::U64(_)) => assert!(matches!(
                status,
                ExecutionStatus::Failed(RunError::BoundResultType { .. })
            )),
            Err(HostFailure::Rejected) => assert!(matches!(
                status,
                ExecutionStatus::Failed(RunError::BoundCallFailed {
                    cause: HostFailure::Rejected,
                    ..
                })
            )),
        }
        assert_eq!(activation.resume(1), status);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn bound_call_rejects_contract_and_callback_mismatches() {
    let missing = Registry::new().host_call(ITEM, digest(), CONTRACT);
    assert!(matches!(
        VerifiedModule::lower_sdk_reference(SOURCE, "entry", missing),
        Err(SdkCompileError::MissingHostCallback(_))
    ));
    let parking = BindingContract {
        safepoint: Safepoint::MayReenter,
        effects: Effects {
            may_park: true,
            ..CONTRACT.effects
        },
        ..CONTRACT
    };
    assert!(matches!(
        install("entry", parking, |_| panic!(
            "invalid contract reached host"
        )),
        Err(SdkCompileError::Link(
            ReferenceLinkError::UnsupportedContract(_)
        ))
    ));
    let bad_result = install("entry", CONTRACT, |_| Ok(Scalar::Bool(false))).unwrap();
    assert!(matches!(
        bad_result.run(&[Scalar::I64(21)], LIMITS),
        Err(RunError::BoundResultType {
            expected: ScalarType::I64,
            actual: ScalarType::Bool,
            ..
        })
    ));
    let infallible = BindingContract {
        effects: Effects {
            may_fail: false,
            ..CONTRACT.effects
        },
        ..CONTRACT
    };
    let rejected = install("entry", infallible, |_| Err(HostFailure::Rejected)).unwrap();
    assert!(matches!(
        rejected.run(&[Scalar::I64(21)], LIMITS),
        Err(RunError::UnexpectedBoundFailure {
            cause: HostFailure::Rejected,
            ..
        })
    ));
    let panicking = install("entry", CONTRACT, |_| panic!("fixture host panic")).unwrap();
    let mut activation = panicking.start(&[Scalar::I64(21)], LIMITS).unwrap();
    let status = activation.resume(LIMITS.fuel);
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::BoundCallPanicked { .. })
    ));
    assert_eq!(activation.resume(1), status);
}
