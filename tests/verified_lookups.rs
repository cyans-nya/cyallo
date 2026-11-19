//! Storage paths and mapped lookups through the verified interpreter. Expected
//! lines are written by hand from the language contract in docs/lookups.md.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, OutputSink, PathFault, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 1_000_000,
    frames: 64,
};

fn run(source: &str, stress: bool) -> (Vec<String>, ExecutionStatus) {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let mut activation = module.start(&[], LIMITS).unwrap();
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

const REGISTRY: &str = r#"
use std::lookup::StoragePath
record Directory { values: Cell<Vec<gc Cell<i64>>> }
proc route(root: gc Directory, key: i64) -> StoragePath<gc Cell<i64>> {
    return std::lookup::path(root).target().field("values").target().element(key)
}
proc never(root: gc Directory, key: i64) -> StoragePath<gc Cell<i64>> {
    return std::lookup::missing<gc Cell<i64>>()
}
let root = gc Directory { values: Cell::new(Vec[gc Cell::new(42), gc Cell::new(7)]) }
"#;

#[test]
fn a_mapped_path_reaches_the_current_value() {
    let source = format!(
        "{REGISTRY}\n{}",
        concat!(
            "let first = std::lookup::mapped(root, 0, route).unwrap()\n",
            "std::io::print(first.*.*)\n",
            "first.*.* = 43\n",
            "std::io::print(std::lookup::mapped(root, 0, route).unwrap().*.*)\n",
            "std::io::print(std::lookup::mapped(root, 1, route).unwrap().*.*)\n",
        )
    );
    for stress in [false, true] {
        let (lines, status) = run(&source, stress);
        assert!(
            matches!(status, ExecutionStatus::Returned(None)),
            "{status:?}"
        );
        assert_eq!(lines, ["42", "43", "7"]);
    }
}

#[test]
fn the_missing_path_gives_none() {
    let source = format!(
        "{REGISTRY}\n{}",
        concat!(
            "std::io::print(std::lookup::mapped(root, 0, never).is_none())\n",
            "std::io::print(std::lookup::mapped(root, 0, route).is_none())\n",
        )
    );
    let (lines, status) = run(&source, false);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    assert_eq!(lines, ["true", "false"]);
}

#[test]
fn an_element_past_the_end_is_a_typed_failure() {
    let source = format!(
        "{REGISTRY}\nlet gone = std::lookup::mapped(root, 5, route)\nstd::io::print(\"unreached\")\n"
    );
    let (lines, status) = run(&source, false);
    assert!(lines.is_empty());
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::Path {
            fault: PathFault::OutOfBounds,
            ..
        })
    ));
}

#[test]
fn a_negative_index_is_a_typed_failure() {
    let source = format!("{REGISTRY}\nlet gone = std::lookup::mapped(root, -1, route)\n");
    let (_, status) = run(&source, false);
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::Path {
            fault: PathFault::NegativeIndex,
            ..
        })
    ));
}

#[test]
fn the_directory_example_follows_updates() {
    let (lines, status) = run(include_str!("../examples/lookups.ya"), true);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    assert_eq!(
        lines,
        [
            "red: 10",
            "red: 10",
            "red: 20",
            "blue: missing",
            "blue: missing",
            "blue: 30",
        ]
    );
}
