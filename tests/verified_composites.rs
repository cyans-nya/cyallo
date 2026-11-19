//! Tuples and enums in the verified interpreter. Expected lines are written
//! by hand from the language contract.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, InvalidBytecode, OutputSink, RunError, Scalar, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 100_000,
    frames: 64,
};

fn run(source: &str) -> Result<Vec<String>, RunError> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap_or_else(|error| {
        panic!("{error}\n{source}");
    });
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    module.run_to(&[], LIMITS, sink)?;
    let lines = lines.lock().unwrap().clone();
    Ok(lines)
}

const SHAPES: &str = r#"
enum Shape { Empty, Circle: i64, Rect: (i64, i64) }
fn area(s: Shape) -> i64 {
    return match s {
        Shape::Empty => 0,
        Shape::Circle(r) => r * r * 3,
        Shape::Rect((w, h)) => w * h,
    }
}
"#;

#[test]
fn match_selects_each_variant_and_reads_nested_tuple_payloads() {
    let source = format!(
        "{SHAPES}std::io::print(area(Shape::Empty))\nstd::io::print(area(Shape::Circle(2)))\nstd::io::print(area(Shape::Rect((3, 4))))\n"
    );
    assert_eq!(run(&source).unwrap(), ["0", "12", "12"]);
}

#[test]
fn option_values_cross_calls_and_are_matched_by_the_caller() {
    let source = r#"
fn pick(n: i64) -> Option<i64> {
    if n > 0 { return Option::Some(n) }
    return Option::None
}
match pick(5) { Option::Some(v) => std::io::print(v), Option::None => std::io::print(-1) }
match pick(0) { Option::Some(v) => std::io::print(v), Option::None => std::io::print(-1) }
"#;
    assert_eq!(run(source).unwrap(), ["5", "-1"]);
}

#[test]
fn tuples_are_passed_returned_and_read_by_field() {
    let source = r#"
fn swap(p: (i64, i64)) -> (i64, i64) {
    return (p.1, p.0)
}
let t = swap((1, 2))
std::io::print(t.0)
std::io::print(t.1)
"#;
    assert_eq!(run(source).unwrap(), ["2", "1"]);
}

#[test]
fn an_enum_rebuilt_in_a_loop_keeps_one_frame_slot() {
    let source = r#"
enum Step { Stop, Go: i64 }
let mut total = 0
let mut i = 0
while i < 1000 {
    let step = Step::Go(i)
    match step {
        Step::Go(n) => { total = total + n }
        Step::Stop => { total = total }
    }
    i = i + 1
}
std::io::print(total)
"#;
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let before = module
        .function(module.entry())
        .unwrap()
        .frame_layout()
        .slot_count();
    assert_eq!(run(source).unwrap(), ["499500"]);
    // The composite register's storage is released and reused each iteration.
    assert!(before < 16, "frame grew to {before} slots");
}

#[test]
fn a_routine_returning_a_composite_can_be_inspected_but_not_started() {
    let program = compile("fn make() -> Option<i64> { return Option::Some(1) }\n").unwrap();
    let entry = program
        .functions()
        .iter()
        .position(|function| function.name.as_ref() == "make")
        .unwrap();
    let module = VerifiedModule::lower(&program, entry).unwrap();
    assert!(module.disassemble().contains("enum.wrap"));
    assert!(matches!(
        module.start(&[], LIMITS),
        Err(RunError::EntryResultNotScalar)
    ));
}

#[test]
fn composite_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PrepareError, PreparedFunction};
    let program = compile(&format!("{SHAPES}std::io::print(area(Shape::Circle(1)))\n")).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let error =
        PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).unwrap_err();
    assert!(matches!(
        error,
        PrepareError::NonScalar(_) | PrepareError::UnsupportedOperation { .. }
    ));
    let _ = (Scalar::I64(0), InvalidBytecode::EmptyFunction);
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            ExecutionLimits, FORMAT_VERSION, FunctionId, FunctionImage, Instruction,
            InvalidBytecode, ModuleImage, OperandListId, Register, RegisterType, RunError,
            ScalarType, Shape, ShapeId, ValueType, encode,
        },
    };

    const R0: Register = Register(0);
    const R1: Register = Register(1);

    fn image(
        shapes: Vec<Shape>,
        registers: Vec<RegisterType>,
        lists: Vec<Box<[Register]>>,
        code: Vec<Instruction>,
    ) -> ModuleImage {
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
                generator: None,
                registers: registers.into(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([]),
                lists: lists.into(),
                texts: Box::new([]),
                outputs: Box::new([]),
                spans: vec![Span::new(0, 1); code.len()].into(),
                words: encode(code),
                statements: Box::new([]),
            }]),
        }
    }

    fn maybe_int() -> Shape {
        Shape::Enum(Box::new([None, Some(ScalarType::I64.into())]))
    }

    fn enum_registers() -> Vec<RegisterType> {
        vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Scalar(ScalarType::I64),
        ]
    }

    fn kind(image: ModuleImage) -> InvalidBytecode {
        image.verify().unwrap_err().kind
    }

    #[test]
    fn reading_the_payload_of_another_variant_fails_with_its_position() {
        let module = image(
            vec![maybe_int()],
            enum_registers(),
            vec![],
            vec![
                StorageLive(R0),
                EnumUnit(R0, 0),
                StorageLive(R1),
                EnumPayload(R1, R0, 1),
                StorageDead(R1),
                StorageDead(R0),
                Return(None),
            ],
        )
        .verify()
        .unwrap();
        let limits = ExecutionLimits {
            fuel: 20,
            frames: 1,
        };
        assert!(matches!(
            module.run(&[], limits),
            Err(RunError::EnumVariant { instruction, .. }) if instruction.0 == 3
        ));
    }

    #[test]
    fn a_unit_constructor_for_a_payload_variant_is_rejected() {
        let code = vec![
            StorageLive(R0),
            EnumUnit(R0, 1),
            StorageDead(R0),
            Return(None),
        ];
        assert!(matches!(
            kind(image(vec![maybe_int()], enum_registers(), vec![], code)),
            InvalidBytecode::ArgumentCount { .. }
        ));
    }

    #[test]
    fn an_unknown_variant_index_is_rejected() {
        let code = vec![
            StorageLive(R0),
            EnumUnit(R0, 9),
            StorageDead(R0),
            Return(None),
        ];
        assert!(matches!(
            kind(image(vec![maybe_int()], enum_registers(), vec![], code)),
            InvalidBytecode::UnknownVariant { variant: 9, .. }
        ));
    }

    #[test]
    fn a_tuple_field_of_the_wrong_type_is_rejected() {
        let shape = Shape::Tuple(Box::new([ValueType::Scalar(ScalarType::Bool)]));
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Scalar(ScalarType::I64),
        ];
        let code = vec![
            StorageLive(R1),
            Constant(R1, cyallo::bytecode::verified::Scalar::I64(1)),
            StorageLive(R0),
            Tuple(R0, OperandListId(0)),
            StorageDead(R0),
            StorageDead(R1),
            Return(None),
        ];
        assert!(matches!(
            kind(image(vec![shape], registers, vec![Box::new([R1])], code)),
            InvalidBytecode::ValueType { .. }
        ));
    }

    #[test]
    fn a_shape_cannot_contain_itself_or_a_later_shape() {
        let shape = Shape::Tuple(Box::new([ValueType::Composite(ShapeId(0))]));
        let code = vec![Return(None)];
        assert!(matches!(
            kind(image(vec![shape], vec![], vec![], code)),
            InvalidBytecode::ShapeCycle(_)
        ));
    }

    #[test]
    fn a_register_of_an_unknown_shape_is_rejected() {
        let registers = vec![RegisterType::Composite(ShapeId(3))];
        assert!(matches!(
            kind(image(vec![], registers, vec![], vec![Return(None)])),
            InvalidBytecode::UnknownShape(_)
        ));
    }
}
