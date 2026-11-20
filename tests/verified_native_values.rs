//! Binary64, boolean and conversion operations in native code. Each program
//! runs in the verified interpreter (the reference) and in every adapter that
//! encodes it; results, failures and executed-instruction counts must agree.

use cyallo::{
    backend::verified::{
        CodeStore, CraneliftProfile, EmitError, NativeTarget, NativeTier, Operation, UnaryOp,
    },
    bytecode::verified::{ExecutionLimits, ExecutionStatus, Scalar, VerifiedModule},
    compile,
};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1000,
    frames: 4,
};
const TIERS: [NativeTier; 3] = [
    NativeTier::Winch,
    NativeTier::Cranelift(CraneliftProfile::LowLatency),
    NativeTier::Cranelift(CraneliftProfile::Optimized),
];

fn module(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    let index = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(&program, index).unwrap()
}

fn f(value: f64) -> Scalar {
    Scalar::from_f64(value)
}

/// NaN has many bit patterns and hardware picks its own sign.
fn canonical(status: ExecutionStatus) -> ExecutionStatus {
    match status {
        ExecutionStatus::Returned(Some(Scalar::F64(bits))) if f64::from_bits(bits).is_nan() => {
            ExecutionStatus::Returned(Some(Scalar::F64(f64::NAN.to_bits())))
        }
        other => other,
    }
}

/// Runs `source`'s entry on every tier that can encode it and compares with
/// the interpreter. Returns the tiers that ran.
fn conform(
    source: &str,
    cases: &[&[Scalar]],
    expect: impl Fn(&Operation) -> bool,
) -> Vec<NativeTier> {
    if NativeTarget::host().is_none() {
        return Vec::new();
    }
    let module = module(source);
    let mut store = CodeStore::new(module.clone()).unwrap();
    let plan = store.prepare(module.entry()).unwrap();
    assert!(
        plan.sites().iter().any(|site| expect(&site.operation)),
        "expected operation missing from the plan for {source}"
    );
    let mut ran = Vec::new();
    for tier in TIERS {
        let plan = store.prepare(module.entry()).unwrap();
        let image = match tier.emit(plan) {
            Ok(image) => image,
            Err(EmitError::UnsupportedOperation(_)) => continue,
            Err(error) => panic!("{tier:?}: {error}"),
        };
        let pin = store.publish(image).unwrap();
        for &args in cases {
            let mut native = pin.start(args, LIMITS).unwrap();
            let mut reference = module.start(args, LIMITS).unwrap();
            let actual = canonical(native.resume(LIMITS.fuel).unwrap());
            let wanted = canonical(reference.resume(LIMITS.fuel));
            assert_eq!(actual, wanted, "{tier:?} {args:?} {source}");
            assert_eq!(
                native.instructions_executed(),
                reference.instructions_executed(),
                "{tier:?} {args:?}"
            );
        }
        ran.push(tier);
    }
    ran
}

const EXTREMES: [f64; 9] = [
    0.0,
    -0.0,
    1.5,
    -2.25,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
    1e300,
    f64::MIN_POSITIVE,
];

fn pairs() -> Vec<[Scalar; 2]> {
    EXTREMES
        .iter()
        .flat_map(|&a| EXTREMES.iter().map(move |&b| [f(a), f(b)]))
        .collect()
}

#[test]
fn float_arithmetic_matches_the_interpreter_on_every_tier() {
    let cases = pairs();
    let cases: Vec<&[Scalar]> = cases.iter().map(|case| case.as_slice()).collect();
    for operator in ["+", "-", "*"] {
        let source = format!("fn entry(a: f64, b: f64) -> f64 {{ return a {operator} b }}\n");
        let ran = conform(&source, &cases, |op| matches!(op, Operation::Float { .. }));
        assert!(
            ran.len() >= 2 || NativeTarget::host().is_none(),
            "{operator}"
        );
    }
}

#[test]
fn float_division_and_comparisons_match_including_nan_and_zero() {
    let cases = pairs();
    let cases: Vec<&[Scalar]> = cases.iter().map(|case| case.as_slice()).collect();
    conform(
        "fn entry(a: f64, b: f64) -> f64 { return a / b }\n",
        &cases,
        |op| matches!(op, Operation::Float { .. }),
    );
    for operator in ["<", "<=", ">", ">=", "==", "!="] {
        let source = format!("fn entry(a: f64, b: f64) -> bool {{ return a {operator} b }}\n");
        conform(&source, &cases, |op| matches!(op, Operation::Float { .. }));
    }
}

#[test]
fn negation_not_and_boolean_equality_match() {
    let floats: Vec<[Scalar; 1]> = EXTREMES.iter().map(|&a| [f(a)]).collect();
    let floats: Vec<&[Scalar]> = floats.iter().map(|case| case.as_slice()).collect();
    conform("fn entry(a: f64) -> f64 { return -a }\n", &floats, |op| {
        matches!(
            op,
            Operation::Unary {
                operation: UnaryOp::NegateFloat,
                ..
            }
        )
    });
    let integers: Vec<[Scalar; 1]> = [0, 1, -1, i64::MAX, i64::MIN, i64::MIN + 1]
        .iter()
        .map(|&a| [Scalar::I64(a)])
        .collect();
    let integers: Vec<&[Scalar]> = integers.iter().map(|case| case.as_slice()).collect();
    conform("fn entry(a: i64) -> i64 { return -a }\n", &integers, |op| {
        matches!(
            op,
            Operation::Unary {
                operation: UnaryOp::NegateInteger,
                ..
            }
        )
    });
    let bools = [
        [Scalar::Bool(false), Scalar::Bool(false)],
        [Scalar::Bool(false), Scalar::Bool(true)],
        [Scalar::Bool(true), Scalar::Bool(false)],
        [Scalar::Bool(true), Scalar::Bool(true)],
    ];
    let bools: Vec<&[Scalar]> = bools.iter().map(|case| case.as_slice()).collect();
    conform(
        "fn entry(a: bool, b: bool) -> bool { return !(a == b) }\n",
        &bools,
        |op| matches!(op, Operation::Boolean { .. }),
    );
    conform(
        "fn entry(a: bool, b: bool) -> bool { return a != b }\n",
        &bools,
        |op| matches!(op, Operation::Boolean { .. }),
    );
}

#[test]
fn integer_to_float_and_real_division_match() {
    let integers: Vec<[Scalar; 2]> = [0, 1, -7, 9, i64::MAX, i64::MIN]
        .iter()
        .flat_map(|&a| {
            [0, 1, -2, 3, i64::MIN, i64::MAX]
                .iter()
                .map(move |&b| [Scalar::I64(a), Scalar::I64(b)])
        })
        .collect();
    let integers: Vec<&[Scalar]> = integers.iter().map(|case| case.as_slice()).collect();
    let ran = conform(
        "fn entry(a: i64, b: i64) -> f64 { return a as f64 + b as f64 }\n",
        &integers,
        |op| {
            matches!(
                op,
                Operation::Unary {
                    operation: UnaryOp::IntegerToFloat,
                    ..
                }
            )
        },
    );
    assert!(ran.len() >= 2 || NativeTarget::host().is_none());
}

#[test]
fn float_to_integer_truncates_and_fails_out_of_range_on_cranelift() {
    let floats: Vec<[Scalar; 1]> = [
        0.0,
        -0.0,
        1.9,
        -1.9,
        9.2e18,
        -9.3e18,
        9_223_372_036_854_775_807.0,
        -9_223_372_036_854_775_808.0,
        f64::NAN,
        f64::INFINITY,
    ]
    .iter()
    .map(|&a| [f(a)])
    .collect();
    let floats: Vec<&[Scalar]> = floats.iter().map(|case| case.as_slice()).collect();
    let ran = conform(
        "fn entry(a: f64) -> i64 { return a as i64 }\n",
        &floats,
        |op| {
            matches!(
                op,
                Operation::Unary {
                    operation: UnaryOp::FloatToInteger,
                    ..
                }
            )
        },
    );
    // Winch has no float truncation; the routine stays in the interpreter there.
    assert!(!ran.contains(&NativeTier::Winch));
    assert!(ran.len() == 2 || NativeTarget::host().is_none());
}
