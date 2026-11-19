//! Loans of composite values and projections into them, in the verified
//! interpreter. Expected text is written by hand from the language contract.

use cyallo::{
    bytecode::verified::{ExecutionLimits, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1_000_000,
    frames: 64,
};

fn run(source: &str) -> Result<Vec<String>, RunError> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap_or_else(|error| panic!("{error}"));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    module.run_to(&[], LIMITS, sink)?;
    let lines = lines.lock().unwrap().clone();
    Ok(lines)
}

#[test]
fn reference_parameters_read_and_write_fields_in_place() {
    let source = r#"
record Point { x: f64
 y: f64 }
proc bump(p: &mut Point) { p.x += 1.0 }
fn norm(p: &Point) -> f64 { return p.x * p.x + p.y * p.y }
proc swap(t: &mut (i64, i64)) {
    let a = t.0
    t.0 = t.1
    t.1 = a
}
let mut pt = Point { x: 3.0, y: 4.0 }
bump(&mut pt)
let mut t = (1, 2)
t.0 = 9
swap(&mut t)
std::io::print(norm(&pt), " ", t.0, t.1)
"#;
    assert_eq!(run(source).unwrap(), ["32 29"]);
}

#[test]
fn writes_reach_nested_fields_of_locals_and_loans() {
    let source = r#"
record Point { x: f64
 y: f64 }
record Segment { from: Point
 to: Point }
proc stretch(s: &mut Segment) { s.to.x = s.to.x * 2.0 }
let mut s = Segment { from: Point { x: 0.0, y: 0.0 }, to: Point { x: 3.0, y: 4.0 } }
s.from.y = 1.5
stretch(&mut s)
std::io::print(s.from.x, ",", s.from.y, ",", s.to.x, ",", s.to.y)
"#;
    assert_eq!(run(source).unwrap(), ["0,1.5,6,4"]);
}

#[test]
fn whole_values_of_any_type_are_borrowed_read_and_replaced() {
    let source = r#"
proc shout(s: &mut string) { s.* = s.* + "!" }
fn length(s: &string) -> i64 { return std::string::len(s.*) }
proc reset(t: &mut (i64, bool)) { t.* = (0, false) }
let mut word = "hey"
shout(&mut word)
shout(&mut word)
std::io::print(word, " ", length(&word))
let mut pair = (5, true)
reset(&mut pair)
std::io::print(pair.0, pair.1)
"#;
    assert_eq!(run(source).unwrap(), ["hey!! 5", "0false"]);
}

#[test]
fn a_loan_survives_one_instruction_slices_and_nested_calls() {
    let source = r#"
proc inc(n: &mut i64) { n.* += 1 }
proc inc_twice(p: &mut (i64, i64)) {
    inc(&mut p.0)
    inc(&mut p.0)
    inc(&mut p.1)
}
let mut pair = (10, 20)
inc_twice(&mut pair)
std::io::print(pair.0, ",", pair.1)
"#;
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    let mut activation = module.start(&[], LIMITS).unwrap();
    activation.set_output(sink);
    while matches!(
        activation.resume(1),
        cyallo::bytecode::verified::ExecutionStatus::Paused(_)
    ) {}
    assert_eq!(*lines.lock().unwrap(), ["12,21"]);
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, Instruction, InvalidBytecode, LoanAccess,
            LoanOrigin, ModuleImage, Register, RegisterType, Scalar, ScalarType, Shape, ShapeId,
            ValueType, encode,
        },
    };

    const OWNER: Register = Register(0);
    const LOAN: Register = Register(1);
    const FIELD: Register = Register(2);
    const VALUE: Register = Register(3);
    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn pair() -> Shape {
        Shape::Tuple(Box::new([I64, I64]))
    }

    fn loan(access: LoanAccess, pointee: ValueType, origin: LoanOrigin) -> RegisterType {
        RegisterType::Loan {
            access,
            origin,
            pointee,
        }
    }

    fn kind(registers: Vec<RegisterType>, code: Vec<Instruction>) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([pair()]),
            signatures: Box::new([]),
            functions: Box::new([FunctionImage {
                registers: registers.into(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([]),
                lists: Box::new([Box::new([VALUE, VALUE])]),
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

    fn registers(field_access: LoanAccess, field_pointee: ValueType) -> Vec<RegisterType> {
        vec![
            RegisterType::Composite(ShapeId(0)),
            loan(
                LoanAccess::Shared,
                ValueType::Composite(ShapeId(0)),
                LoanOrigin::Local(OWNER),
            ),
            loan(field_access, field_pointee, LoanOrigin::Local(OWNER)),
            RegisterType::Scalar(ScalarType::I64),
        ]
    }

    fn prologue() -> Vec<Instruction> {
        vec![
            StorageLive(VALUE),
            Constant(VALUE, Scalar::I64(1)),
            StorageLive(OWNER),
            Tuple(OWNER, cyallo::bytecode::verified::OperandListId(0)),
            StorageLive(LOAN),
            Borrow(LOAN, OWNER),
            StorageLive(FIELD),
        ]
    }

    #[test]
    fn a_field_loan_must_name_the_field_type() {
        let mut code = prologue();
        code.extend([LoanField(FIELD, LOAN, 0), Return(None)]);
        let bool_pointee = ValueType::Scalar(ScalarType::Bool);
        assert!(matches!(
            kind(registers(LoanAccess::Shared, bool_pointee), code),
            InvalidBytecode::ValueType { .. }
        ));
    }

    #[test]
    fn an_exclusive_projection_cannot_come_from_a_shared_loan() {
        let mut code = prologue();
        code.extend([LoanField(FIELD, LOAN, 0), Return(None)]);
        assert!(matches!(
            kind(registers(LoanAccess::Exclusive, I64), code),
            InvalidBytecode::LoanOwnerMismatch { .. }
        ));
    }

    #[test]
    fn a_field_beyond_the_tuple_is_rejected() {
        let mut code = prologue();
        code.extend([LoanField(FIELD, LOAN, 5), Return(None)]);
        assert!(matches!(
            kind(registers(LoanAccess::Shared, I64), code),
            InvalidBytecode::UnknownField { .. }
        ));
    }

    #[test]
    fn the_owner_cannot_be_read_while_an_exclusive_projection_lives() {
        let mut registers = registers(LoanAccess::Exclusive, I64);
        registers[1] = loan(
            LoanAccess::Exclusive,
            ValueType::Composite(ShapeId(0)),
            LoanOrigin::Local(OWNER),
        );
        let mut code = prologue();
        code.extend([
            LoanField(FIELD, LOAN, 0),
            Extract(VALUE, OWNER, 1),
            WriteLoan(FIELD, VALUE),
            Return(None),
        ]);
        assert!(matches!(
            kind(registers, code),
            InvalidBytecode::ExclusiveOwnerAccess(_)
        ));
    }
}
