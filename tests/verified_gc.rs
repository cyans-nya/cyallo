//! Garbage-collected handles in the verified interpreter. Expected text is
//! written by hand from the language contract.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, HeapStats, OutputSink, RunError, VerifiedModule,
    },
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

/// Run to completion and return the output lines with the final heap stats.
fn run_with(source: &str, stress: bool) -> Result<(Vec<String>, HeapStats), RunError> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    let mut activation = module(source).start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    activation.set_gc_stress(stress);
    loop {
        match activation.resume(100_000) {
            ExecutionStatus::Paused(_) => {}
            ExecutionStatus::Returned(_) => break,
            ExecutionStatus::Failed(error) => return Err(error),
        }
    }
    let lines = lines.lock().unwrap().clone();
    Ok((lines, activation.heap_stats()))
}

const LIST: &str = r#"
enum List<T> { End, Node: (T, gc List<T>) }
proc sum(value: &List<i64>) -> i64 {
    return match value {
        List::End => 0,
        List::Node((number, next)) => number.* + sum(&next.*.*),
    }
}
"#;

#[test]
fn a_recursive_list_is_built_on_the_heap_and_summed() {
    let source = format!(
        "{LIST}let end: gc List<i64> = gc List::End\nlet list = gc List::Node((3, gc List::Node((2, end))))\nstd::io::print(sum(&list.*))\n"
    );
    for stress in [false, true] {
        assert_eq!(run_with(&source, stress).unwrap().0, ["5"]);
    }
}

#[test]
fn aliases_copy_the_handle_and_read_the_same_object() {
    let source = r#"
let a = gc 41
let b = a
std::io::print(a.*, " ", b.*, " ", a.* + b.*)
"#;
    assert_eq!(run_with(source, true).unwrap().0, ["41 41 82"]);
}

#[test]
fn temporaries_are_reclaimed_while_the_program_keeps_running() {
    let source = r#"
proc churn(n: i64) -> i64 {
    let mut total = 0
    let mut i = 0
    while i < n {
        let temporary = gc i
        total = total + temporary.*
        i = i + 1
    }
    return total
}
std::io::print(churn(5000))
"#;
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(lines, ["12497500"]);
    assert!(stats.collections > 0, "{stats:?}");
    assert!(stats.peak < 600, "peak {}", stats.peak);
    assert!(stats.live < 600, "live {}", stats.live);
}

#[test]
fn what_a_register_holds_survives_every_collection() {
    let source = format!(
        "{LIST}proc build(n: i64) -> gc List<i64> {{\n    let mut list: gc List<i64> = gc List::End\n    let mut i = 1\n    while i <= n {{\n        list = gc List::Node((i, list))\n        i = i + 1\n    }}\n    return list\n}}\nlet list = build(300)\nstd::io::print(sum(&list.*))\n"
    );
    // 1 + 2 + ... + 300, with a collection before every allocation.
    assert_eq!(run_with(&source, true).unwrap().0, ["45150"]);
}

#[test]
fn an_abandoned_chain_is_reclaimed_through_its_handles() {
    let source = format!(
        "{LIST}proc chain_sum(n: i64) -> i64 {{\n    let mut list: gc List<i64> = gc List::End\n    let mut i = 1\n    while i <= n {{\n        list = gc List::Node((i, list))\n        i = i + 1\n    }}\n    return sum(&list.*)\n}}\nproc churn(rounds: i64) -> i64 {{\n    let mut total = 0\n    let mut r = 0\n    while r < rounds {{\n        total = total + chain_sum(50)\n        r = r + 1\n    }}\n    return total\n}}\nstd::io::print(churn(100))\n"
    );
    let (lines, stats) = run_with(&source, false).unwrap();
    // 100 rounds of 1 + ... + 50.
    assert_eq!(lines, ["127500"]);
    assert!(stats.collections > 0, "{stats:?}");
    assert!(stats.peak < 800, "chains were not reclaimed: {stats:?}");
    let (_, stressed) = run_with(&source, true).unwrap();
    assert!(stressed.live <= 51, "{stressed:?}");
}

#[test]
fn collection_survives_one_instruction_slices() {
    let source = format!(
        "{LIST}let end: gc List<i64> = gc List::End\nlet list = gc List::Node((3, gc List::Node((2, end))))\nstd::io::print(sum(&list.*))\n"
    );
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    let mut activation = module(&source).start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    activation.set_gc_stress(true);
    while matches!(activation.resume(1), ExecutionStatus::Paused(_)) {}
    assert_eq!(*lines.lock().unwrap(), ["5"]);
}

#[test]
fn heap_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let module = module("let h = gc 1\nstd::io::print(h.*)\n");
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, Instruction, InvalidBytecode, LoanAccess,
            LoanOrigin, ManagedId, ModuleImage, Register, RegisterType, Scalar, ScalarType, Shape,
            ShapeId, ValueType, encode,
        },
    };

    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn image(
        shapes: Vec<Shape>,
        managed: Vec<ValueType>,
        registers: Vec<RegisterType>,
        code: Vec<Instruction>,
    ) -> ModuleImage {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: shapes.into(),
            signatures: Box::new([]),
            globals: Box::new([]),
            managed: managed.into(),
            traits: Box::new([]),
            implementations: Box::new([]),
            functions: Box::new([FunctionImage {
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
    }

    fn kind(image: ModuleImage) -> InvalidBytecode {
        image.verify().unwrap_err().kind
    }

    #[test]
    fn a_composite_cannot_contain_itself_by_value_but_can_through_a_handle() {
        let by_value = Shape::Tuple(Box::new([ValueType::Composite(ShapeId(0))]));
        assert!(matches!(
            kind(image(vec![by_value], vec![], vec![], vec![Return(None)])),
            InvalidBytecode::ShapeCycle(_)
        ));
        let through_handle = Shape::Tuple(Box::new([I64, ValueType::Gc(ManagedId(0))]));
        let ok = image(
            vec![through_handle],
            vec![ValueType::Composite(ShapeId(0))],
            vec![],
            vec![Return(None)],
        );
        assert!(ok.verify().is_ok());
    }

    #[test]
    fn a_handle_to_an_unknown_managed_type_is_rejected() {
        let registers = vec![RegisterType::Gc(ManagedId(3))];
        assert!(matches!(
            kind(image(vec![], vec![], registers, vec![Return(None)])),
            InvalidBytecode::UnknownManaged(_)
        ));
    }

    #[test]
    fn allocation_checks_the_stored_type() {
        let registers = vec![
            RegisterType::Gc(ManagedId(0)),
            RegisterType::Scalar(ScalarType::Bool),
        ];
        let code = vec![
            StorageLive(Register(1)),
            Constant(Register(1), Scalar::Bool(true)),
            StorageLive(Register(0)),
            GcNew(Register(0), Register(1)),
            Return(None),
        ];
        assert!(matches!(
            kind(image(vec![], vec![I64], registers, code)),
            InvalidBytecode::ValueType { .. }
        ));
    }

    #[test]
    fn a_borrow_of_a_heap_object_names_its_managed_type() {
        let registers = vec![
            RegisterType::Gc(ManagedId(0)),
            RegisterType::Loan {
                access: LoanAccess::Shared,
                origin: LoanOrigin::Heap(ManagedId(1)),
                pointee: I64,
            },
        ];
        let code = vec![
            StorageLive(Register(0)),
            StorageLive(Register(1)),
            GcBorrow(Register(1), Register(0)),
            Return(None),
        ];
        assert!(matches!(
            kind(image(vec![], vec![I64, I64], registers, code)),
            InvalidBytecode::LoanOwnerMismatch { .. }
        ));
    }
}
