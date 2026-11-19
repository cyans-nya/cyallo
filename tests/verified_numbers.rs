//! Binary64 values, unary operators and numeric conversion in the verified
//! interpreter. Expected text is written by hand from IEEE-754 and the
//! language contract.

use cyallo::{
    CompileOptions, Dialect,
    bytecode::verified::{ExecutionLimits, OutputSink, RunError, VerifiedModule},
    compile, compile_with_options,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 100_000,
    frames: 64,
};

fn execute(program: &cyallo::ir::Program) -> Result<Vec<String>, RunError> {
    let module = VerifiedModule::lower(program, 0).unwrap_or_else(|error| panic!("{error}"));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    module.run_to(&[], LIMITS, sink)?;
    let lines = lines.lock().unwrap().clone();
    Ok(lines)
}

fn run(source: &str) -> Result<Vec<String>, RunError> {
    execute(&compile(source).unwrap())
}

#[test]
fn float_arithmetic_and_comparison_follow_ieee() {
    let source = r#"
std::io::print(1.5 + 2.25)
std::io::print(10.0 - 0.5, " ", 2.5 * 4.0, " ", 7.0 / 2.0, " ", 5.5 % 2.0)
std::io::print(1.0 < 2.0, " ", 2.0 <= 1.0, " ", 1.5 == 1.5, " ", 1.5 != 1.5)
"#;
    assert_eq!(
        run(source).unwrap(),
        ["3.75", "9.5 10 3.5 1.5", "true false true false"]
    );
}

#[test]
fn float_division_by_zero_gives_infinities_and_nan() {
    let source = r#"
let zero = 0.0
std::io::print(1.0 / zero, " ", -1.0 / zero, " ", zero / zero, " ", -zero)
std::io::print(zero / zero == zero / zero, " ", zero / zero != zero / zero)
"#;
    assert_eq!(run(source).unwrap(), ["inf -inf NaN -0", "false true"]);
}

#[test]
fn unary_minus_not_and_boolean_equality() {
    let source = r#"
let five = 5
let yes = true
std::io::print(-five, " ", !yes, " ", !(1 < 2), " ", yes == false, " ", yes != false)
"#;
    assert_eq!(run(source).unwrap(), ["-5 false false false true"]);
}

#[test]
fn negating_the_minimum_integer_overflows_with_its_span() {
    let source = "let minimum = -9223372036854775807 - 1\nstd::io::print(-minimum)\n";
    let Err(RunError::IntegerOverflow { span, .. }) = run(source) else {
        panic!("expected overflow")
    };
    assert_eq!(span.source_text(source), "-minimum");
}

#[test]
fn conversions_truncate_toward_zero_and_widen_integers_exactly() {
    let source = r#"
let seven = 7.9
let minus = -7.9
let three = 3
std::io::print(seven as i64, " ", minus as i64, " ", three as f64 / 2.0, " ", 0.0 as i64)
"#;
    assert_eq!(run(source).unwrap(), ["7 -7 1.5 0"]);
}

#[test]
fn out_of_range_and_non_finite_conversions_fail() {
    for value in [
        "1e30",
        "-1e30",
        "9223372036854775808.0",
        "1.0 / zero",
        "zero / zero",
    ] {
        let source = format!("let zero = 0.0\nlet x = {value}\nstd::io::print(x as i64)\n");
        assert!(
            matches!(run(&source), Err(RunError::ConversionOutOfRange { .. })),
            "{value}"
        );
    }
    let edge = "let x = -9223372036854775808.0\nstd::io::print(x as i64)\n";
    assert_eq!(run(edge).unwrap(), ["-9223372036854775808"]);
}

#[test]
fn floats_cross_calls_and_branches() {
    let source = r#"
fn pick(x: f64, n: i64, b: bool) -> f64 {
    let y = -x + 1.5
    let z = n as f64
    if !b && y > 2.0 || n == 3 { return y * z / 2.0 }
    return -y - 0.25
}
std::io::print(pick(1.0, 2, true))
std::io::print(pick(1.0, 3, true))
std::io::print(pick(-2.0, 1, false))
"#;
    // y = 0.5: -0.75. n == 3: 0.5 * 3 / 2. y = 3.5 and !false: 3.5 * 1 / 2.
    assert_eq!(run(source).unwrap(), ["-0.75", "0.75", "1.75"]);
}

#[test]
fn caie_real_division_and_its_zero_divisor() {
    let caie = |source: &str| {
        compile_with_options(
            source,
            CompileOptions {
                dialect: Dialect::Compatibility,
                ..Default::default()
            },
        )
        .unwrap()
    };
    assert_eq!(
        execute(&caie("OUTPUT 7 / 2\nOUTPUT 6 / 3\n")).unwrap(),
        ["3.5", "2"]
    );
    assert!(matches!(
        execute(&caie("DECLARE z : INTEGER\nz <- 0\nOUTPUT 1 / z\n")),
        Err(RunError::DivisionByZero { .. })
    ));
}

#[test]
fn float_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let program = compile("let x = 1.5\nstd::io::print(x + x)\n").unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FloatOp, FunctionId, FunctionImage, Instruction, InvalidBytecode,
            ModuleImage, Register, RegisterType, ScalarType, encode,
        },
    };

    fn kind(registers: Vec<ScalarType>, code: Vec<Instruction>) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([]),
            functions: Box::new([FunctionImage {
                registers: registers.into_iter().map(RegisterType::Scalar).collect(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([]),
                lists: Box::new([]),
                texts: Box::new([]),
                outputs: Box::new([]),
                spans: vec![Span::new(0, 1); code.len()].into(),
                words: encode(code),
                statements: Box::new([]),
            }]),
        }
        .verify()
        .unwrap_err()
        .kind
    }

    const R0: Register = Register(0);
    const R1: Register = Register(1);

    #[test]
    fn a_float_operation_on_integers_is_rejected() {
        let code = vec![
            StorageLive(R0),
            StorageLive(R1),
            Float(FloatOp::Add, R0, R1, R1),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![ScalarType::I64, ScalarType::I64], code),
            InvalidBytecode::RegisterType { .. }
        ));
    }

    #[test]
    fn negation_and_conversion_check_both_operand_types() {
        for code in [
            vec![StorageLive(R0), StorageLive(R1), Not(R0, R1), Return(None)],
            vec![
                StorageLive(R0),
                StorageLive(R1),
                IntegerToFloat(R0, R1),
                Return(None),
            ],
        ] {
            assert!(matches!(
                kind(vec![ScalarType::I64, ScalarType::I64], code),
                InvalidBytecode::RegisterType { .. }
            ));
        }
    }
}
