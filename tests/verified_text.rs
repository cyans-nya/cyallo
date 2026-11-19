//! Strings and fixed-size arrays in the verified interpreter. Expected text
//! is written by hand from the language contract.

use cyallo::{
    CompileOptions, Dialect,
    bytecode::verified::{ExecutionLimits, OutputSink, RunError, TextFailure, VerifiedModule},
    compile, compile_with_options,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1_000_000,
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

fn modern(source: &str) -> Result<Vec<String>, RunError> {
    execute(&compile(source).unwrap())
}

fn caie(source: &str) -> Result<Vec<String>, RunError> {
    execute(
        &compile_with_options(
            source,
            CompileOptions {
                dialect: Dialect::Compatibility,
                convenience_builtins: true,
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

#[test]
fn concatenation_comparison_and_length_work_on_characters() {
    let source = r#"
let a = "caf"
let b = a + "é"
std::io::print(b, " ", std::string::len(b), " ", std::string::len(""))
std::io::print(a == "caf", " ", a != "caf", " ", a < b, " ", b <= a, " ", "b" > "a", " ", "a" >= "b")
"#;
    assert_eq!(
        modern(source).unwrap(),
        ["café 4 0", "true false true false true false"]
    );
}

#[test]
fn left_right_and_mid_use_one_based_character_positions() {
    let source = concat!(
        "DECLARE t : STRING\n",
        "t <- \"Cambridge\"\n",
        "OUTPUT LEFT(t, 3), \"|\", RIGHT(t, 3), \"|\", MID(t, 4, 3), \"|\", MID(t, 1, 9), \"|\", LEFT(t, 0)\n",
    );
    assert_eq!(caie(source).unwrap(), ["Cam|dge|bri|Cambridge|"]);
}

#[test]
fn slicing_failures_are_typed_and_leave_earlier_output() {
    let run = |expression: &str| {
        caie(&format!(
            "DECLARE t : STRING\nt <- \"abc\"\nOUTPUT \"ok\"\nOUTPUT {expression}\n"
        ))
    };
    let failure = |expression: &str| match run(expression) {
        Err(RunError::Text { failure, .. }) => failure,
        other => panic!("{expression}: {other:?}"),
    };
    assert_eq!(failure("MID(t, 0, 1)"), TextFailure::IndexOutOfBounds);
    assert_eq!(failure("MID(t, 2, -1)"), TextFailure::NegativeLength);
    assert_eq!(failure("MID(t, 3, 2)"), TextFailure::RangeOutOfBounds);
    assert_eq!(failure("LEFT(t, 4)"), TextFailure::RangeOutOfBounds);
    assert_eq!(failure("RIGHT(t, 4)"), TextFailure::RangeOutOfBounds);
    assert_eq!(failure("LEFT(t, -1)"), TextFailure::NegativeLength);
}

#[test]
fn strings_cross_calls_and_live_inside_tuples_and_enums() {
    let source = r#"
fn greet(name: string) -> string { return "hello, " + name }
enum Reply { None, Said: string }
fn reply(loud: bool) -> Reply {
    if loud { return Reply::Said("HEY") }
    return Reply::None
}
std::io::print(greet("ada"))
let pair = (greet("x"), 2)
std::io::print(pair.0, pair.1)
match reply(true) { Reply::Said(s) => std::io::print(s), Reply::None => std::io::print("silent") }
match reply(false) { Reply::Said(s) => std::io::print(s), Reply::None => std::io::print("silent") }
"#;
    assert_eq!(
        modern(source).unwrap(),
        ["hello, ada", "hello, x2", "HEY", "silent"]
    );
}

#[test]
fn arrays_are_read_written_and_measured() {
    let source = r#"
let a = [1, 2, 3]
let mut b = [10, 20]
b[1] = a[0] + std::array::len(a)
std::io::print(b[0], " ", b[1], " ", std::array::len(b))
let mut total = 0
let mut i = 0
while i < 3 {
    total = total + a[i]
    i = i + 1
}
std::io::print(total)
"#;
    assert_eq!(modern(source).unwrap(), ["10 4 2", "6"]);
}

#[test]
fn caie_arrays_have_their_declared_bounds() {
    assert_eq!(
        caie(include_str!("../examples/arrays.psc")).unwrap(),
        ["4,4"]
    );
    let source = concat!(
        "DECLARE grid : ARRAY[1:2, 1:3] OF INTEGER\n",
        "DECLARE r : INTEGER\n",
        "DECLARE c : INTEGER\n",
        "FOR r <- 1 TO 2\n",
        "    FOR c <- 1 TO 3\n",
        "        grid[r, c] <- r * 10 + c\n",
        "    NEXT c\n",
        "NEXT r\n",
        "OUTPUT grid[1, 1], \",\", grid[1, 3], \",\", grid[2, 1], \",\", grid[2, 3]\n",
    );
    assert_eq!(caie(source).unwrap(), ["11,13,21,23"]);
}

#[test]
fn indices_outside_the_bounds_fail_and_uninitialized_elements_cannot_be_read() {
    let declare = "DECLARE xs : ARRAY[1:3] OF INTEGER\n";
    for index in ["0", "4", "-1"] {
        let source = format!("{declare}xs[1] <- 1\nOUTPUT xs[{index}]\n");
        assert!(
            matches!(caie(&source), Err(RunError::IndexOutOfBounds { .. })),
            "{index}"
        );
    }
    let source = format!("{declare}xs[1] <- 1\nOUTPUT xs[2]\n");
    assert!(matches!(
        caie(&source),
        Err(RunError::UninitializedElement { .. })
    ));
    let write = format!("{declare}xs[4] <- 1\n");
    assert!(matches!(
        caie(&write),
        Err(RunError::IndexOutOfBounds { .. })
    ));
}

#[test]
fn arrays_hold_strings_and_cross_calls() {
    let source = concat!(
        "FUNCTION Second(w : ARRAY[1:3] OF STRING) RETURNS STRING\n",
        "    RETURN w[2]\n",
        "ENDFUNCTION\n",
        "DECLARE words : ARRAY[1:3] OF STRING\n",
        "words[1] <- \"a\"\n",
        "words[2] <- \"b\"\n",
        "words[3] <- \"c\"\n",
        "OUTPUT Second(words), words[3]\n",
    );
    assert_eq!(caie(source).unwrap(), ["bc"]);
}

#[test]
fn text_and_array_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let program = compile("let s = \"x\"\nstd::io::print(s)\n").unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, Instruction, InvalidBytecode, ModuleImage,
            OperandListId, Register, RegisterType, ScalarType, Shape, ShapeId, TextId, ValueType,
            encode,
        },
    };
    use std::sync::Arc;

    const R0: Register = Register(0);
    const R1: Register = Register(1);

    fn kind(
        shapes: Vec<Shape>,
        registers: Vec<RegisterType>,
        lists: Vec<Box<[Register]>>,
        texts: Vec<Arc<str>>,
        code: Vec<Instruction>,
    ) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: shapes.into(),
            signatures: Box::new([]),
            functions: Box::new([FunctionImage {
                registers: registers.into(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([]),
                lists: lists.into(),
                texts: texts.into(),
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

    fn array(bounds: Vec<(i64, i64)>) -> Shape {
        Shape::Array {
            element: ValueType::Scalar(ScalarType::I64),
            bounds: bounds.into(),
        }
    }

    #[test]
    fn reversed_empty_and_oversized_bounds_are_rejected() {
        for bounds in [
            vec![(3, 1)],
            vec![],
            vec![(0, i64::MAX)],
            vec![(i64::MIN, i64::MAX)],
            vec![(0, 4095), (0, 4095)],
        ] {
            assert!(
                matches!(
                    kind(
                        vec![array(bounds.clone())],
                        vec![],
                        vec![],
                        vec![],
                        vec![Return(None)]
                    ),
                    InvalidBytecode::InvalidArrayBounds(_)
                ),
                "{bounds:?}"
            );
        }
    }

    #[test]
    fn an_unknown_text_constant_is_rejected() {
        let code = vec![
            StorageLive(R0),
            TextConstant(R0, TextId(0)),
            StorageDead(R0),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![], vec![RegisterType::Text], vec![], vec![], code),
            InvalidBytecode::UnknownText(_)
        ));
    }

    #[test]
    fn text_operations_reject_non_text_operands() {
        let code = vec![
            StorageLive(R0),
            StorageLive(R1),
            TextLength(R0, R1),
            Return(None),
        ];
        let registers = vec![
            RegisterType::Scalar(ScalarType::I64),
            RegisterType::Scalar(ScalarType::I64),
        ];
        assert!(matches!(
            kind(vec![], registers, vec![], vec![], code),
            InvalidBytecode::ExpectedText(_)
        ));
    }

    #[test]
    fn element_access_checks_rank_and_element_type() {
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Scalar(ScalarType::I64),
            RegisterType::Scalar(ScalarType::Bool),
        ];
        let load = |list: Vec<Register>, dst: u32| {
            let code = vec![
                StorageLive(R0),
                ArrayDeclare(R0),
                StorageLive(R1),
                StorageLive(Register(2)),
                ArrayLoad(Register(dst), R0, OperandListId(0)),
                Return(None),
            ];
            kind(
                vec![array(vec![(0, 1)])],
                registers.clone(),
                vec![list.into()],
                vec![],
                code,
            )
        };
        assert!(matches!(
            load(vec![R1, R1], 1),
            InvalidBytecode::ArrayRank {
                expected: 1,
                actual: 2,
                ..
            }
        ));
        assert!(matches!(
            load(vec![R1], 2),
            InvalidBytecode::ValueType { .. }
        ));
    }
}
