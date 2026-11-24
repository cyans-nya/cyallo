use cyallo::{
    Jobs, Span,
    bytecode::verified::{
        CallSite, CallSiteId, CallTarget, ExecutionLimits, ExecutionStatus, FORMAT_VERSION,
        FunctionId, FunctionImage, HostBindingId, Instruction, InstructionId, IntegerOp,
        InvalidBytecode, ModuleImage, Register, RunError, Scalar, ScalarType, StatementArrival,
        StatementBoundary, VerifiedModule, encode,
    },
    compile, compile_snapshot,
};
use std::sync::Arc;

fn image(
    registers: &[ScalarType],
    parameters: &[Register],
    result: Option<ScalarType>,
    instructions: &[Instruction],
) -> FunctionImage {
    FunctionImage {
        locals: Box::new([]),
        frozen: Box::new([]),
        contracts: Box::new([]),
        generator: None,
        registers: registers.iter().copied().map(Into::into).collect(),
        parameters: parameters.into(),
        result: result.map(Into::into),
        calls: Box::new([]),
        lists: Box::new([]),
        texts: Box::new([]),
        outputs: Box::new([]),
        words: encode(instructions.iter().copied()),
        spans: vec![Span::default(); instructions.len()].into_boxed_slice(),
        statements: Box::new([]),
    }
}

fn module(functions: Vec<FunctionImage>) -> ModuleImage {
    ModuleImage {
        version: FORMAT_VERSION,
        entry: FunctionId(0),
        shapes: Box::new([]),
        signatures: Box::new([]),
        globals: Box::new([]),
        managed: Box::new([]),
        traits: Box::new([]),
        implementations: Box::new([]),
        functions: functions.into_boxed_slice(),
    }
}

#[test]
fn checked_source_branches_and_calls_with_span_on_overflow() {
    let source = concat!(
        "fn increment(n: i64) -> i64 { return n + 1 }\n",
        "fn choose(use_increment: bool, n: i64) -> i64 {\n",
        "  if use_increment { return increment(n) }\n",
        "  return n\n",
        "}\n",
    );
    let snapshot = cyallo_parser::parse_modern_snapshot(Arc::from(source), false, Jobs(1)).unwrap();
    let program = compile_snapshot(Arc::new(snapshot), Default::default()).unwrap();
    let id = program
        .functions()
        .iter()
        .position(|function| function.name.as_ref() == "choose")
        .unwrap();
    let verified = VerifiedModule::lower(&program, id).unwrap();
    let limits = ExecutionLimits {
        fuel: 100,
        frames: 2,
    };
    assert_eq!(
        verified.start(&[], limits).unwrap_err(),
        RunError::ArgumentCount {
            expected: 2,
            actual: 0
        }
    );
    assert_eq!(
        verified
            .start(&[Scalar::I64(1), Scalar::I64(41)], limits)
            .unwrap_err(),
        RunError::ArgumentType {
            index: 0,
            expected: ScalarType::Bool,
            actual: ScalarType::I64
        }
    );
    assert_eq!(
        verified.run(&[Scalar::Bool(true), Scalar::I64(41)], limits),
        Ok(Some(Scalar::I64(42)))
    );
    assert_eq!(
        verified.run(&[Scalar::Bool(false), Scalar::I64(i64::MAX)], limits),
        Ok(Some(Scalar::I64(i64::MAX)))
    );
    let failure = verified
        .run(&[Scalar::Bool(true), Scalar::I64(i64::MAX)], limits)
        .unwrap_err();
    let RunError::IntegerOverflow {
        function,
        instruction,
        span,
    } = failure
    else {
        panic!("expected checked overflow, got {failure:?}");
    };
    assert_ne!(function, verified.entry());
    assert_eq!(
        verified.function(function).unwrap().spans()[instruction.0 as usize],
        span
    );
    assert_eq!(span.source_text(source), "n + 1");
    assert_eq!(
        verified.run(
            &[Scalar::Bool(true), Scalar::I64(41)],
            ExecutionLimits {
                fuel: limits.fuel,
                frames: 1
            }
        ),
        Err(RunError::FrameLimit)
    );

    for (input, expected) in [
        (41, ExecutionStatus::Returned(Some(Scalar::I64(42)))),
        (i64::MAX, ExecutionStatus::Failed(failure)),
    ] {
        let arguments = [Scalar::Bool(true), Scalar::I64(input)];
        let mut uninterrupted = verified.start(&arguments, limits).unwrap();
        assert_eq!(uninterrupted.resume(limits.fuel), expected);

        let mut sliced = verified.start(&arguments, limits).unwrap();
        let entry = sliced.status();
        assert_eq!(sliced.resume(0), entry);
        assert_eq!(sliced.instructions_executed(), 0);
        let mut entered_callee = false;
        for step in 1..=uninterrupted.instructions_executed() {
            let status = sliced.resume(1);
            assert_eq!(sliced.instructions_executed(), step);
            if let ExecutionStatus::Paused(position) = status {
                entered_callee |= position.function == function;
                assert_eq!(
                    verified.function(position.function).unwrap().spans()
                        [position.instruction.0 as usize],
                    position.span,
                );
            } else {
                assert_eq!(step, uninterrupted.instructions_executed());
            }
        }
        assert!(entered_callee);
        assert_eq!(sliced.status(), expected);
        assert_eq!(sliced.resume(1), expected);
        assert_eq!(
            sliced.instructions_executed(),
            uninterrupted.instructions_executed()
        );
    }
}

#[test]
fn verification_rejects_truncated_instruction() {
    let mut function = image(
        &[ScalarType::I64],
        &[],
        Some(ScalarType::I64),
        &[
            Instruction::Constant(Register(0), Scalar::I64(i64::MIN)),
            Instruction::Return(Some(Register(0))),
        ],
    );
    function.words = function.words[..function.words.len() - 1].into();
    let error = module(vec![function]).verify().unwrap_err();
    assert!(matches!(
        error.kind,
        InvalidBytecode::TruncatedInstruction { .. }
    ));
}

#[test]
fn verification_rejects_the_retired_nop_opcode() {
    let mut body = image(&[], &[], None, &[Instruction::Return(None)]);
    body.words = Box::new([1 << u16::BITS]);
    let image = module(vec![body]);
    let mut v3 = image.clone();
    v3.version = 3;
    assert_eq!(v3.verify().unwrap_err().kind, InvalidBytecode::Version(3));
    let error = image.verify().unwrap_err();
    assert_eq!(
        error.kind,
        InvalidBytecode::UnknownOpcode { word: 0, opcode: 0 }
    );
}

#[test]
fn verification_rejects_invalid_statement_metadata() {
    let span = Span::default();
    let mark = |instruction, arrival| StatementBoundary {
        instruction,
        span,
        arrival,
    };
    let entry = InstructionId(0);
    let next = InstructionId(1);
    for (markers, expected) in [
        (
            vec![mark(InstructionId(2), None)],
            InvalidBytecode::StatementTarget(InstructionId(2)),
        ),
        (
            vec![mark(next, Some(StatementArrival::Entry))],
            InvalidBytecode::StatementEdge(StatementArrival::Entry),
        ),
        (
            vec![mark(next, None), mark(entry, None)],
            InvalidBytecode::StatementOrder(entry),
        ),
    ] {
        let mut body = image(
            &[],
            &[],
            None,
            &[Instruction::Jump(next), Instruction::Return(None)],
        );
        body.statements = markers.into_boxed_slice();
        let error = module(vec![body]).verify().unwrap_err();
        assert_eq!(error.kind, expected);
    }
}

#[test]
fn verification_intersects_initialization_at_joins() {
    let function = image(
        &[ScalarType::Bool, ScalarType::I64],
        &[Register(0)],
        Some(ScalarType::I64),
        &[
            Instruction::StorageLive(Register(1)),
            Instruction::Branch(Register(0), InstructionId(4), InstructionId(2)),
            Instruction::Constant(Register(1), Scalar::I64(12)),
            Instruction::Jump(InstructionId(5)),
            Instruction::Jump(InstructionId(5)),
            Instruction::StorageDead(Register(0)),
            Instruction::Return(Some(Register(1))),
        ],
    );
    let error = module(vec![function]).verify().unwrap_err();
    assert_eq!(error.instruction, Some(InstructionId(6)));
    assert_eq!(error.kind, InvalidBytecode::Uninitialized(Register(1)));
}

#[test]
fn verifier_accepts_a_discarded_scalar_call_result() {
    let mut caller = image(
        &[],
        &[],
        None,
        &[Instruction::Call(CallSiteId(0)), Instruction::Return(None)],
    );
    caller.calls = vec![CallSite {
        target: CallTarget::Direct(FunctionId(1)),
        arguments: Box::new([]),
        destination: None,
    }]
    .into_boxed_slice();
    let callee = image(
        &[ScalarType::I64],
        &[],
        Some(ScalarType::I64),
        &[
            Instruction::StorageLive(Register(0)),
            Instruction::Constant(Register(0), Scalar::I64(12)),
            Instruction::Return(Some(Register(0))),
        ],
    );
    let module = module(vec![caller, callee]).verify().unwrap();
    let limits = ExecutionLimits { fuel: 5, frames: 2 };
    let mut activation = module.start(&[], limits).unwrap();
    for expected in [FunctionId(1), FunctionId(1), FunctionId(1), FunctionId(0)] {
        assert!(
            matches!(activation.resume(1), ExecutionStatus::Paused(position) if position.function == expected)
        );
    }
    assert_eq!(activation.resume(1), ExecutionStatus::Returned(None));
    assert_eq!(activation.instructions_executed(), limits.fuel);
}

#[test]
fn verifier_rejects_a_destination_for_a_void_call() {
    let destination = Register(0);
    let mut caller = image(
        &[ScalarType::I64],
        &[],
        Some(ScalarType::I64),
        &[
            Instruction::StorageLive(destination),
            Instruction::Call(CallSiteId(0)),
            Instruction::Return(Some(destination)),
        ],
    );
    caller.calls = Box::new([CallSite {
        target: CallTarget::Direct(FunctionId(1)),
        arguments: Box::new([]),
        destination: Some(destination),
    }]);
    let callee = image(&[], &[], None, &[Instruction::Return(None)]);
    let error = module(vec![caller, callee]).verify().unwrap_err();
    assert_eq!(error.function, Some(FunctionId(0)));
    assert_eq!(error.kind, InvalidBytecode::ResultPresence);
}

#[test]
fn checked_source_discards_a_scalar_result_and_reports_callee_failure() {
    let source = concat!(
        "proc calculate(value: i64) -> i64 { return value + 1 }\n",
        "proc discard(value: i64) -> i64 { calculate(value); return value }\n",
    );
    let program = compile(source).unwrap();
    let entry = program
        .functions()
        .iter()
        .position(|function| function.name.as_ref() == "discard")
        .unwrap();
    let module = VerifiedModule::lower(&program, entry).unwrap();
    let body = module.function(module.entry()).unwrap();
    let [call] = body.calls() else {
        panic!("expected one call")
    };
    assert!(matches!(call.target, CallTarget::Direct(_)));
    assert_eq!(call.destination, None);
    let limits = ExecutionLimits {
        fuel: 100,
        frames: 2,
    };
    assert_eq!(
        module.run(&[Scalar::I64(21)], limits),
        Ok(Some(Scalar::I64(21)))
    );
    let Err(RunError::IntegerOverflow { function, span, .. }) =
        module.run(&[Scalar::I64(i64::MAX)], limits)
    else {
        panic!("expected overflow in the discarded call")
    };
    assert_ne!(function, module.entry());
    assert_eq!(span.source_text(source), "value + 1");
}

#[test]
fn verifier_rejects_host_id_without_an_installed_table() {
    let mut caller = image(
        &[],
        &[],
        None,
        &[Instruction::Call(CallSiteId(0)), Instruction::Return(None)],
    );
    let binding = HostBindingId(0);
    caller.calls = Box::new([CallSite {
        target: CallTarget::Bound(binding),
        arguments: Box::new([]),
        destination: None,
    }]);
    let error = module(vec![caller]).verify().unwrap_err();
    assert_eq!(error.kind, InvalidBytecode::UnknownHostBinding(binding));
}

#[test]
fn reuse_clears_initialization_and_rejects_invalid_lifetime_transitions() {
    let slot = Register(0);
    let other = Register(1);
    use Instruction::*;
    for (instructions, expected) in [
        (
            vec![Constant(slot, Scalar::I64(1)), Trap],
            InvalidBytecode::DeadStorageAccess(slot),
        ),
        (
            vec![StorageDead(slot), Trap],
            InvalidBytecode::StorageAlreadyDead(slot),
        ),
        (
            vec![StorageLive(slot), StorageLive(slot), Trap],
            InvalidBytecode::StorageAlreadyLive(slot),
        ),
        (
            vec![
                StorageLive(slot),
                Constant(slot, Scalar::I64(1)),
                StorageDead(slot),
                StorageLive(slot),
                Return(Some(slot)),
            ],
            InvalidBytecode::Uninitialized(slot),
        ),
        (
            vec![
                StorageLive(slot),
                Constant(slot, Scalar::I64(1)),
                StorageLive(other),
                Return(Some(slot)),
            ],
            InvalidBytecode::LiveStorageAtReturn(other),
        ),
    ] {
        let body = image(
            &[ScalarType::I64; 2],
            &[],
            Some(ScalarType::I64),
            &instructions,
        );
        assert_eq!(module(vec![body]).verify().unwrap_err().kind, expected);
    }

    let body = image(
        &[ScalarType::I64],
        &[],
        Some(ScalarType::I64),
        &[
            StorageLive(slot),
            Constant(slot, Scalar::I64(1)),
            StorageDead(slot),
            StorageLive(slot),
            Constant(slot, Scalar::I64(2)),
            Return(Some(slot)),
        ],
    );
    let program = module(vec![body]).verify().unwrap();
    assert_eq!(
        program.run(&[], ExecutionLimits { fuel: 6, frames: 1 }),
        Ok(Some(Scalar::I64(2)))
    );
}

#[test]
fn verifier_requires_agreement_on_live_storage_at_a_join() {
    use Instruction::*;
    let condition = Register(0);
    let slot = Register(1);
    let body = image(
        &[ScalarType::Bool, ScalarType::I64],
        &[condition],
        None,
        &[
            Branch(condition, InstructionId(1), InstructionId(3)),
            StorageLive(slot),
            Jump(InstructionId(3)),
            Trap,
        ],
    );
    let error = module(vec![body]).verify().unwrap_err();
    assert_eq!(error.instruction, Some(InstructionId(3)));
    assert_eq!(error.kind, InvalidBytecode::StorageJoin(slot));
}

#[test]
fn frame_placement_reuses_dead_slots_across_scalar_types() {
    use Instruction::*;
    use ScalarType::*;
    let first = Register(0);
    let second = Register(1);
    let overlap = Register(2);
    let function = image(
        &[Bool, I64, Bool],
        &[],
        Some(I64),
        &[
            StorageLive(first),
            Constant(first, Scalar::Bool(false)),
            StorageDead(first),
            StorageLive(second),
            Constant(second, Scalar::I64(42)),
            StorageLive(overlap),
            Constant(overlap, Scalar::Bool(true)),
            StorageDead(overlap),
            Return(Some(second)),
        ],
    );
    let program = module(vec![function]).verify().unwrap();
    let verified = program.function(program.entry()).unwrap();
    let frame = verified.frame_layout();
    assert_eq!(frame.slot_count(), 2);
    assert_eq!(frame.slot(first), frame.slot(second));
    assert_ne!(frame.slot(second), frame.slot(overlap));
    assert_eq!(
        program.run(&[], ExecutionLimits { fuel: 9, frames: 1 }),
        Ok(Some(Scalar::I64(42)))
    );
}

#[test]
fn frame_placement_accounts_for_live_ins_after_backward_jumps() {
    use Instruction::*;
    let value = Register(0);
    let temporary = Register(1);
    let unreachable = Register(2);
    let function = image(
        &[ScalarType::I64; 3],
        &[],
        Some(ScalarType::I64),
        &[
            Jump(InstructionId(8)),
            StorageLive(temporary),
            Constant(temporary, Scalar::I64(2)),
            Integer(IntegerOp::Add, value, value, temporary),
            StorageDead(temporary),
            Return(Some(value)),
            StorageLive(unreachable),
            Trap,
            StorageLive(value),
            Constant(value, Scalar::I64(40)),
            Jump(InstructionId(1)),
        ],
    );
    let program = module(vec![function]).verify().unwrap();
    let verified = program.function(program.entry()).unwrap();
    let frame = verified.frame_layout();
    assert_eq!(frame.slot_count(), 2);
    assert_ne!(frame.slot(value), frame.slot(temporary));
    assert_eq!(frame.slot(unreachable), None);
    assert_eq!(
        program.run(&[], ExecutionLimits { fuel: 9, frames: 1 }),
        Ok(Some(Scalar::I64(42)))
    );
}

#[test]
fn checked_source_division_and_remainder_preserve_failure_kind_and_span() {
    let source = concat!(
        "fn quotient(a: i64, b: i64) -> i64 { return a / b }\n",
        "fn remainder(a: i64, b: i64) -> i64 { return a % b }\n",
    );
    let program = compile(source).unwrap();
    let lower = |name: &str| {
        let id = program
            .functions()
            .iter()
            .position(|r| r.name.as_ref() == name)
            .unwrap();
        VerifiedModule::lower(&program, id).unwrap()
    };
    let quotient = lower("quotient");
    let remainder = lower("remainder");
    let run = |module: &VerifiedModule, lhs, rhs| {
        module.run(
            &[Scalar::I64(lhs), Scalar::I64(rhs)],
            ExecutionLimits {
                fuel: 100,
                frames: 1,
            },
        )
    };
    assert_eq!(run(&quotient, -7, 3), Ok(Some(Scalar::I64(-2))));
    assert_eq!(run(&remainder, -7, 3), Ok(Some(Scalar::I64(-1))));
    assert_eq!(run(&remainder, i64::MIN, -1), Ok(Some(Scalar::I64(0))));
    assert!(matches!(
        run(&quotient, i64::MIN, -1),
        Err(RunError::IntegerOverflow { .. })
    ));
    for (module, expression) in [(&quotient, "a / b"), (&remainder, "a % b")] {
        let Err(RunError::DivisionByZero {
            function,
            instruction,
            span,
        }) = run(module, 4, 0)
        else {
            panic!("expected division by zero");
        };
        assert_eq!(function, module.entry());
        assert_eq!(
            module.function(function).unwrap().spans()[instruction.0 as usize],
            span
        );
        assert_eq!(span.source_text(source), expression);
    }
}

#[test]
fn checked_lowering_releases_values_on_branch_edges_and_loop_iterations() {
    let source = concat!(
        "fn pick(flag: bool, x: i64, y: i64) -> i64 {\n",
        " if flag { return x }\n return y\n}\n",
        "fn accumulate(unused: i64, n: i64) -> i64 {\n",
        " let mut sum = 0\n let mut i = 0\n",
        " while i < n { let doubled = i * 2; sum += doubled; i += 1 }\n",
        " return sum\n}\n",
    );
    let checked = compile(source).unwrap();
    let lower = |name: &str| {
        let id = checked
            .functions()
            .iter()
            .position(|routine| routine.name.as_ref() == name)
            .unwrap();
        VerifiedModule::lower(&checked, id).unwrap()
    };
    let limits = ExecutionLimits {
        fuel: 1_000,
        frames: 1,
    };
    let pick = lower("pick");
    for (flag, expected) in [(true, 7), (false, 9)] {
        assert_eq!(
            pick.run(
                &[Scalar::Bool(flag), Scalar::I64(7), Scalar::I64(9)],
                limits
            ),
            Ok(Some(Scalar::I64(expected)))
        );
    }
    let accumulate = lower("accumulate");
    let body = accumulate.function(accumulate.entry()).unwrap();
    assert_eq!(
        body.instructions().first(),
        Some(&Instruction::StorageDead(body.parameters()[0]))
    );
    for (n, expected) in [(0, 0), (5, 20)] {
        let arguments = [Scalar::I64(100), Scalar::I64(n)];
        assert_eq!(
            accumulate.run(&arguments, limits),
            Ok(Some(Scalar::I64(expected)))
        );
        let mut activation = accumulate.start(&arguments, limits).unwrap();
        while let ExecutionStatus::Paused(_) = activation.resume(1) {}
        assert_eq!(
            activation.status(),
            ExecutionStatus::Returned(Some(Scalar::I64(expected)))
        );
    }
}

#[test]
fn loop_exhausts_fuel() {
    let infinite = image(&[], &[], None, &[Instruction::Jump(InstructionId(0))]);
    let verified = module(vec![infinite]).verify().unwrap();
    assert_eq!(
        verified.run(&[], ExecutionLimits { fuel: 5, frames: 1 }),
        Err(RunError::FuelExhausted)
    );
    let mut activation = verified
        .start(&[], ExecutionLimits { fuel: 5, frames: 1 })
        .unwrap();
    let initial = activation.status();
    assert!(matches!(initial, ExecutionStatus::Paused(_)));
    drop(verified);
    assert_eq!(activation.resume(2), initial);
    assert_eq!(activation.resume(2), initial);
    assert_eq!(activation.instructions_executed(), 4);
    let hard_limit = ExecutionStatus::Failed(RunError::FuelExhausted);
    assert_eq!(activation.resume(2), hard_limit);
    assert_eq!(activation.instructions_executed(), 5);
    assert_eq!(activation.resume(1), hard_limit);
    assert_eq!(activation.instructions_executed(), 5);
}

#[test]
fn factorial_excludes_nops_and_keeps_statement_boundaries_after_relocation() {
    let source = include_str!("../examples/functions.psc");
    let checked = cyallo::compile_with_options(
        source,
        cyallo::CompileOptions {
            dialect: cyallo::Dialect::Caie27,
            ..Default::default()
        },
    )
    .unwrap();
    let verified = VerifiedModule::lower(&checked, 1).unwrap();
    assert!(
        !verified
            .disassemble()
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("nop"))
    );
    let function = verified.function(verified.entry()).unwrap();
    let limits = ExecutionLimits {
        fuel: 500,
        frames: 6,
    };
    for (input, expected) in [(0, 1), (5, 120)] {
        let args = [Scalar::I64(input)];
        let expected = Some(Scalar::I64(expected));
        assert_eq!(verified.run(&args, limits), Ok(expected));
        let mut activation = verified.start(&args, limits).unwrap();
        let mut base_returns = 0;
        let mut recursive_returns = 0;
        while let ExecutionStatus::Paused(_) = activation.status() {
            for marker in activation.statement_boundaries() {
                assert!((marker.instruction.0 as usize) < function.instructions().len());
                let text = marker.span.source_text(source);
                base_returns += usize::from(text == "RETURN 1");
                recursive_returns += usize::from(text == "RETURN n * Factorial(n - 1)");
            }
            let fuel = activation.instructions_executed();
            activation.resume(1);
            assert_eq!(activation.instructions_executed(), fuel + 1);
        }
        assert_eq!(activation.status(), ExecutionStatus::Returned(expected));
        assert_eq!(base_returns, 1);
        assert_eq!(recursive_returns, input as usize);
    }
}

#[test]
fn exact_fuel_can_finish_an_activation() {
    let body = image(&[], &[], None, &[Instruction::Return(None)]);
    let program = module(vec![body]).verify().unwrap();
    assert_eq!(
        program
            .start(&[], ExecutionLimits { fuel: 0, frames: 1 })
            .unwrap_err(),
        RunError::FuelExhausted
    );
    let mut activation = program
        .start(&[], ExecutionLimits { fuel: 1, frames: 1 })
        .unwrap();
    assert_eq!(activation.resume(1), ExecutionStatus::Returned(None));
    assert_eq!(activation.instructions_executed(), 1);
}
