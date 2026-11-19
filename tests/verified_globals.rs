//! Module-level variables in the verified interpreter. Expected text is
//! written by hand from the language contract.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 5_000_000,
    frames: 64,
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
fn routines_read_and_write_variables_of_the_main_program() {
    let source = r#"
let mut entries = 0
const LIMIT = 5
let mut label = "x"
proc bump() {
    entries += 1
    label = label + "!"
}
proc total() -> i64 { return entries * LIMIT }
bump()
bump()
std::io::print(total(), " ", label, " ", entries)
"#;
    assert_eq!(run(source).unwrap(), ["10 x!! 2"]);
}

#[test]
fn globals_hold_composites_and_support_field_writes() {
    let source = r#"
record Stats { hits: i64
 misses: i64 }
let mut stats = Stats { hits: 0, misses: 0 }
let mut scores = (1, 2)
proc hit() { stats.hits += 1 }
proc miss() { stats.misses += 10 }
proc swap_scores() {
    let a = scores.0
    scores.0 = scores.1
    scores.1 = a
}
hit()
hit()
miss()
swap_scores()
std::io::print(stats.hits, ",", stats.misses, ",", scores.0, scores.1)
"#;
    assert_eq!(run(source).unwrap(), ["2,10,21"]);
}

#[test]
fn a_global_can_be_lent_to_a_routine_and_survives_slices() {
    let source = r#"
let mut counter = 0
proc add(n: &mut i64, by: i64) { n.* += by }
proc work() {
    add(&mut counter, 3)
    add(&mut counter, 4)
}
work()
std::io::print(counter)
"#;
    let (writer, lines) = sink();
    let mut activation = module(source).start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    while matches!(activation.resume(1), ExecutionStatus::Paused(_)) {}
    assert_eq!(*lines.lock().unwrap(), ["7"]);
}

#[test]
fn a_routine_run_before_its_variable_is_assigned_fails_cleanly() {
    let source = "let mut early = 5\nproc read() -> i64 { return early }\nstd::io::print(read())\n";
    let program = compile(source).unwrap();
    let routine = program
        .functions()
        .iter()
        .position(|function| function.name.as_ref() == "read")
        .unwrap();
    let module = VerifiedModule::lower(&program, routine).unwrap();
    assert!(matches!(
        module.run(&[], LIMITS),
        Err(RunError::UninitializedGlobal { .. })
    ));
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, GlobalId, Instruction, InvalidBytecode,
            LoanAccess, LoanOrigin, ModuleImage, Register, RegisterType, ScalarType, ValueType,
            encode,
        },
    };

    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn kind(origin: LoanOrigin, pointee: ValueType, globals: Vec<ValueType>) -> InvalidBytecode {
        let code = vec![
            StorageLive(Register(0)),
            BorrowGlobal(Register(0), GlobalId(0)),
            Return(None),
        ];
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([]),
            globals: globals.into(),
            managed: Box::new([]),
            traits: Box::new([]),
            implementations: Box::new([]),
            functions: Box::new([FunctionImage {
                locals: Box::new([]),
                frozen: Box::new([]),
                contracts: Box::new([]),
                generator: None,
                registers: Box::new([RegisterType::Loan {
                    access: LoanAccess::Exclusive,
                    origin,
                    pointee,
                }]),
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

    #[test]
    fn a_global_borrow_names_a_declared_variable_and_its_type() {
        assert!(matches!(
            kind(LoanOrigin::Global(GlobalId(0)), I64, vec![]),
            InvalidBytecode::UnknownGlobal(_)
        ));
        assert!(matches!(
            kind(LoanOrigin::Global(GlobalId(0)), ValueType::Text, vec![I64]),
            InvalidBytecode::ValueType { .. }
        ));
        assert!(matches!(
            kind(LoanOrigin::Global(GlobalId(1)), I64, vec![I64, I64]),
            InvalidBytecode::LoanOwnerMismatch { .. }
        ));
    }
}
