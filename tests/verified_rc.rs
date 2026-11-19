//! Counted owners, weak references, moves and drops in the verified
//! interpreter. Expected text is written by hand from the language contract.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, HeapStats, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 20_000_000,
    frames: 256,
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
        match activation.resume(100_000) {
            ExecutionStatus::Paused(_) => {}
            ExecutionStatus::Returned(_) => break,
            ExecutionStatus::Failed(error) => return Err(error),
        }
    }
    let lines = lines.lock().unwrap().clone();
    Ok((lines, activation.heap_stats()))
}

#[test]
fn the_documented_ownership_example_prints_its_counts_and_leaves_nothing_counted() {
    let source = include_str!("../examples/rc-ownership.ya");
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(
        lines,
        [
            "strong: 2",
            "survivor: 42",
            "upgraded: 42:2",
            "expired: true"
        ]
    );
    assert_eq!(stats.counted, 0, "a counted object leaked: {stats:?}");
}

#[test]
fn clone_and_drop_move_the_strong_count_by_one() {
    let source = r#"
let a = rc 10
let b = std::rc::clone(&a)
let c = std::rc::clone(&b)
std::io::print(std::rc::strong_count(&a))
std::mem::drop(b)
std::io::print(std::rc::strong_count(&a))
std::mem::drop(c)
std::io::print(std::rc::strong_count(&a))
std::mem::drop(a)
"#;
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(lines, ["3", "2", "1"]);
    assert_eq!(stats.counted, 0);
}

#[test]
fn a_weak_reference_upgrades_only_while_an_owner_lives() {
    let source = r#"
let owner = rc 7
let weak = std::rc::downgrade(&owner)
let first = std::rc::upgrade(&weak)
std::io::print(first.is_some(), " ", std::rc::strong_count(&owner))
std::mem::drop(first)
std::io::print(std::rc::strong_count(&owner))
std::mem::drop(owner)
let second = std::rc::upgrade(&weak)
std::io::print(second.is_none())
std::mem::drop(second)
std::mem::drop(weak)
"#;
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(lines, ["true 2", "1", "true"]);
    assert_eq!(stats.counted, 0);
}

#[test]
fn a_payload_is_destroyed_with_its_last_owner_and_releases_what_it_owned() {
    let source = r#"
let inner = rc 5
let watch = std::rc::downgrade(&inner)
let pair = (std::rc::clone(&inner), 1)
let outer = rc pair
std::mem::drop(inner)
let alive = std::rc::upgrade(&watch)
std::io::print(alive.is_some())
std::mem::drop(alive)
std::mem::drop(outer)
let gone = std::rc::upgrade(&watch)
std::io::print(gone.is_none())
std::mem::drop(gone)
std::mem::drop(watch)
"#;
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(lines, ["true", "true"]);
    assert_eq!(stats.counted, 0);
}

#[test]
fn a_moved_owner_keeps_the_count_and_a_partial_move_drops_only_the_rest() {
    let source = include_str!("../examples/enums/ownership.ya");
    for stress in [false, true] {
        let (lines, stats) = run_with(source, stress).unwrap();
        assert_eq!(lines, ["first:1:1:true"]);
        assert_eq!(stats.counted, 0, "{stats:?}");
    }
}

#[test]
fn an_owner_keeps_the_heap_objects_in_its_payload_alive() {
    let source = r#"
let kept = rc (gc 7)
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
std::io::print(churn(3000))
std::io::print(kept.*.*)
std::mem::drop(kept)
"#;
    let (lines, stats) = run_with(source, false).unwrap();
    assert_eq!(lines, ["4498500", "7"]);
    assert!(stats.collections > 0);
    assert_eq!(stats.counted, 0);
}

#[test]
fn counted_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let program =
        compile("let a = rc 1\nstd::io::print(std::rc::strong_count(&a))\nstd::mem::drop(a)\n")
            .unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            FORMAT_VERSION, FunctionId, FunctionImage, Instruction, InvalidBytecode, LoanAccess,
            LoanOrigin, ManagedId, ModuleImage, Register, RegisterType, Scalar, ScalarType, Shape,
            ValueType, encode,
        },
    };

    const I64: ValueType = ValueType::Scalar(ScalarType::I64);
    const OWNER: Register = Register(0);
    const OTHER: Register = Register(1);
    const VALUE: Register = Register(2);

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
            managed: Box::new([I64]),
            traits: Box::new([]),
            implementations: Box::new([]),
            functions: Box::new([FunctionImage {
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

    fn owners() -> Vec<RegisterType> {
        vec![
            RegisterType::Rc(ManagedId(0)),
            RegisterType::Rc(ManagedId(0)),
            RegisterType::Scalar(ScalarType::I64),
        ]
    }

    fn setup() -> Vec<Instruction> {
        vec![
            StorageLive(VALUE),
            Constant(VALUE, Scalar::I64(1)),
            StorageLive(OWNER),
            RcNew(OWNER, VALUE),
            StorageLive(OTHER),
        ]
    }

    #[test]
    fn a_moved_owner_cannot_be_read_again() {
        let mut code = setup();
        code.extend([
            Move(OTHER, OWNER),
            Move(OWNER, OTHER),
            Move(OTHER, OWNER),
            Return(None),
        ]);
        // The third move is fine; reading the first source before it is set is not.
        let mut bad = setup();
        bad.extend([Move(OTHER, OWNER), Move(OTHER, OWNER), Return(None)]);
        assert!(matches!(
            kind(vec![], owners(), bad),
            InvalidBytecode::Uninitialized(_)
        ));
    }

    #[test]
    fn allocation_and_clone_check_the_counted_type() {
        let registers = vec![
            RegisterType::Rc(ManagedId(0)),
            RegisterType::Scalar(ScalarType::Bool),
        ];
        let code = vec![
            StorageLive(Register(1)),
            Constant(Register(1), Scalar::Bool(true)),
            StorageLive(Register(0)),
            RcNew(Register(0), Register(1)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![], registers, code),
            InvalidBytecode::ValueType { .. }
        ));
    }

    #[test]
    fn a_move_out_needs_an_exclusive_loan() {
        let registers = vec![
            RegisterType::Scalar(ScalarType::I64),
            RegisterType::Loan {
                access: LoanAccess::Shared,
                origin: LoanOrigin::Local(Register(0)),
                pointee: I64,
            },
            RegisterType::Scalar(ScalarType::I64),
        ];
        let code = vec![
            StorageLive(Register(0)),
            Constant(Register(0), Scalar::I64(1)),
            StorageLive(Register(1)),
            Borrow(Register(1), Register(0)),
            StorageLive(Register(2)),
            MoveOut(Register(2), Register(1)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![], registers, code),
            InvalidBytecode::ExpectedExclusiveLoan(_)
        ));
    }

    #[test]
    fn an_upgrade_result_must_be_an_option_of_the_owner() {
        let registers = vec![
            RegisterType::Weak(ManagedId(0)),
            RegisterType::Scalar(ScalarType::I64),
        ];
        let code = vec![
            StorageLive(Register(0)),
            StorageLive(Register(1)),
            RcUpgrade(Register(1), Register(0)),
            Return(None),
        ];
        assert!(matches!(
            kind(vec![], registers, code),
            InvalidBytecode::ExpectedEnum(_)
        ));
    }
}
