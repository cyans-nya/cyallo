use cyallo::bytecode::verified::{
    ExecutionLimits, ExecutionStatus, LowerError, RunError, Scalar, SdkCompileError, VerifiedModule,
};
use cyallo_ir::{
    contract::{Disposition, Effects, Operand, OperandModes, Safepoint},
    registry::{BindingContract, Reference, ReferenceLinkError, RegistrationError, Registry},
};

const SOURCE: &str = include_str!("fixtures/sdk_scalar.ya");
const ITEM: &str = "api::calculate";
const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 200,
    frames: 3,
};
const CHECKED_SCALAR: Effects = Effects {
    may_fail: true,
    ..Effects::NONE
};

fn register(path: &str, effects: Effects, safepoint: Safepoint) -> Registry {
    let sdk = cyallo_ir::namespace::single(cyallo_parser::parse(SOURCE).unwrap())
        .unwrap()
        .interface;
    Registry::new().intrinsic(
        ITEM,
        sdk.item(ITEM).unwrap().digest,
        BindingContract {
            disposition: Disposition::EnginePrimitive,
            effects,
            safepoint,
            operands: OperandModes {
                receiver: Operand::Read,
                rest: Operand::Read,
                exceptions: &[],
            },
        },
        Some(Reference::Source(path.into())),
        vec![],
    )
}

#[test]
fn installed_reference_executes_through_a_resolved_alias() {
    let first_registry = || {
        register(
            "crate::implementation::twice",
            CHECKED_SCALAR,
            Safepoint::Never,
        )
    };
    let first = VerifiedModule::lower_sdk_reference(SOURCE, "entry", first_registry()).unwrap();
    let second = VerifiedModule::lower_sdk_reference(
        SOURCE,
        "entry",
        register(
            "crate::implementation::twice_plus_one",
            CHECKED_SCALAR,
            Safepoint::Never,
        ),
    )
    .unwrap();
    let local = VerifiedModule::lower_sdk_reference(SOURCE, "local", first_registry()).unwrap();
    let args = [Scalar::I64(21)];
    assert_eq!(second.run(&args, LIMITS), Ok(Some(Scalar::I64(43))));
    assert_eq!(local.run(&args, LIMITS), Ok(Some(Scalar::I64(21))));

    let mut activation = first.start(&args, LIMITS).unwrap();
    while let ExecutionStatus::Paused(_) = activation.resume(1) {}
    assert_eq!(
        activation.status(),
        ExecutionStatus::Returned(Some(Scalar::I64(42)))
    );
    let Err(RunError::IntegerOverflow { span, .. }) = first.run(&[Scalar::I64(i64::MAX)], LIMITS)
    else {
        panic!("expected arithmetic failure in the installed reference");
    };
    assert_eq!(span.source_text(SOURCE), "value + value");
}

#[test]
fn scalar_reference_checks_registration_signature_and_effects() {
    let missing =
        VerifiedModule::lower_sdk_reference(SOURCE, "entry", Registry::new()).unwrap_err();
    assert!(
        matches!(missing, SdkCompileError::Link(ReferenceLinkError::Registration(errors))
        if matches!(errors.as_slice(), [RegistrationError::Unbound { path }] if path.as_ref() == ITEM))
    );

    let error = |path, effects, safepoint| {
        VerifiedModule::lower_sdk_reference(SOURCE, "entry", register(path, effects, safepoint))
            .unwrap_err()
    };
    assert!(matches!(
        error(
            "crate::implementation::missing",
            CHECKED_SCALAR,
            Safepoint::Never
        ),
        SdkCompileError::Link(ReferenceLinkError::MissingSource { .. })
    ));
    assert!(matches!(
        error(
            "crate::implementation::wrong_type",
            CHECKED_SCALAR,
            Safepoint::Never
        ),
        SdkCompileError::Link(ReferenceLinkError::SignatureMismatch(_))
    ));
    assert!(matches!(
        error(
            "crate::implementation::twice",
            Effects::NONE,
            Safepoint::Never
        ),
        SdkCompileError::ReferenceMayFail(_)
    ));
    assert!(matches!(
        error(
            "crate::implementation::twice",
            Effects {
                may_park: true,
                ..CHECKED_SCALAR
            },
            Safepoint::MayReenter,
        ),
        SdkCompileError::Link(ReferenceLinkError::UnsupportedContract(_))
    ));
    assert!(matches!(
        error(
            "crate::implementation::needs_cast",
            CHECKED_SCALAR,
            Safepoint::Never
        ),
        SdkCompileError::Lower(
            LowerError::UnsupportedOperation { .. }
                | LowerError::UnsupportedRegister { .. }
                | LowerError::UnsupportedSignature { .. }
        )
    ));
}
