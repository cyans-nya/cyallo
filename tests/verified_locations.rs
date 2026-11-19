//! Retained field locations through the verified interpreter. Expected lines
//! are written by hand from the language contract in docs/lookups.md.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> Vec<String> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 1_000_000,
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
fn a_clone_aliases_the_field_and_a_load_is_a_snapshot() {
    let source = r#"
record Settings { retries: i64, name: string }
let settings = gc Settings { retries: 2, name: "a" }
let retries = std::lookup::field(&settings, "retries")
let name = std::lookup::field(&settings, "name")
let snapshot = retries.load()
let alias = std::mem::clone(&retries)
alias.store(5)
name.store("b")
std::io::print(snapshot, " ", retries.load(), " ", name.load())
"#;
    assert_eq!(run(source, false), ["2 5 b"]);
}

#[test]
fn a_location_keeps_its_record_alive_across_collections() {
    let source = r#"
record Box { value: i64 }
proc make() -> std::lookup::Location<i64> {
    let owner = gc Box { value: 7 }
    return std::lookup::field(&owner, "value")
}
let kept = make()
let noise = gc Box { value: 1 }
kept.store(kept.load() + 1)
std::io::print(kept.load())
"#;
    assert_eq!(run(source, true), ["8"]);
}

#[test]
fn the_locations_example_runs_under_gc_stress() {
    assert_eq!(
        run(include_str!("../examples/locations.ya"), true),
        ["snapshot: 2", "field: 5", "first: updated", "other: second"]
    );
}
