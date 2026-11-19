//! Native functions held in erased values, through the verified interpreter.
//! Expected lines are written by hand from docs/objects.md: calling a callable
//! held in a `dynamic` checks the actual argument types, and the result is
//! erased.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, ObjectFault, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> (Vec<String>, ExecutionStatus) {
    let program = Arc::new(compile(source).unwrap_or_else(|error| panic!("{error:?}\n{source}")));
    let module = VerifiedModule::lower_shared(program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 20_000_000,
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
    (lines.lock().unwrap().clone(), status)
}

#[test]
fn a_native_function_in_a_dynamic_is_called_with_the_actual_argument_types() {
    let source = r#"
let callback: dynamic = std::num::abs
std::io::print(callback(-42))
std::io::print(callback(-2.5))
std::io::print(callback(7))
"#;
    for stress in [false, true] {
        let (lines, status) = run(source, stress);
        assert!(
            matches!(status, ExecutionStatus::Returned(None)),
            "{status:?}"
        );
        assert_eq!(lines, ["42", "2.5", "7"]);
    }
}

#[test]
fn the_result_is_erased_and_can_be_read_back_by_type() {
    let source = r#"
let callback: dynamic = std::num::abs
let result = callback(-5)
let number: i64 = result
std::io::print(number + 1)
"#;
    let (lines, status) = run(source, true);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    assert_eq!(lines, ["6"]);
}

#[test]
fn an_argument_the_function_does_not_accept_is_a_typed_failure() {
    let source = r#"
let callback: dynamic = std::num::abs
std::io::print(callback("text"))
"#;
    assert!(matches!(
        run(source, false).1,
        ExecutionStatus::Failed(RunError::Object {
            fault: ObjectFault::Type,
            ..
        })
    ));
}
