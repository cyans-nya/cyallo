use cyallo::{
    Span,
    bytecode::verified::{
        CallSite, CallSiteId, CallTarget, ExecutionLimits, ExecutionStatus, FORMAT_VERSION,
        FunctionId, FunctionImage,
        Instruction::{self, *},
        InstructionId, InvalidBytecode, LoanAccess, LoanOrigin, ModuleImage, Register,
        RegisterType, Scalar, ScalarType, StorageState, VerifiedModule, VerifyError, encode,
    },
    compile,
};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1_000,
    frames: 1,
};

fn shared(owner: Register) -> RegisterType {
    RegisterType::Loan {
        access: LoanAccess::Shared,
        origin: LoanOrigin::Local(owner),
        pointee: ScalarType::I64.into(),
    }
}

fn lower(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    let entry = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(&program, entry).unwrap()
}

fn verify(
    registers: &[RegisterType],
    parameters: &[Register],
    result: Option<ScalarType>,
    instructions: &[Instruction],
) -> Result<VerifiedModule, VerifyError> {
    ModuleImage {
        version: FORMAT_VERSION,
        entry: FunctionId(0),
        shapes: Box::new([]),
        signatures: Box::new([]),
        globals: Box::new([]),
        managed: Box::new([]),
        traits: Box::new([]),
        implementations: Box::new([]),
        functions: Box::new([image(registers, parameters, result, instructions)]),
    }
    .verify()
}

fn image(
    registers: &[RegisterType],
    parameters: &[Register],
    result: Option<ScalarType>,
    instructions: &[Instruction],
) -> FunctionImage {
    FunctionImage {
        locals: Box::new([]),
        frozen: Box::new([]),
        contracts: Box::new([]),
        generator: None,
        registers: registers.into(),
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

#[test]
fn shared_loan_survives_slices_and_branches_until_its_last_alias() {
    let source = r#"
fn entry(early: bool, number: i64) -> i64 {
    let owner = number + 1
    let view = &owner
    let alias = view
    if early { return alias.* }
    return view.* + 1
}
"#;
    let module = lower(source);
    let body = module.function(module.entry()).unwrap();
    assert!(
        body.instructions()
            .iter()
            .any(|op| matches!(op, Borrow(..)))
    );
    assert_eq!(
        body.instructions()
            .iter()
            .filter(|op| matches!(op, ReadLoan(..)))
            .count(),
        2
    );
    for (early, expected) in [(true, 42), (false, 43)] {
        let mut task = module
            .start(&[Scalar::Bool(early), Scalar::I64(41)], LIMITS)
            .unwrap();
        while let ExecutionStatus::Paused(_) = task.resume(1) {}
        assert_eq!(
            task.status(),
            ExecutionStatus::Returned(Some(Scalar::I64(expected)))
        );
    }
}

#[test]
fn an_ended_shared_loan_releases_its_owner_home_for_reuse() {
    let owner = Register(0);
    let loan = Register(1);
    let first = Register(2);
    let second = Register(3);
    let module = verify(
        &[
            ScalarType::I64.into(),
            shared(owner),
            ScalarType::I64.into(),
            ScalarType::I64.into(),
        ],
        &[owner],
        Some(ScalarType::I64),
        &[
            StorageLive(loan),
            Borrow(loan, owner),
            StorageDead(loan),
            StorageDead(owner),
            StorageLive(first),
            Constant(first, Scalar::I64(5)),
            StorageLive(second),
            Constant(second, Scalar::I64(42)),
            StorageDead(first),
            Return(Some(second)),
        ],
    )
    .unwrap();
    let layout = module.function(module.entry()).unwrap().frame_layout();
    assert_ne!(layout.slot(owner), layout.slot(loan));
    assert!([layout.slot(first), layout.slot(second)].contains(&layout.slot(owner)));
    assert_eq!(layout.slot_count(), 2);
    assert_eq!(
        module.run(&[Scalar::I64(21)], LIMITS),
        Ok(Some(Scalar::I64(42)))
    );
}

#[test]
fn verifier_rejects_live_alias_mutation_owner_release_and_escapes() {
    let owner = Register(0);
    let loan = Register(1);
    let alias = Register(2);
    let other = Register(3);
    let registers = [
        ScalarType::I64.into(),
        shared(owner),
        shared(owner),
        ScalarType::I64.into(),
    ];
    for (op, expected) in [
        (
            Constant(owner, Scalar::I64(2)),
            InvalidBytecode::BorrowedOwner(owner),
        ),
        (StorageDead(owner), InvalidBytecode::BorrowedOwner(owner)),
    ] {
        let error = verify(
            &registers,
            &[owner],
            None,
            &[
                StorageLive(loan),
                Borrow(loan, owner),
                StorageLive(alias),
                Copy(alias, loan),
                StorageDead(loan),
                op,
                Trap,
            ],
        )
        .unwrap_err();
        assert_eq!(error.instruction, Some(InstructionId(5)));
        assert_eq!(error.kind, expected);
    }
    let dead_owner = verify(
        &registers,
        &[],
        None,
        &[StorageLive(loan), Borrow(loan, owner), Trap],
    )
    .unwrap_err();
    assert_eq!(dead_owner.kind, InvalidBytecode::DeadStorageAccess(owner));
    let wrong_owner = verify(
        &registers,
        &[owner, other],
        None,
        &[StorageLive(loan), Borrow(loan, other), Trap],
    )
    .unwrap_err();
    assert_eq!(
        wrong_owner.kind,
        InvalidBytecode::LoanOwnerMismatch { loan, owner: other }
    );
    let escaped = verify(
        &registers,
        &[owner],
        Some(ScalarType::I64),
        &[StorageLive(loan), Borrow(loan, owner), Return(Some(loan))],
    )
    .unwrap_err();
    assert_eq!(escaped.kind, InvalidBytecode::ExpectedScalar(loan));
}

#[test]
fn verifier_requires_loan_initialization_to_agree_at_joins() {
    let condition = Register(0);
    let owner = Register(1);
    let loan = Register(2);
    let registers = [
        ScalarType::Bool.into(),
        ScalarType::I64.into(),
        shared(owner),
    ];
    let error = verify(
        &registers,
        &[condition, owner],
        None,
        &[
            StorageLive(loan),
            Branch(condition, InstructionId(2), InstructionId(4)),
            Borrow(loan, owner),
            Jump(InstructionId(5)),
            Jump(InstructionId(5)),
            Trap,
        ],
    )
    .unwrap_err();
    assert_eq!(error.instruction, Some(InstructionId(5)));
    assert_eq!(error.kind, InvalidBytecode::LoanJoin(loan));
}

#[test]
fn exclusive_parameter_preserves_the_callers_home_through_nested_slices() {
    let module = lower(
        r#"
proc increase(value: &mut i64) { value.* += 1 }
proc repeat(value: &mut i64) {
    increase(value)
    increase(value)
}
proc entry(seed: i64) -> i64 {
    let mut owner = seed
    repeat(&mut owner)
    return owner
}
"#,
    );
    let limits = ExecutionLimits {
        frames: 3,
        ..LIMITS
    };
    let mut task = module.start(&[Scalar::I64(40)], limits).unwrap();
    let mut entered_write = false;
    while let ExecutionStatus::Paused(position) = task.resume(1) {
        let body = module.function(position.function).unwrap();
        if matches!(
            body.instructions()[position.instruction.0 as usize],
            WriteLoan(..)
        ) {
            entered_write = true;
            assert_ne!(position.function, module.entry());
        }
    }
    assert!(entered_write);
    assert_eq!(
        task.status(),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
}

#[test]
fn verifier_rejects_direct_access_under_exclusive_loans_and_writes_under_shared_loans() {
    let owner = Register(0);
    let loan = Register(1);
    let dst = Register(2);
    let other = Register(3);
    let exclusive = RegisterType::Loan {
        access: LoanAccess::Exclusive,
        origin: LoanOrigin::Local(owner),
        pointee: ScalarType::I64.into(),
    };
    let mut kinds = [
        ScalarType::I64.into(),
        exclusive,
        ScalarType::I64.into(),
        exclusive,
    ];
    let error = verify(
        &kinds,
        &[owner],
        None,
        &[
            StorageLive(loan),
            Borrow(loan, owner),
            StorageLive(dst),
            Copy(dst, owner),
            Trap,
        ],
    )
    .unwrap_err();
    assert_eq!(error.kind, InvalidBytecode::ExclusiveOwnerAccess(owner));
    let error = verify(
        &kinds,
        &[owner],
        None,
        &[
            StorageLive(loan),
            Borrow(loan, owner),
            StorageLive(other),
            Borrow(other, owner),
            Trap,
        ],
    )
    .unwrap_err();
    assert_eq!(error.kind, InvalidBytecode::BorrowedOwner(owner));
    kinds[loan.0 as usize] = shared(owner);
    let error = verify(
        &kinds,
        &[owner],
        None,
        &[
            StorageLive(loan),
            Borrow(loan, owner),
            WriteLoan(loan, owner),
            Trap,
        ],
    )
    .unwrap_err();
    assert_eq!(error.kind, InvalidBytecode::ExpectedExclusiveLoan(loan));
}

#[test]
fn verifier_rejects_conflicting_arguments_even_if_they_copy_the_same_loan() {
    let owner = Register(0);
    let loan = Register(1);
    let alias = Register(2);
    let exclusive = RegisterType::Loan {
        access: LoanAccess::Exclusive,
        origin: LoanOrigin::Local(owner),
        pointee: ScalarType::I64.into(),
    };
    let mut caller = image(
        &[ScalarType::I64.into(), exclusive, exclusive],
        &[owner],
        None,
        &[
            StorageLive(loan),
            Borrow(loan, owner),
            StorageLive(alias),
            Copy(alias, loan),
            Call(CallSiteId(0)),
            Trap,
        ],
    );
    caller.calls = Box::new([CallSite {
        target: CallTarget::Direct(FunctionId(1)),
        arguments: Box::new([loan, alias]),
        destination: None,
    }]);
    let parameter = |register, access| RegisterType::Loan {
        access,
        origin: LoanOrigin::Parameter { register },
        pointee: ScalarType::I64.into(),
    };
    let first = Register(0);
    let second = Register(1);
    let callee = image(
        &[
            parameter(first, LoanAccess::Exclusive),
            parameter(second, LoanAccess::Shared),
        ],
        &[first, second],
        None,
        &[Trap],
    );
    let error = ModuleImage {
        version: FORMAT_VERSION,
        entry: FunctionId(0),
        shapes: Box::new([]),
        signatures: Box::new([]),
        globals: Box::new([]),
        managed: Box::new([]),
        traits: Box::new([]),
        implementations: Box::new([]),
        functions: Box::new([caller, callee]),
    }
    .verify()
    .unwrap_err();
    assert_eq!(
        error.kind,
        InvalidBytecode::ConflictingLoanArgument {
            first: loan,
            second: alias
        }
    );
}

#[test]
fn verified_storage_can_clean_up_a_partially_initialized_scalar() {
    let condition = Register(0);
    let value = Register(1);
    let module = verify(
        &[ScalarType::Bool.into(), ScalarType::I64.into()],
        &[condition],
        None,
        &[
            StorageLive(value),
            Branch(condition, InstructionId(2), InstructionId(4)),
            Constant(value, Scalar::I64(42)),
            Jump(InstructionId(5)),
            Jump(InstructionId(5)),
            StorageDead(value),
            StorageDead(condition),
            Return(None),
            Trap,
        ],
    )
    .unwrap();
    let body = module.function(module.entry()).unwrap();
    assert_eq!(
        body.storage_before(InstructionId(5)).unwrap()[1],
        StorageState::MaybeInitialized
    );
    assert_eq!(
        body.storage_before(InstructionId(6)).unwrap()[1],
        StorageState::Dead
    );
    assert_eq!(body.storage_before(InstructionId(8)), None);
    for condition in [false, true] {
        let mut task = module.start(&[Scalar::Bool(condition)], LIMITS).unwrap();
        while let ExecutionStatus::Paused(_) = task.resume(1) {}
        assert_eq!(task.status(), ExecutionStatus::Returned(None));
    }
}

#[test]
fn loop_loans_end_on_break_continue_and_backedges() {
    let module = lower(
        r#"
fn entry(limit: i64, stop: i64) -> i64 {
    let mut index = 0
    let mut sum = 0
    while index < limit {
        let current = index
        let view = &current
        index += 1
        if view.* == stop { break }
        if view.* == 3 { continue }
        sum += view.*
    }
    return sum
}
"#,
    );
    let limits = ExecutionLimits {
        fuel: 250_000,
        ..LIMITS
    };
    assert_eq!(
        module.run(&[Scalar::I64(2_000), Scalar::I64(-1)], limits),
        Ok(Some(Scalar::I64(1_998_997)))
    );
    let mut task = module
        .start(&[Scalar::I64(2_000), Scalar::I64(7)], LIMITS)
        .unwrap();
    while let ExecutionStatus::Paused(_) = task.resume(1) {}
    assert_eq!(
        task.status(),
        ExecutionStatus::Returned(Some(Scalar::I64(18)))
    );
}
