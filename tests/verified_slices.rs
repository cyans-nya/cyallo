//! Slices of arrays in the verified interpreter. Expected text is written by
//! hand from the language contract.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 20_000_000,
    frames: 512,
};

fn module(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    VerifiedModule::lower(&program, 0).unwrap_or_else(|error| panic!("{error}"))
}

fn sink() -> (OutputSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    (writer, lines)
}

fn run(source: &str) -> Result<Vec<String>, RunError> {
    let (writer, lines) = sink();
    module(source).run_to(&[], LIMITS, writer)?;
    let lines = lines.lock().unwrap().clone();
    Ok(lines)
}

#[test]
fn a_slice_parameter_reads_and_writes_the_array_it_views() {
    let source = r#"
proc fill(a: &mut []i64, n: i64) {
    for p in 0..=n - 1 { a[p] = p * 2 }
}
fn total(a: &[]i64) -> i64 {
    let mut sum = 0
    for p in 0..=std::array::len(a) - 1 { sum += a[p] }
    return sum
}
let mut arr = [0, 0, 0, 0]
fill(&mut arr, 4)
std::io::print(arr[0], ",", arr[1], ",", arr[3], " ", total(&arr), " ", std::array::sum(arr))
"#;
    assert_eq!(run(source).unwrap(), ["0,2,6 12 12"]);
}

#[test]
fn recursion_passes_the_same_view_down_to_every_call() {
    let source = r#"
proc reverse(a: &mut []i64, lo: i64, hi: i64) {
    if lo >= hi { return }
    let t = a[lo]
    a[lo] = a[hi]
    a[hi] = t
    reverse(a, lo + 1, hi - 1)
}
let mut arr = [1, 2, 3, 4, 5]
reverse(&mut arr, 0, 4)
std::io::print(arr[0], arr[1], arr[2], arr[3], arr[4])
"#;
    assert_eq!(run(source).unwrap(), ["54321"]);
}

#[test]
fn an_index_outside_the_view_fails_with_its_position() {
    let source = r#"
proc poke(a: &mut []i64, i: i64) { a[i] = 1 }
let mut arr = [0, 0, 0]
poke(&mut arr, 3)
"#;
    assert!(matches!(
        run(source),
        Err(RunError::IndexOutOfBounds { .. })
    ));
    let negative = "proc poke(a: &mut []i64, i: i64) { a[i] = 1 }\nlet mut arr = [0, 0, 0]\npoke(&mut arr, -1)\n";
    assert!(matches!(
        run(negative),
        Err(RunError::IndexOutOfBounds { .. })
    ));
}

#[test]
fn slices_survive_one_instruction_slices_and_the_sum_checks_overflow() {
    let source = r#"
proc double(a: &mut []i64) {
    for p in 0..=std::array::len(a) - 1 { a[p] = a[p] * 2 }
}
let mut arr = [1, 2, 3]
double(&mut arr)
std::io::print(std::array::sum(arr))
"#;
    let (writer, lines) = sink();
    let mut activation = module(source).start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    while matches!(activation.resume(1), ExecutionStatus::Paused(_)) {}
    assert_eq!(*lines.lock().unwrap(), ["12"]);
    let overflow = "let arr = [9223372036854775807, 1]\nstd::io::print(std::array::sum(arr))\n";
    assert!(matches!(
        run(overflow),
        Err(RunError::IntegerOverflow { .. })
    ));
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, Instruction, InvalidBytecode, LoanAccess,
            LoanOrigin, ModuleImage, Register, RegisterType, ScalarType, Shape, ShapeId, ValueType,
            encode,
        },
    };

    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn kind(
        shapes: Vec<Shape>,
        registers: Vec<RegisterType>,
        code: Vec<Instruction>,
    ) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: shapes.into(),
            signatures: Box::new([]),
            globals: Box::new([]),
            managed: Box::new([]),
            traits: Box::new([]),
            implementations: Box::new([]),
            functions: Box::new([FunctionImage {
                locals: Box::new([]),
                frozen: Box::new([]),
                contracts: Box::new([]),
                generator: None,
                registers: registers.into(),
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

    fn array(lower: i64) -> Shape {
        Shape::Array {
            element: I64,
            bounds: Box::new([(lower, lower + 2)]),
        }
    }

    #[test]
    fn a_slice_cannot_be_held_by_value() {
        let registers = vec![RegisterType::Composite(ShapeId(0))];
        assert!(matches!(
            kind(
                vec![Shape::Slice { element: I64 }],
                registers,
                vec![Return(None)]
            ),
            InvalidBytecode::UnsizedValue(_)
        ));
    }

    #[test]
    fn only_arrays_that_start_at_zero_can_be_viewed() {
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Loan {
                access: LoanAccess::Shared,
                origin: LoanOrigin::Local(Register(0)),
                pointee: ValueType::Composite(ShapeId(0)),
            },
            RegisterType::Loan {
                access: LoanAccess::Shared,
                origin: LoanOrigin::Local(Register(0)),
                pointee: ValueType::Composite(ShapeId(1)),
            },
        ];
        let code = vec![
            StorageLive(Register(0)),
            ArrayDeclare(Register(0)),
            StorageLive(Register(1)),
            Borrow(Register(1), Register(0)),
            StorageLive(Register(2)),
            SliceOf(Register(2), Register(1)),
            Return(None),
        ];
        let shapes = vec![array(1), Shape::Slice { element: I64 }];
        assert!(matches!(
            kind(shapes, registers, code),
            InvalidBytecode::SliceSource(_)
        ));
    }
}
