//! Trait objects and dynamic dispatch in the verified interpreter. Expected
//! text is written by hand from the language contract.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, HeapStats, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 50_000_000,
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
fn the_documented_example_shares_state_between_object_copies() {
    let source = include_str!("../examples/traits.ya");
    for stress in [false, true] {
        let (lines, _) = run_with(source, stress).unwrap();
        assert_eq!(lines, ["direct: 5", "shared: 9", "original: 5"]);
    }
}

const SHAPES: &str = r#"
trait Shape {
    fn area(self: &Self) -> i64
    proc scale(self: &mut Self, by: i64)
}
record Square { side: i64 }
record Rect { width: i64
 height: i64 }
impl Shape for Square {
    fn area(self: &Self) -> i64 { return self.side * self.side }
    proc scale(self: &mut Self, by: i64) { self.side = self.side * by }
}
impl Shape for Rect {
    fn area(self: &Self) -> i64 { return self.width * self.height }
    proc scale(self: &mut Self, by: i64) {
        self.width = self.width * by
        self.height = self.height * by
    }
}
"#;

#[test]
fn each_object_runs_the_method_of_its_own_type() {
    let source = format!(
        "{SHAPES}let square = Square {{ side: 3 }} as dyn Shape\nlet rect = Rect {{ width: 2, height: 5 }} as dyn Shape\nstd::io::print(square.area(), \" \", rect.area())\nsquare.scale(2)\nrect.scale(3)\nstd::io::print(square.area(), \" \", rect.area())\n"
    );
    assert_eq!(run(&source).unwrap(), ["9 10", "36 90"]);
}

#[test]
fn objects_of_different_types_are_stored_together_and_dispatched_in_a_loop() {
    let source = format!(
        "{SHAPES}let shapes = [Square {{ side: 4 }} as dyn Shape, Rect {{ width: 3, height: 7 }} as dyn Shape, Square {{ side: 1 }} as dyn Shape]\nlet mut total = 0\nlet mut i = 0\nwhile i < 3 {{\n    total = total + shapes[i].area()\n    i = i + 1\n}}\nstd::io::print(total)\n"
    );
    // 16 + 21 + 1.
    for stress in [false, true] {
        assert_eq!(run_with(&source, stress).unwrap().0, ["38"]);
    }
}

#[test]
fn an_object_is_kept_alive_by_a_routine_that_holds_it() {
    let source = format!(
        "{SHAPES}proc churn(n: i64) -> i64 {{\n    let mut total = 0\n    let mut i = 0\n    while i < n {{\n        let temporary = Square {{ side: i }} as dyn Shape\n        total = total + temporary.area()\n        i = i + 1\n    }}\n    return total\n}}\nlet kept = Rect {{ width: 6, height: 7 }} as dyn Shape\nstd::io::print(churn(600))\nstd::io::print(kept.area())\n"
    );
    let (lines, stats) = run_with(&source, false).unwrap();
    // The sum of the squares from 0 up to 599.
    assert_eq!(lines, ["71820100", "42"]);
    assert!(stats.collections > 0, "{stats:?}");
    assert!(stats.peak < 600, "{stats:?}");
}

#[test]
fn trait_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let source =
        format!("{SHAPES}let s = Square {{ side: 2 }} as dyn Shape\nstd::io::print(s.area())\n");
    let program = compile(&source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            CallSite, CallSiteId, CallTarget, FORMAT_VERSION, FunctionId, FunctionImage,
            Implementation, ImplementationId, Instruction, InvalidBytecode, LoanAccess, LoanOrigin,
            Method, ModuleImage, Register, RegisterType, ScalarType, TraitDescription, TraitId,
            ValueType, encode,
        },
    };

    const I64: ValueType = ValueType::Scalar(ScalarType::I64);
    const SELF: Register = Register(0);

    /// `f1(self: &i64) -> i64`.
    fn reader(access: LoanAccess, result: Option<ValueType>) -> FunctionImage {
        FunctionImage {
            contracts: Box::new([]),
            generator: None,
            registers: Box::new([RegisterType::Loan {
                access,
                origin: LoanOrigin::Parameter { register: SELF },
                pointee: I64,
            }]),
            parameters: Box::new([SELF]),
            result,
            calls: Box::new([]),
            lists: Box::new([]),
            texts: Box::new([]),
            outputs: Box::new([]),
            spans: Box::new([Span::new(0, 1); 2]),
            words: encode([StorageDead(SELF), Return(None)]),
            statements: Box::new([]),
        }
    }

    fn entry(call: Vec<CallSite>, code: Vec<Instruction>) -> FunctionImage {
        FunctionImage {
            contracts: Box::new([]),
            generator: None,
            registers: Box::new([RegisterType::Object(TraitId(0))]),
            parameters: Box::new([]),
            result: None,
            calls: call.into(),
            lists: Box::new([]),
            texts: Box::new([]),
            outputs: Box::new([]),
            spans: vec![Span::new(0, 1); code.len()].into(),
            words: encode(code),
            statements: Box::new([]),
        }
    }

    fn kind(
        method: Method,
        implementation_receiver: ValueType,
        routine: FunctionImage,
        entry: FunctionImage,
    ) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([]),
            globals: Box::new([]),
            managed: Box::new([]),
            traits: Box::new([TraitDescription {
                methods: Box::new([method]),
            }]),
            implementations: Box::new([Implementation {
                trait_id: TraitId(0),
                receiver: implementation_receiver,
                methods: Box::new([FunctionId(1)]),
            }]),
            functions: Box::new([entry, routine]),
        }
        .verify()
        .unwrap_err()
        .kind
    }

    fn method(receiver: LoanAccess) -> Method {
        Method {
            receiver,
            parameters: Box::new([]),
            result: None,
        }
    }

    #[test]
    fn a_routine_must_take_the_receiver_its_trait_declares() {
        let idle = entry(vec![], vec![Return(None)]);
        // The trait wants an exclusive receiver; the routine takes a shared one.
        assert!(matches!(
            kind(
                method(LoanAccess::Exclusive),
                I64,
                reader(LoanAccess::Shared, None),
                idle.clone()
            ),
            InvalidBytecode::ImplementationMismatch(ImplementationId(0))
        ));
        // The implementation is for another receiver type.
        assert!(matches!(
            kind(
                method(LoanAccess::Shared),
                ValueType::Text,
                reader(LoanAccess::Shared, None),
                idle.clone()
            ),
            InvalidBytecode::ImplementationMismatch(_)
        ));
        // The routine returns something the trait does not.
        assert!(matches!(
            kind(
                method(LoanAccess::Shared),
                I64,
                reader(LoanAccess::Shared, Some(I64)),
                idle
            ),
            InvalidBytecode::ImplementationMismatch(_)
        ));
    }

    #[test]
    fn a_call_names_a_method_the_trait_declares() {
        let call = CallSite {
            target: CallTarget::Method {
                object: Register(0),
                method: 4,
            },
            arguments: Box::new([]),
            destination: None,
        };
        let caller = entry(
            vec![call],
            vec![StorageLive(Register(0)), Call(CallSiteId(0)), Return(None)],
        );
        assert!(matches!(
            kind(
                method(LoanAccess::Shared),
                I64,
                reader(LoanAccess::Shared, None),
                caller
            ),
            InvalidBytecode::UnknownMethod { method: 4, .. }
        ));
    }
}
