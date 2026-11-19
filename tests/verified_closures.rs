//! Records and first-class functions with by-value capture in the verified
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
fn records_hold_fields_nest_and_cross_calls() {
    let source = r#"
record Point {
    x: f64
    y: f64
}
record Segment {
    from: Point
    to: Point
    label: string
}
fn length_squared(s: Segment) -> f64 {
    let dx = s.to.x - s.from.x
    let dy = s.to.y - s.from.y
    return dx * dx + dy * dy
}
fn flip(p: Point) -> Point { return Point { x: p.y, y: p.x } }
let s = Segment { from: Point { x: 0.0, y: 0.0 }, to: Point { x: 3.0, y: 4.0 }, label: "hyp" }
let q = flip(Point { x: 3.0, y: 4.0 })
std::io::print(s.label, " ", length_squared(s))
std::io::print(q.x, ",", q.y)
"#;
    assert_eq!(run(source).unwrap(), ["hyp 25", "4,3"]);
}

#[test]
fn a_returned_closure_keeps_its_captured_value() {
    let source = r#"
fn apply(f: fn(i64) -> i64, n: i64) -> i64 { return f(n) }
fn offset_by(offset: i64) -> fn(i64) -> i64 {
    return fn(n: i64) -> i64 { return n + offset }
}
std::io::print(apply(offset_by(3), 4), " ", apply(offset_by(10), 4), " ", apply(offset_by(3), 100))
"#;
    assert_eq!(run(source).unwrap(), ["7 14 103"]);
}

#[test]
fn closures_live_inside_tuples_and_records_beside_other_fields() {
    let source = r#"
record Point { x: f64
 y: f64 }
fn apply(f: fn(i64) -> i64, n: i64) -> i64 { return f(n) }
fn offset_by(offset: i64) -> fn(i64) -> i64 {
    return fn(n: i64) -> i64 { return n + offset }
}
let offset = offset_by(3)
let point = Point { x: 1.0, y: 2.0 }
let pair = (point, offset)
std::io::print(pair.0.x, ":", apply(pair.1, 4))
"#;
    assert_eq!(run(source).unwrap(), ["1:7"]);
}

#[test]
fn closures_capture_text_and_composites_by_value() {
    let source = r#"
fn greeter(name: string, times: i64) -> fn(i64) -> string {
    return fn(n: i64) -> string {
        if n > times { return name + "!" }
        return name
    }
}
let hello = greeter("ada", 2)
std::io::print(hello(1), " ", hello(3))
let pair = (10, 20)
let sum = fn() -> i64 { return pair.0 + pair.1 }
std::io::print(sum())
"#;
    assert_eq!(run(source).unwrap(), ["ada ada!", "30"]);
}

#[test]
fn function_items_are_values_and_calls_nest() {
    let source = r#"
fn double(n: i64) -> i64 { return n * 2 }
fn inc(n: i64) -> i64 { return n + 1 }
fn twice(f: fn(i64) -> i64, n: i64) -> i64 { return f(f(n)) }
std::io::print(twice(double, 5), " ", twice(inc, 5), " ", twice(fn(n: i64) -> i64 { return n - 3 }, 10))
"#;
    assert_eq!(run(source).unwrap(), ["20 7 4"]);
}

#[test]
fn closures_in_arrays_are_chosen_at_run_time() {
    let source = r#"
fn double(n: i64) -> i64 { return n * 2 }
fn inc(n: i64) -> i64 { return n + 1 }
let table = [double, inc]
let mut total = 0
let mut i = 0
while i < 2 {
    total = total + table[i](10)
    i = i + 1
}
std::io::print(total)
"#;
    assert_eq!(run(source).unwrap(), ["31"]);
}

#[test]
fn a_closure_call_past_the_frame_limit_fails_cleanly() {
    let source = r#"
fn spin(f: fn(i64) -> i64, n: i64) -> i64 { return f(n) }
fn deep(n: i64) -> i64 { return spin(deep, n) }
std::io::print(deep(1))
"#;
    assert!(matches!(run(source), Err(RunError::FrameLimit)));
}

#[test]
fn closure_routines_stay_in_the_verified_interpreter() {
    use cyallo::backend::verified::{CodeVersion, NativeTarget, PreparedFunction};
    let Some(target) = NativeTarget::host() else {
        return;
    };
    let source = "fn make(k: i64) -> fn(i64) -> i64 { return fn(n: i64) -> i64 { return n + k } }\nlet f = make(1)\nstd::io::print(f(2))\n";
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    assert!(PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).is_err());
}

mod images {
    use Instruction::*;
    use cyallo::{
        Span,
        bytecode::verified::{
            CallSite, CallSiteId, CallTarget, FORMAT_VERSION, FunctionId, FunctionImage,
            Instruction, InvalidBytecode, ModuleImage, OperandListId, Register, RegisterType,
            ScalarType, Signature, SignatureId, ValueType, encode,
        },
    };

    const R0: Register = Register(0);
    const R1: Register = Register(1);
    const I64: ValueType = ValueType::Scalar(ScalarType::I64);

    fn function(
        registers: Vec<RegisterType>,
        parameters: Vec<Register>,
        result: Option<ValueType>,
        calls: Vec<CallSite>,
        lists: Vec<Box<[Register]>>,
        code: Vec<Instruction>,
    ) -> FunctionImage {
        FunctionImage {
            registers: registers.into(),
            parameters: parameters.into(),
            result,
            calls: calls.into(),
            lists: lists.into(),
            texts: Box::new([]),
            outputs: Box::new([]),
            spans: vec![Span::new(0, 1); code.len()].into(),
            words: encode(code),
            statements: Box::new([]),
        }
    }

    /// `f1(a, b) -> i64` returning `a`.
    fn callee() -> FunctionImage {
        function(
            vec![RegisterType::Scalar(ScalarType::I64); 2],
            vec![R0, R1],
            Some(I64),
            vec![],
            vec![],
            vec![StorageDead(R1), Return(Some(R0))],
        )
    }

    fn kind(entry: FunctionImage, signature: Signature) -> InvalidBytecode {
        ModuleImage {
            version: FORMAT_VERSION,
            entry: FunctionId(0),
            shapes: Box::new([]),
            signatures: Box::new([signature]),
            globals: Box::new([]),
            managed: Box::new([]),
            functions: Box::new([entry, callee()]),
        }
        .verify()
        .unwrap_err()
        .kind
    }

    fn entry(code: Vec<Instruction>, calls: Vec<CallSite>) -> FunctionImage {
        function(
            vec![
                RegisterType::Function(SignatureId(0)),
                RegisterType::Scalar(ScalarType::I64),
            ],
            vec![],
            None,
            calls,
            vec![Box::new([R1])],
            code,
        )
    }

    #[test]
    fn a_closure_whose_routine_has_the_wrong_arity_is_rejected() {
        let signature = Signature {
            parameters: Box::new([I64, I64]),
            result: Some(I64),
        };
        let code = vec![
            StorageLive(R1),
            Constant(R1, cyallo::bytecode::verified::Scalar::I64(1)),
            StorageLive(R0),
            Closure(R0, FunctionId(1), OperandListId(0)),
            Return(None),
        ];
        assert!(matches!(
            kind(entry(code, vec![]), signature),
            InvalidBytecode::ClosureTarget(_)
        ));
    }

    #[test]
    fn a_closure_call_checks_the_callee_register_and_arguments() {
        let signature = Signature {
            parameters: Box::new([I64]),
            result: Some(I64),
        };
        let call = |callee| CallSite {
            target: CallTarget::Closure(callee),
            arguments: Box::new([R1, R1]),
            destination: None,
        };
        let code = vec![
            StorageLive(R0),
            StorageLive(R1),
            Call(CallSiteId(0)),
            Return(None),
        ];
        // Two arguments for a one-parameter signature.
        assert!(matches!(
            kind(entry(code.clone(), vec![call(R0)]), signature.clone()),
            InvalidBytecode::ArgumentCount { .. }
        ));
        // An integer register is not callable.
        assert!(matches!(
            kind(entry(code, vec![call(R1)]), signature),
            InvalidBytecode::ExpectedFunction(_)
        ));
    }

    #[test]
    fn a_signature_that_names_an_unknown_shape_is_rejected() {
        let signature = Signature {
            parameters: Box::new([ValueType::Composite(cyallo::bytecode::verified::ShapeId(7))]),
            result: None,
        };
        assert!(matches!(
            kind(entry(vec![Return(None)], vec![]), signature),
            InvalidBytecode::UnknownShape(_)
        ));
    }
}
