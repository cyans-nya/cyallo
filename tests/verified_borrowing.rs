//! Closures that capture loans, and calls through loans to closures, in the
//! verified interpreter. Expected text is written by hand.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1_000_000,
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
fn a_closure_borrowing_a_local_runs_through_a_reference_parameter() {
    let source = r#"
proc twice(action: &mut proc()) {
    action()
    action()
}
proc count_twice() -> i64 {
    let mut count = 0
    let mut increment = proc[&mut count]() { count += 1 }
    twice(&mut increment)
    return count
}
std::io::print(count_twice())
"#;
    assert_eq!(run(source).unwrap(), ["2"]);
}

#[test]
fn the_borrowed_local_is_readable_again_once_the_closure_is_done() {
    let source = r#"
proc times(action: &mut proc(), n: i64) {
    let mut i = 0
    while i < n {
        action()
        i = i + 1
    }
}
proc main_work() -> i64 {
    let mut total = 10
    let mut add = proc[&mut total]() { total += 5 }
    times(&mut add, 3)
    let after = total
    let mut add_again = proc[&mut total]() { total += 1 }
    times(&mut add_again, 2)
    return after + total
}
std::io::print(main_work())
"#;
    // 10 + 3 * 5 = 25, then 25 + 2 = 27: 25 + 27.
    assert_eq!(run(source).unwrap(), ["52"]);
}

#[test]
fn borrowing_closures_survive_one_instruction_slices() {
    let source = r#"
proc twice(action: &mut proc()) {
    action()
    action()
}
proc count_twice() -> i64 {
    let mut count = 0
    let mut increment = proc[&mut count]() { count += 1 }
    twice(&mut increment)
    return count
}
std::io::print(count_twice())
"#;
    let (writer, lines) = sink();
    let mut activation = module(source).start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    while matches!(activation.resume(1), ExecutionStatus::Paused(_)) {}
    assert_eq!(*lines.lock().unwrap(), ["2"]);
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            CallSite, CallSiteId, CallTarget, FORMAT_VERSION, FunctionId, FunctionImage,
            Instruction, InvalidBytecode, LoanAccess, LoanOrigin, ModuleImage, OperandListId,
            Register, RegisterType, ScalarType, Signature, SignatureId, ValueType, encode,
        },
    };

    const COUNT: Register = Register(0);
    const LOAN: Register = Register(1);
    const CLOSURE: Register = Register(2);
    const OTHER: Register = Register(3);
    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn function(
        registers: Vec<RegisterType>,
        parameters: Vec<Register>,
        result: Option<ValueType>,
        calls: Vec<CallSite>,
        code: Vec<Instruction>,
    ) -> FunctionImage {
        FunctionImage {
            registers: registers.into(),
            parameters: parameters.into(),
            result,
            calls: calls.into(),
            lists: Box::new([Box::new([LOAN])]),
            texts: Box::new([]),
            outputs: Box::new([]),
            spans: vec![Span::new(0, 1); code.len()].into(),
            words: encode(code),
            statements: Box::new([]),
        }
    }

    /// `f1(count: &mut i64)`: the closure routine.
    fn routine() -> FunctionImage {
        function(
            vec![RegisterType::Loan {
                access: LoanAccess::Exclusive,
                origin: LoanOrigin::Parameter { register: COUNT },
                pointee: I64,
            }],
            vec![COUNT],
            None,
            vec![],
            vec![StorageDead(COUNT), Return(None)],
        )
    }

    fn borrowing(origin: LoanOrigin) -> RegisterType {
        RegisterType::BorrowingFunction {
            signature: SignatureId(0),
            access: LoanAccess::Exclusive,
            origin,
        }
    }

    fn kind(
        registers: Vec<RegisterType>,
        result: Option<ValueType>,
        code: Vec<Instruction>,
    ) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([Signature {
                parameters: Box::new([]),
                result: None,
            }]),
            globals: Box::new([]),
            managed: Box::new([]),
            functions: Box::new([function(registers, vec![], result, vec![], code), routine()]),
        }
        .verify()
        .unwrap_err()
        .kind
    }

    fn registers(closure: RegisterType) -> Vec<RegisterType> {
        vec![
            RegisterType::Scalar(ScalarType::I64),
            RegisterType::Loan {
                access: LoanAccess::Exclusive,
                origin: LoanOrigin::Local(COUNT),
                pointee: I64,
            },
            closure,
            RegisterType::Scalar(ScalarType::I64),
        ]
    }

    fn setup() -> Vec<Instruction> {
        vec![
            StorageLive(COUNT),
            Constant(COUNT, cyallo::bytecode::verified::Scalar::I64(0)),
            StorageLive(LOAN),
            Borrow(LOAN, COUNT),
            StorageLive(CLOSURE),
            Closure(CLOSURE, FunctionId(1), OperandListId(0)),
        ]
    }

    #[test]
    fn a_borrowing_closure_cannot_be_returned() {
        let mut code = setup();
        code.push(Return(Some(CLOSURE)));
        let escaping = function(
            registers(borrowing(LoanOrigin::Local(COUNT))),
            vec![],
            Some(ValueType::Function(SignatureId(0))),
            vec![],
            code,
        );
        let module = ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([Signature {
                parameters: Box::new([]),
                result: None,
            }]),
            globals: Box::new([]),
            managed: Box::new([]),
            functions: Box::new([
                function(vec![], vec![], None, vec![], vec![Return(None)]),
                routine(),
                escaping,
            ]),
        };
        assert!(matches!(
            module.verify().unwrap_err().kind,
            InvalidBytecode::EscapingBorrow(_)
        ));
    }

    #[test]
    fn the_borrowed_owner_cannot_end_while_the_closure_lives() {
        let mut code = setup();
        code.extend([StorageDead(COUNT), Return(None)]);
        assert!(matches!(
            kind(registers(borrowing(LoanOrigin::Local(COUNT))), None, code),
            InvalidBytecode::BorrowedOwner(_)
        ));
    }

    #[test]
    fn a_closure_cannot_claim_a_root_other_than_its_captures() {
        let code = setup();
        let mut all = registers(borrowing(LoanOrigin::Local(OTHER)));
        all[3] = RegisterType::Scalar(ScalarType::I64);
        let mut code = code;
        code.push(Return(None));
        assert!(matches!(
            kind(all, None, code),
            InvalidBytecode::LoanOwnerMismatch { .. }
        ));
    }

    #[test]
    fn the_owner_cannot_be_read_while_an_exclusive_closure_lives() {
        let mut code = setup();
        code.extend([
            StorageLive(OTHER),
            Copy(OTHER, COUNT),
            Call(CallSiteId(0)),
            Return(None),
        ]);
        let call = CallSite {
            target: CallTarget::Closure(CLOSURE),
            arguments: Box::new([]),
            destination: None,
        };
        let module = ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([Signature {
                parameters: Box::new([]),
                result: None,
            }]),
            globals: Box::new([]),
            managed: Box::new([]),
            functions: Box::new([
                function(
                    registers(borrowing(LoanOrigin::Local(COUNT))),
                    vec![],
                    None,
                    vec![call],
                    code,
                ),
                routine(),
            ]),
        };
        assert!(matches!(
            module.verify().unwrap_err().kind,
            InvalidBytecode::ExclusiveOwnerAccess(_)
        ));
    }
}
