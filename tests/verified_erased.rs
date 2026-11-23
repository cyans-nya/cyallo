//! Owned erased values, constant aggregates, count arithmetic and a service
//! registry written in source, through the verified interpreter. Expected lines
//! are written by hand from std/any.ya and the registry example's README.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, ObjectFault, OutputSink, RunError, VerifiedModule,
    },
    compile,
    modules::{SourceModule, compile_modules},
};
use std::sync::{Arc, Mutex};

fn run_program(program: &cyallo::ir::Program, stress: bool) -> (Vec<String>, ExecutionStatus) {
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
    (lines.lock().unwrap().clone(), status)
}

fn lines(source: &str) -> Vec<String> {
    let program = compile(source).unwrap_or_else(|error| panic!("{error:?}\n{source}"));
    let (lines, status) = run_program(&program, true);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines
}

#[test]
fn an_owned_erased_value_comes_back_only_as_its_own_type() {
    let source = r#"
let boxed = std::any::erase(41)
let number: i64 = std::any::checked<i64>(boxed)
std::io::print(number + 1)
"#;
    assert_eq!(lines(source), ["42"]);
    let wrong = r#"
let boxed = std::any::erase("text")
let number: i64 = std::any::checked<i64>(boxed)
"#;
    let program = compile(wrong).unwrap();
    assert!(matches!(
        run_program(&program, false).1,
        ExecutionStatus::Failed(RunError::Object {
            fault: ObjectFault::Type,
            ..
        })
    ));
}

#[test]
fn an_erased_owner_keeps_its_counts_until_it_is_released() {
    let source = r#"
let shared = rc "s"
let boxed = std::any::erase(shared.clone())
std::io::print(shared.strong_count())
let back: rc string = std::any::checked<rc string>(boxed)
std::mem::drop(back)
std::io::print(shared.strong_count())
"#;
    assert_eq!(lines(source), ["2", "1"]);
}

#[test]
fn counts_compare_and_index_vectors() {
    let source = r#"
let names = std::collections::Vec["a", "b", "c"]
let mut i: usize = 0
let mut joined = ""
while i < names.len() {
    joined = joined + names[i]
    i += 1
}
std::io::print(joined, " ", i, " ", i - 1 >= 2 as usize)
"#;
    assert_eq!(lines(source), ["abc 3 true"]);
}

#[test]
fn a_service_registry_in_source_follows_replacement_and_withdrawal() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/lookup-services");
    let sources = ["registry.ya", "logging.ya", "console.ya", "main.yas"]
        .map(|name| SourceModule::new(name, std::fs::read_to_string(dir.join(name)).unwrap()));
    let program = compile_modules(3, &sources, Default::default(), false).unwrap();
    for stress in [false, true] {
        let (output, status) = run_program(&program, stress);
        assert!(
            matches!(status, ExecutionStatus::Returned(None)),
            "{status:?}"
        );
        assert_eq!(
            output,
            [
                "first: ready",
                "second: replacement",
                "second: cached lookup",
                "false"
            ]
        );
    }
}
