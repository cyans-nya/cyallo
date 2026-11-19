//! Vectors, ranges and cells in the verified interpreter. Expected text is
//! written by hand from the language contract.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, HeapStats, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 50_000_000,
    frames: 512,
};

fn run_with(source: &str, stress: bool) -> Result<(Vec<String>, HeapStats), RunError> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap_or_else(|error| panic!("{error}"));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    let mut activation = module.start(&[], LIMITS).unwrap();
    activation.set_output(writer);
    activation.set_gc_stress(stress);
    loop {
        match activation.resume(1_000_000) {
            ExecutionStatus::Paused(_) => {}
            ExecutionStatus::Returned(_) => break,
            ExecutionStatus::Failed(error) => return Err(error),
        }
    }
    let lines = lines.lock().unwrap().clone();
    Ok((lines, activation.heap_stats()))
}

fn run(source: &str) -> Result<Vec<String>, RunError> {
    run_with(source, false).map(|(lines, _)| lines)
}

#[test]
fn a_vector_is_built_from_ranges_and_literals_and_grows() {
    let source = r#"
let mut v = std::collections::Vec::from(0..4)
let w = std::collections::Vec::from(3..=5)
let literal = std::collections::Vec[10, 20, 30]
let empty = std::collections::Vec<i64>[]
v.push(9)
v.push(8)
std::io::print(v.len(), " ", v[0], v[3], v[4], v[5])
std::io::print(w.len(), " ", w[0], w[2], " ", literal.len(), " ", literal[1], " ", empty.len())
std::io::print(std::array::sum(v), " ", std::array::sum(w), " ", std::array::sum(literal))
"#;
    // 0 + 1 + 2 + 3 + 9 + 8, 3 + 4 + 5, 10 + 20 + 30.
    assert_eq!(run(source).unwrap(), ["6 0398", "3 35 3 20 0", "23 12 60"]);
}

#[test]
fn elements_are_written_in_place_and_an_index_past_the_end_fails() {
    let source = r#"
let mut v = std::collections::Vec::from(0..3)
v[1] = 50
v.push(7)
std::io::print(v[0], ",", v[1], ",", v[3])
"#;
    assert_eq!(run(source).unwrap(), ["0,50,7"]);
    for index in ["3", "-1"] {
        let failing =
            format!("let mut v = std::collections::Vec::from(0..3)\nstd::io::print(v[{index}])\n");
        assert!(
            matches!(run(&failing), Err(RunError::IndexOutOfBounds { .. })),
            "{index}"
        );
    }
}

#[test]
fn a_vector_lent_to_a_routine_can_grow_there_and_be_sorted_through_a_slice() {
    let source = r#"
proc fill(v: &mut std::collections::Vec<i64>, n: i64) {
    let mut i = 0
    while i < n {
        v.push(n - i)
        i = i + 1
    }
}
proc bubble(a: &mut []i64) {
    let n = std::array::len(a)
    let mut pass = 0
    while pass < n {
        let mut i = 0
        while i < n - 1 - pass {
            if a[i] > a[i + 1] {
                let t = a[i]
                a[i] = a[i + 1]
                a[i + 1] = t
            }
            i = i + 1
        }
        pass = pass + 1
    }
}
let mut v = std::collections::Vec<i64>[]
fill(&mut v, 6)
bubble(&mut v)
std::io::print(v[0], v[1], v[2], v[3], v[4], v[5], " ", v.len())
"#;
    assert_eq!(run(source).unwrap(), ["123456 6"]);
}

#[test]
fn the_documented_sorting_and_knapsack_examples_print_their_results() {
    for (name, source) in [
        ("mergesort", include_str!("../examples/mergesort.ya")),
        ("quicksort", include_str!("../examples/quicksort.ya")),
        ("knapsack", include_str!("../examples/knapsack.ya")),
    ] {
        let lines = run(source).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!lines.is_empty(), "{name}");
    }
}

#[test]
fn a_cell_changes_through_a_shared_reference_and_replace_returns_the_old_value() {
    let source = r#"
use std::cell::Cell
proc bump(c: &Cell<i64>) { c.*.* = c.*.* + 1 }
let c = Cell::new(5)
bump(&c)
bump(&c)
let old = c.replace(100)
std::io::print(old, " ", c.*)
"#;
    assert_eq!(run(source).unwrap(), ["7 100"]);
}

#[test]
fn collected_cycles_and_shared_counters_behave_like_the_documented_examples() {
    let cycles = include_str!("../examples/gc-cycles.ya");
    for stress in [false, true] {
        let (lines, stats) = run_with(cycles, stress).unwrap();
        assert_eq!(lines, ["cycle: 10 -> 20 -> 10", "temporary cycles: 128"]);
        assert!(stats.collections > 0, "{stats:?}");
        // Unreachable cycles are reclaimed, so the heap stays small.
        assert!(stats.peak < 300, "cycles were not reclaimed: {stats:?}");
    }
    let (_, stressed) = run_with(cycles, true).unwrap();
    assert!(stressed.live <= 16, "{stressed:?}");
    let sharing = include_str!("../examples/gc-sharing.ya");
    let (lines, _) = run_with(sharing, true).unwrap();
    assert_eq!(lines, ["snapshot: 10", "shared: 11:11", "retained: 21"]);
}

#[test]
fn ranges_and_growth_are_checked() {
    let overflow =
        "let v = std::collections::Vec::from(0..=9223372036854775807)\nstd::io::print(v.len())\n";
    assert!(matches!(
        run(overflow),
        Err(RunError::IntegerOverflow { .. } | RunError::HeapExhausted { .. })
    ));
    let huge = "let v = std::collections::Vec::from(0..100000000000)\nstd::io::print(v.len())\n";
    assert!(matches!(run(huge), Err(RunError::HeapExhausted { .. })));
}

#[test]
fn collection_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let program = compile("let v = std::collections::Vec[1]\nstd::io::print(v.len())\n").unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
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
                generator: None,
                registers: registers.into(),
                parameters: Box::new([]),
                result: None,
                calls: Box::new([]),
                lists: Box::new([Box::new([])]),
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
    fn a_push_through_a_shared_loan_is_rejected() {
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Loan {
                access: LoanAccess::Shared,
                origin: LoanOrigin::Local(Register(0)),
                pointee: ValueType::Composite(ShapeId(0)),
            },
            RegisterType::Scalar(ScalarType::I64),
        ];
        let code = vec![
            StorageLive(Register(0)),
            VecNew(Register(0), cyallo::bytecode::verified::OperandListId(0)),
            StorageLive(Register(1)),
            Borrow(Register(1), Register(0)),
            StorageLive(Register(2)),
            Constant(Register(2), Scalar::I64(1)),
            VecPush(Register(1), Register(2)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![Shape::Vec { element: I64 }], registers, code),
            InvalidBytecode::ExpectedExclusiveLoan(_)
        ));
    }

    #[test]
    fn a_cell_holds_one_declared_type() {
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Scalar(ScalarType::Bool),
        ];
        let code = vec![
            StorageLive(Register(1)),
            Constant(Register(1), Scalar::Bool(true)),
            StorageLive(Register(0)),
            CellNew(Register(0), Register(1)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![Shape::Cell { element: I64 }], registers, code),
            InvalidBytecode::ValueType { .. }
        ));
    }

    #[test]
    fn a_vector_operation_on_a_non_vector_is_rejected() {
        let registers = vec![
            RegisterType::Composite(ShapeId(0)),
            RegisterType::Scalar(ScalarType::U64),
        ];
        let code = vec![
            StorageLive(Register(0)),
            VecNew(Register(0), cyallo::bytecode::verified::OperandListId(0)),
            StorageLive(Register(1)),
            VecLength(Register(1), Register(0)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![Shape::Cell { element: I64 }], registers, code),
            InvalidBytecode::ExpectedVec(_)
        ));
    }
}
