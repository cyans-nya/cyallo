//! Source modules and trait methods that take their receiver by value, through
//! the verified interpreter. Expected lines are written by hand: an imported
//! declaration behaves as if declared in place, and a by-value method receives a
//! copy of what the object holds, so the object keeps its value.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, VerifiedModule},
    compile,
    modules::{SourceModule, compile_modules},
};
use std::sync::{Arc, Mutex};

fn run_program(program: &cyallo::ir::Program, stress: bool) -> Vec<String> {
    let module = VerifiedModule::lower(program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 5_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_gc_stress(stress);
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    let status = activation.resume(usize::MAX);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines.lock().unwrap().clone()
}

#[test]
fn a_by_value_receiver_gets_a_copy_and_the_object_keeps_its_value() {
    let source = r#"
trait Readable { fn value(self) -> i64 }
record Reading { number: i64 }
impl Readable for Reading {
    fn value(self) -> i64 { return self.number }
}
let reading = Reading { number: 42 } as dyn Readable
std::io::print(reading.value())
std::io::print(reading.value())
"#;
    for stress in [false, true] {
        let program = compile(source).unwrap();
        assert_eq!(run_program(&program, stress), ["42", "42"]);
    }
}

#[test]
fn a_loan_receiver_and_a_by_value_receiver_can_share_one_trait() {
    let source = r#"
trait Shape {
    fn area(self) -> i64
    fn name(self: &Self) -> string
}
record Square { side: i64 }
impl Shape for Square {
    fn area(self) -> i64 { return self.side * self.side }
    fn name(self: &Self) -> string { return "square" }
}
let shape = Square { side: 6 } as dyn Shape
std::io::print(shape.name(), " ", shape.area())
"#;
    let program = compile(source).unwrap();
    assert_eq!(run_program(&program, true), ["square 36"]);
}

#[test]
fn imported_traits_records_constants_and_closures_work_across_modules() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/modules/static");
    let sources = ["model.ya", "math.ya", "main.yas"]
        .map(|name| SourceModule::new(name, std::fs::read_to_string(dir.join(name)).unwrap()));
    let program = compile_modules(2, &sources, Default::default(), false).unwrap();
    assert_eq!(run_program(&program, true), ["42:retained"]);
}
