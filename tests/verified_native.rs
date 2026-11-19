use cyallo::{
    Span,
    backend::verified::{
        AbiArgument, AbiLayout, Architecture, CodeStore, CodeVersion, CraneliftProfile, Dispatch,
        EntryPin, ExecutionMode, Failure, GuestRegister, ImageError, NativeTarget, NativeTier,
        Operation, Platform, PrepareError, PreparedFunction, PublishError, RecoverySlot,
        ValidatedImage,
    },
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, FORMAT_VERSION, FunctionId, FunctionImage, Instruction,
        InstructionId, ModuleImage, Register, RunError, Scalar, ScalarType, StatementArrival,
        StatementBoundary, VerifiedModule, encode,
    },
    compile,
};
use std::sync::Arc;

const VERSION: CodeVersion = CodeVersion(3);
const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 200,
    frames: 4,
};
const TIERS: [NativeTier; 3] = [
    NativeTier::Winch,
    NativeTier::Cranelift(CraneliftProfile::LowLatency),
    NativeTier::Cranelift(CraneliftProfile::Optimized),
];

fn verified(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    let index = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(&program, index).unwrap()
}

#[test]
fn scalar_branch_prepares_checked_homes_and_source_origins() {
    let source = "fn entry(take: bool, number: i64) -> i64 { if take { return number + 1 }; return number }\n";
    let module = verified(source);
    let target = NativeTarget {
        architecture: Architecture::Aarch64,
        platform: Platform::MacOs,
    };
    let plan = PreparedFunction::prepare(&module, module.entry(), VERSION, target).unwrap();
    assert_eq!(plan.target(), target);
    assert_eq!(plan.result(), Some(ScalarType::I64));
    assert_eq!(
        plan.arguments()
            .iter()
            .map(|home| home.ty)
            .collect::<Vec<_>>(),
        [ScalarType::Bool, ScalarType::I64]
    );
    assert_ne!(plan.arguments()[0].offset, plan.arguments()[1].offset);
    for home in plan.arguments() {
        assert_eq!(
            home.offset.0 % u32::try_from(plan.abi().slot_alignment).unwrap(),
            0
        );
        assert!(home.offset.0 < plan.frame_bytes());
    }
    let branch = plan
        .sites()
        .iter()
        .find(|site| matches!(site.operation, Operation::Branch { .. }))
        .unwrap();
    let Operation::Branch { condition, yes, no } = branch.operation else {
        unreachable!()
    };
    assert_eq!(condition.ty, ScalarType::Bool);
    assert_ne!(yes, no);
    assert!(plan.site(yes).is_some() && plan.site(no).is_some());
    let addition = plan
        .sites()
        .iter()
        .find(|site| matches!(site.operation, Operation::Integer { .. }))
        .unwrap();
    assert_eq!(addition.origin.function, module.entry());
    assert_eq!(addition.origin.version, VERSION);
    assert_eq!(addition.origin.span.source_text(source), "number + 1");
    assert!(matches!(
        PreparedFunction::prepare(&module, FunctionId(100), VERSION, target),
        Err(PrepareError::UnknownFunction(FunctionId(100)))
    ));
}

#[test]
fn scalar_abi_has_declared_host_layout_and_registers_on_admitted_targets() {
    let abi = AbiLayout::SCALARS;
    assert_eq!(
        (
            abi.context_bytes,
            abi.context_alignment,
            abi.slot_bytes,
            abi.slot_alignment
        ),
        (64, 8, 8, 8)
    );
    assert_eq!(
        (abi.slots, abi.budget, abi.next, abi.failure),
        (0, 8, 16, 24)
    );
    assert_eq!(
        (abi.links, abi.callee, abi.predecessor, abi.edge),
        (32, 40, 48, 56)
    );
    for platform in [Platform::MacOs, Platform::Linux] {
        for (architecture, expected) in [
            (
                Architecture::Aarch64,
                [GuestRegister::X0, GuestRegister::X1, GuestRegister::X2],
            ),
            (
                Architecture::X86_64,
                [GuestRegister::Rdi, GuestRegister::Rsi, GuestRegister::Rdx],
            ),
        ] {
            let target = NativeTarget {
                architecture,
                platform,
            };
            assert_eq!(
                [
                    AbiArgument::Context,
                    AbiArgument::Environment,
                    AbiArgument::ResultPlace
                ]
                .map(|argument| target.argument(argument)),
                expected
            );
        }
    }
}

fn publish(store: &mut CodeStore, id: FunctionId, tier: NativeTier) -> EntryPin {
    let plan = store.prepare(id).unwrap();
    store.publish(tier.emit(plan).unwrap()).unwrap()
}

fn callee(plan: &PreparedFunction) -> FunctionId {
    plan.sites()
        .iter()
        .find_map(|site| match site.operation {
            Operation::Call(pc) => plan.call(pc).map(|call| call.target.function),
            _ => None,
        })
        .unwrap()
}

#[test]
fn published_scalar_code_runs_every_tier_and_preserves_failures() {
    if NativeTarget::host().is_none() {
        return;
    }
    let source = "fn entry(take: bool, a: i64, b: i64) -> i64 { if take { return a / b + 1 }; return a % b }\n";
    let module = verified(source);
    let mut store = CodeStore::new(module.clone()).unwrap();
    for tier in TIERS {
        let pin = publish(&mut store, module.entry(), tier);
        let division = pin
            .plan()
            .sites()
            .iter()
            .find(|site| site.failures == [Failure::DivisionByZero, Failure::Overflow])
            .unwrap();
        let address = pin.machine_address(division.origin.instruction).unwrap();
        assert_eq!(pin.origin_at(address), Some(division.origin));
        assert_eq!(division.origin.span.source_text(source), "a / b");
        assert_eq!(pin.origin_at(usize::MAX), None);
        for (branch, a, b, expected) in [
            (true, 41, 2, Some(21)),
            (false, -5, 2, Some(-1)),
            (false, i64::MIN, -1, Some(0)),
            (true, 9, 0, None),
            (true, i64::MIN, -1, None),
            (true, i64::MAX, 1, None),
        ] {
            let args = [Scalar::Bool(branch), Scalar::I64(a), Scalar::I64(b)];
            let mut native = pin.start(&args, LIMITS).unwrap();
            let mut reference = module.start(&args, LIMITS).unwrap();
            loop {
                let actual = native.resume(1).unwrap();
                assert_eq!(actual, reference.resume(1), "{tier:?}, {args:?}");
                assert_eq!(native.last_entry().unwrap().tier, tier);
                assert_eq!(
                    native.instructions_executed(),
                    reference.instructions_executed()
                );
                if !matches!(actual, ExecutionStatus::Paused(_)) {
                    match expected {
                        Some(value) => {
                            assert_eq!(actual, ExecutionStatus::Returned(Some(Scalar::I64(value))))
                        }
                        None if b == 0 => assert!(
                            matches!(actual, ExecutionStatus::Failed(RunError::DivisionByZero { span, .. }) if span.source_text(source) == "a / b")
                        ),
                        None => assert!(matches!(
                            actual,
                            ExecutionStatus::Failed(RunError::IntegerOverflow { .. })
                        )),
                    }
                    assert_eq!(native.resume(5).unwrap(), actual);
                    break;
                }
            }
        }
        let args = [Scalar::Bool(true), Scalar::I64(41), Scalar::I64(2)];
        let mut native = pin.start(&args, LIMITS).unwrap();
        assert_eq!(
            native.resume(LIMITS.fuel).unwrap(),
            ExecutionStatus::Returned(Some(Scalar::I64(21)))
        );
    }
}

#[test]
fn stale_and_foreign_code_is_rejected_before_publication() {
    if NativeTarget::host().is_none() {
        return;
    }
    let module = verified("fn entry(value: i64) -> i64 { return value + 1 }\n");
    let mut store = CodeStore::new(module.clone()).unwrap();
    let mut foreign = CodeStore::new(module.clone()).unwrap();
    let root = module.entry();
    let plan = store.prepare(root).unwrap();
    let foreign_plan = foreign.prepare(root).unwrap();
    assert!(matches!(
        store.publish(NativeTier::Winch.emit(foreign_plan).unwrap()),
        Err(PublishError::Image(ImageError::WrongPlan))
    ));
    assert!(store.entry(root).is_none());
    let old = store
        .publish(NativeTier::Winch.emit(plan.clone()).unwrap())
        .unwrap();
    let stale = NativeTier::Winch.emit(plan).unwrap();
    store.replace(module).unwrap();
    assert!(matches!(
        store.publish(stale),
        Err(PublishError::StaleVersion { .. })
    ));
    assert!(store.entry(root).is_none());
    let mut native = old.start(&[Scalar::I64(41)], LIMITS).unwrap();
    assert_eq!(
        native.resume(100).unwrap(),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
}

#[test]
fn mixed_scalar_callee_recovers_both_frames_and_exact_fuel() {
    if NativeTarget::host().is_none() {
        return;
    }
    let source = "fn leaf(a: i64, b: i64) -> i64 { return (a / b) * 2 }\nfn entry(a: i64, b: i64) -> i64 { let saved = a + 3; return leaf(a, b) + saved }\n";
    let module = verified(source);
    let optimized = NativeTier::Cranelift(CraneliftProfile::Optimized);
    for (caller_tier, callee_tier) in [
        (NativeTier::Winch, optimized),
        (optimized, NativeTier::Winch),
    ] {
        let mut store = CodeStore::new(module.clone()).unwrap();
        let root = module.entry();
        let root_plan = store.prepare(root).unwrap();
        let child = callee(&root_plan);
        assert!(matches!(
            store.publish(caller_tier.emit(root_plan).unwrap()),
            Err(PublishError::MissingDependency(_))
        ));
        assert!(store.entry(root).is_none());
        publish(&mut store, child, callee_tier);
        let root_pin = publish(&mut store, root, caller_tier);
        let args = [Scalar::I64(40), Scalar::I64(2)];
        let mut native = root_pin.start(&args, LIMITS).unwrap();
        let mut reference = module.start(&args, LIMITS).unwrap();
        let mut saw_caller = false;
        loop {
            let status = native.resume(1).unwrap();
            assert_eq!(status, reference.resume(1));
            let event = native.last_entry().unwrap();
            if event.origin.function == root {
                assert_eq!(event.tier, caller_tier);
                saw_caller = true;
            }
            let ExecutionStatus::Paused(position) = status else {
                panic!("native returned before pausing inside callee")
            };
            if position.function == child && event.origin.function == child {
                assert_eq!(event.tier, callee_tier);
                break;
            }
        }
        assert!(saw_caller);
        store.retire(child);
        assert!(store.entry(child).is_none());
        let mut recovered = native.recover().unwrap();
        let mut saw_return = false;
        loop {
            assert_eq!(recovered.status(), reference.status());
            assert_eq!(
                recovered.instructions_executed(),
                reference.instructions_executed()
            );
            assert!(
                recovered
                    .statement_boundaries()
                    .eq(reference.statement_boundaries())
            );
            let status = recovered.resume(1);
            assert_eq!(status, reference.resume(1));
            if matches!(status, ExecutionStatus::Paused(position) if position.function == root) {
                saw_return = true;
            }
            if !matches!(status, ExecutionStatus::Paused(_)) {
                assert_eq!(status, ExecutionStatus::Returned(Some(Scalar::I64(83))));
                break;
            }
        }
        assert!(saw_return);
        let mut limited = root_pin
            .start(
                &args,
                ExecutionLimits {
                    frames: 1,
                    ..LIMITS
                },
            )
            .unwrap();
        assert_eq!(
            limited.resume(100).unwrap(),
            ExecutionStatus::Failed(RunError::FrameLimit)
        );
    }
}

#[test]
fn a_retired_transitive_callee_stays_executable_through_the_caller_pin() {
    if NativeTarget::host().is_none() {
        return;
    }
    let module = verified(
        "fn leaf(a: i64) -> i64 { return a + 1 }\nfn middle(a: i64) -> i64 { return leaf(a) + 1 }\nfn entry(a: i64) -> i64 { return middle(a) + 1 }\n",
    );
    let mut store = CodeStore::new(module.clone()).unwrap();
    let root = module.entry();
    let root_plan = store.prepare(root).unwrap();
    let middle = callee(&root_plan);
    let middle_plan = store.prepare(middle).unwrap();
    let leaf = callee(&middle_plan);
    publish(
        &mut store,
        leaf,
        NativeTier::Cranelift(CraneliftProfile::Optimized),
    );
    publish(&mut store, middle, NativeTier::Winch);
    let root_pin = publish(
        &mut store,
        root,
        NativeTier::Cranelift(CraneliftProfile::LowLatency),
    );
    let args = [Scalar::I64(39)];
    let Dispatch::Interpreter(mut interpreted) = store
        .start(&args, LIMITS, ExecutionMode::InterpreterOnly)
        .unwrap()
    else {
        panic!("native-disabled execution must use verified bytecode")
    };
    assert_eq!(
        interpreted.resume(100),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
    let Dispatch::Native(mut native) = store
        .start(&args, LIMITS, ExecutionMode::PreferNative)
        .unwrap()
    else {
        panic!("a published root must enter native code")
    };
    assert!(matches!(
        native.resume(1).unwrap(),
        ExecutionStatus::Paused(_)
    ));
    for function in [root, middle, leaf] {
        store.retire(function);
    }
    assert!(matches!(
        store
            .start(&args, LIMITS, ExecutionMode::PreferNative)
            .unwrap(),
        Dispatch::Interpreter(_)
    ));
    drop(root_pin);
    drop(store);
    let mut saw_leaf = false;
    while let ExecutionStatus::Paused(_) = native.resume(1).unwrap() {
        saw_leaf |= native
            .last_entry()
            .is_some_and(|event| event.origin.function == leaf);
    }
    assert!(saw_leaf);
    assert_eq!(
        native.status().unwrap(),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
}

#[test]
fn loop_fuel_and_recovery_are_stable_at_different_slice_sizes() {
    if NativeTarget::host().is_none() {
        return;
    }
    let module = verified(
        "fn entry(limit: i64) -> i64 { let mut count = 0; while count < limit { count += 1 }; return count }\n",
    );
    let args = [Scalar::I64(4)];
    let mut store = CodeStore::new(module.clone()).unwrap();
    for tier in TIERS {
        let entry = publish(&mut store, module.entry(), tier);
        for (limits, expected) in [
            (LIMITS, ExecutionStatus::Returned(Some(Scalar::I64(4)))),
            (
                ExecutionLimits { fuel: 7, ..LIMITS },
                ExecutionStatus::Failed(RunError::FuelExhausted),
            ),
        ] {
            for budget in [1, 3, limits.fuel] {
                let mut native = entry.start(&args, limits).unwrap();
                let mut reference = module.start(&args, limits).unwrap();
                loop {
                    let status = native.resume(budget).unwrap();
                    assert_eq!(status, reference.resume(budget));
                    assert_eq!(
                        native.instructions_executed(),
                        reference.instructions_executed()
                    );
                    if !matches!(status, ExecutionStatus::Paused(_)) {
                        assert_eq!(status, expected);
                        break;
                    }
                }
            }
        }
    }
}

#[test]
fn native_ineligible_loans_execute_via_verified_dispatch() {
    if NativeTarget::host().is_none() {
        return;
    }
    let module = verified(
        "fn entry(input: i64) -> i64 { let owner = input + 1; let view = &owner; return view.* }\n",
    );
    let mut store = CodeStore::new(module.clone()).unwrap();
    assert!(matches!(
        store.prepare(module.entry()),
        Err(PrepareError::NonScalar(_))
    ));
    let args = [Scalar::I64(41)];
    let mut run = store
        .start(&args, LIMITS, ExecutionMode::PreferNative)
        .unwrap();
    assert!(matches!(run, Dispatch::Interpreter(_)));
    assert_eq!(
        run.resume(100).unwrap(),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
}

#[test]
fn recovery_preserves_the_taken_statement_edge_at_a_shared_pc() {
    if NativeTarget::host().is_none() {
        return;
    }
    let branch = InstructionId(0);
    let joined = InstructionId(1);
    let condition = Register(0);
    let true_span = Span::new(1, 2);
    let false_span = Span::new(2, 3);
    let module = ModuleImage {
        version: FORMAT_VERSION,
        entry: FunctionId(0),
        shapes: Box::new([]),
        signatures: Box::new([]),
        globals: Box::new([]),
        managed: Box::new([]),
        traits: Box::new([]),
        implementations: Box::new([]),
        functions: Box::new([FunctionImage {
            contracts: Box::new([]),
            generator: None,
            registers: Box::new([ScalarType::Bool.into()]),
            parameters: Box::new([condition]),
            result: Some(ScalarType::Bool.into()),
            calls: Box::new([]),
            lists: Box::new([]),
            texts: Box::new([]),
            outputs: Box::new([]),
            words: encode([
                Instruction::Branch(condition, joined, joined),
                Instruction::Return(Some(condition)),
            ]),
            spans: Box::new([Span::default(); 2]),
            statements: Box::new([
                StatementBoundary {
                    instruction: joined,
                    span: true_span,
                    arrival: Some(StatementArrival::Branch {
                        from: branch,
                        taken: true,
                    }),
                },
                StatementBoundary {
                    instruction: joined,
                    span: false_span,
                    arrival: Some(StatementArrival::Branch {
                        from: branch,
                        taken: false,
                    }),
                },
            ]),
        }]),
    }
    .verify()
    .unwrap();
    let mut store = CodeStore::new(module.clone()).unwrap();
    for tier in TIERS {
        let entry = publish(&mut store, module.entry(), tier);
        for (taken, span) in [(true, true_span), (false, false_span)] {
            let mut native = entry.start(&[Scalar::Bool(taken)], LIMITS).unwrap();
            assert!(
                matches!(native.resume(1).unwrap(), ExecutionStatus::Paused(position) if position.instruction == joined)
            );
            let mut recovered = native.recover().unwrap();
            assert_eq!(
                recovered
                    .statement_boundaries()
                    .map(|mark| mark.span)
                    .collect::<Vec<_>>(),
                [span]
            );
            assert_eq!(
                recovered.resume(1),
                ExecutionStatus::Returned(Some(Scalar::Bool(taken)))
            );
        }
    }
}

#[test]
fn prepared_scalar_call_keeps_destination_successor_and_checked_failure() {
    let source = "fn divide(a: i64, b: i64) -> i64 { return a / b }\nfn entry(a: i64, b: i64) -> i64 { return divide(a, b) + 1 }\n";
    let module = verified(source);
    let target = NativeTarget {
        architecture: Architecture::X86_64,
        platform: Platform::Linux,
    };
    let plan = PreparedFunction::prepare(&module, module.entry(), VERSION, target).unwrap();
    let (site, id) = plan
        .sites()
        .iter()
        .find_map(|site| match site.operation {
            Operation::Call(id) => Some((site, id)),
            _ => None,
        })
        .unwrap();
    let call = plan.call(id).unwrap();
    assert_eq!(call.target.version, VERSION);
    assert_eq!(Some(call.successor), site.successor);
    assert_eq!(site.fuel, 1);
    for argument in &call.arguments {
        assert!(
            site.recover
                .contains(&RecoverySlot::Initialized(argument.caller))
        );
    }
    let destination = call.destination.unwrap();
    assert!(
        site.recover
            .contains(&RecoverySlot::Uninitialized(destination))
    );
    let next = plan.site(call.successor).unwrap();
    assert!(
        next.recover
            .contains(&RecoverySlot::Initialized(destination))
    );
    let callee =
        PreparedFunction::prepare(&module, call.target.function, call.target.version, target)
            .unwrap();
    for (argument, parameter) in call.arguments.iter().zip(callee.arguments()) {
        assert_eq!(argument.callee, *parameter);
    }
    let failure = callee
        .sites()
        .iter()
        .find(|site| !site.failures.is_empty())
        .unwrap();
    assert_eq!(failure.origin.span.source_text(source), "a / b");
    assert_eq!(
        failure.failures,
        [Failure::DivisionByZero, Failure::Overflow]
    );
}

#[test]
fn both_adapters_emit_the_same_versioned_plan_with_final_machine_ranges() {
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let source = "fn divide(a: i64, b: i64) -> i64 { return a / b }\nfn entry(take: bool, a: i64, b: i64) -> i64 { if take { return divide(a, b) + 1 }; return a % b }\n";
    let module = verified(source);
    let caller =
        Arc::new(PreparedFunction::prepare(&module, module.entry(), VERSION, target).unwrap());
    let callee = caller
        .sites()
        .iter()
        .find_map(|site| match site.operation {
            Operation::Call(id) => caller.call(id).map(|call| call.target),
            _ => None,
        })
        .unwrap();
    let callee = Arc::new(
        PreparedFunction::prepare(&module, callee.function, callee.version, target).unwrap(),
    );
    for plan in [caller, callee] {
        for tier in [
            NativeTier::Winch,
            NativeTier::Cranelift(CraneliftProfile::LowLatency),
            NativeTier::Cranelift(CraneliftProfile::Optimized),
        ] {
            let image = tier.emit(plan.clone()).unwrap();
            let again = tier.emit(plan.clone()).unwrap();
            assert_eq!(image.tier(), tier);
            assert!(!image.bytes().is_empty());
            assert_eq!(image.bytes(), again.bytes());
            assert_eq!(image.ranges(), again.ranges());
            assert_eq!(image.relocations(), again.relocations());
            assert_eq!(
                image.cranelift_machine_info(),
                again.cranelift_machine_info()
            );
            if let NativeTier::Cranelift(_) = tier {
                let machine = image.cranelift_machine_info().unwrap();
                assert!(!machine.blocks.is_empty());
                assert!(!machine.edges.is_empty());
            }
            assert_eq!(image.plan().version(), VERSION);
            for range in image.ranges() {
                assert!(range.start < range.end);
                assert!(usize::try_from(range.end).unwrap() <= image.bytes().len());
                assert!(plan.site(range.instruction).is_some());
            }
            for site in plan.sites() {
                assert!(
                    image
                        .ranges()
                        .iter()
                        .any(|range| range.instruction == site.origin.instruction),
                    "{tier:?} omitted {:?}",
                    site.origin.instruction
                );
            }
            let image = ValidatedImage::validate(&plan, image).unwrap();
            for range in image.image().ranges() {
                let origin = image.origin_at(range.start).unwrap();
                assert_eq!(origin, plan.site(range.instruction).unwrap().origin);
            }
        }
    }
}
