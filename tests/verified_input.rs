//! Interactive input through the verified interpreter. Expected lines are
//! written by hand from the language contract, not taken from another engine.

use cyallo::{
    CompileOptions, Dialect,
    bytecode::verified::{
        ExecutionStatus, InputKind, InputSource, OutputSink, RunError, VerifiedModule,
    },
    compile, compile_with_options,
};
use std::sync::{Arc, Mutex};

const LIMITS: cyallo::bytecode::verified::ExecutionLimits =
    cyallo::bytecode::verified::ExecutionLimits {
        fuel: 100_000,
        frames: 64,
    };

fn caie(source: &str) -> cyallo::ir::Program {
    compile_with_options(
        source,
        CompileOptions {
            dialect: Dialect::Compatibility,
            ..Default::default()
        },
    )
    .unwrap()
}

/// Run with the given input lines and a fuel slice; return output and status.
fn run(
    program: &cyallo::ir::Program,
    input: &[&str],
    slice: usize,
) -> (Vec<String>, ExecutionStatus) {
    let module = VerifiedModule::lower(program, 0).unwrap();
    let mut activation = module.start(&[], LIMITS).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    activation.set_input(InputSource::lines(
        input
            .iter()
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>(),
    ));
    loop {
        match activation.resume(slice) {
            ExecutionStatus::Paused(_) => {}
            status => return (lines.lock().unwrap().clone(), status),
        }
    }
}

#[test]
fn caie_reads_each_kind_of_value() {
    let program = caie(concat!(
        "DECLARE n : INTEGER\n",
        "DECLARE x : REAL\n",
        "DECLARE b : BOOLEAN\n",
        "DECLARE s : STRING\n",
        "INPUT n\n",
        "INPUT x\n",
        "INPUT b\n",
        "INPUT s\n",
        "OUTPUT n + 1\n",
        "OUTPUT x * 2.0\n",
        "OUTPUT NOT b\n",
        "OUTPUT s\n",
    ));
    let (lines, status) = run(&program, &[" 41 ", "1.25", "true", "  keep spaces "], 1000);
    assert!(matches!(status, ExecutionStatus::Returned(None)));
    assert_eq!(lines, ["42", "2.5", "FALSE", "  keep spaces "]);
}

#[test]
fn each_line_is_consumed_once_across_tiny_slices() {
    let program = caie(concat!(
        "DECLARE a : INTEGER\n",
        "DECLARE b : INTEGER\n",
        "INPUT a\n",
        "INPUT b\n",
        "OUTPUT a - b\n",
    ));
    let (lines, status) = run(&program, &["10", "3", "99"], 1);
    assert!(matches!(status, ExecutionStatus::Returned(None)));
    assert_eq!(lines, ["7"]);
}

#[test]
fn end_of_input_and_bad_lines_are_typed_failures() {
    let program = caie("DECLARE n : INTEGER\nINPUT n\nOUTPUT n\n");
    let (lines, status) = run(&program, &[], 1000);
    assert!(lines.is_empty());
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::InputExhausted { .. })
    ));
    let (_, status) = run(&program, &["4x"], 1000);
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::InputInvalid {
            kind: InputKind::Integer,
            ..
        })
    ));
    assert!(matches!(
        run(&caie("DECLARE b : BOOLEAN\nINPUT b\n"), &["maybe"], 1000).1,
        ExecutionStatus::Failed(RunError::InputInvalid {
            kind: InputKind::Bool,
            ..
        })
    ));
}

#[test]
fn cyallo_reads_through_a_loan_and_parses() {
    let program = compile(include_str!("../examples/secret-number.yas")).unwrap();
    let (lines, status) = run(&program, &["50", "abc", "10", "42"], 1000);
    assert!(matches!(status, ExecutionStatus::Returned(None)));
    assert_eq!(
        lines,
        [
            "Guess a whole number from 1 to 100.",
            "Try lower.",
            "Please enter a whole number.",
            "Try higher.",
            "Correct!",
        ]
    );
}
