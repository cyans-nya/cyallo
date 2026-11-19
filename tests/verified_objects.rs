//! Open objects and erased values through the verified interpreter. Expected
//! lines and failures are written by hand from docs/objects.md: objects share
//! identity, an object literal's properties keep their types, a view requires
//! its properties through every alias, freezing makes an object readonly through
//! every alias, and erased values are checked when they are read back.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, ObjectFault, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> (Vec<String>, ExecutionStatus) {
    let program = compile(source).unwrap_or_else(|error| panic!("{error:?}\n{source}"));
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 50_000_000,
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
    let lines = lines.lock().unwrap().clone();
    (lines, status)
}

fn lines(source: &str) -> Vec<String> {
    let (lines, status) = run(source, true);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines
}

fn fault(source: &str) -> ObjectFault {
    match run(source, false).1 {
        ExecutionStatus::Failed(RunError::Object { fault, .. }) => fault,
        other => panic!("{other:?}"),
    }
}

#[test]
fn aliases_share_one_object_and_extra_properties_are_added() {
    let source = r#"
type Named = object { name: string, .. }
let item = object { name: "Ada", score: 10 }
let alias: Named = item
alias.name = "Grace"
alias.tag = "x"
item.count = 3
std::io::print(item.name, " ", alias.tag, " ", alias.count, " ", item.score)
"#;
    assert_eq!(lines(source), ["Grace x 3 10"]);
}

#[test]
fn computed_keys_read_write_and_delete_configurable_properties() {
    let source = r#"
let bag = dynamic { a: 1 }
bag["b"] = "two"
std::io::print(bag["a"], " ", bag["b"])
std::io::print(delete bag.a, " ", delete bag.a)
std::io::print(bag.b)
"#;
    assert_eq!(lines(source), ["1 two", "true false", "two"]);
}

#[test]
fn erased_numbers_follow_the_ordinary_numeric_rules() {
    let source = r#"
let a: dynamic = 40
let b: dynamic = 2
std::io::print(a + b)
std::io::print(a - b, " ", a * b, " ", a / b, " ", a % b)
std::io::print(a < b, " ", a >= b, " ", a == b, " ", a != b)
"#;
    assert_eq!(lines(source), ["42", "38 80 20 0", "false true false true"]);
}

#[test]
fn reading_a_missing_property_is_a_typed_failure() {
    let source = "let bag = dynamic { a: 1 }\nstd::io::print(bag.b)\n";
    assert_eq!(fault(source), ObjectFault::Missing);
}

#[test]
fn a_required_property_keeps_its_type_through_every_alias() {
    let source = r#"
let item = object { name: "Ada" }
let alias: dynamic = item
alias.name = 5
"#;
    assert_eq!(fault(source), ObjectFault::Contract);
}

#[test]
fn a_required_property_cannot_be_deleted() {
    let source = r#"
let item = object { name: "Ada" }
let alias: dynamic = item
delete alias.name
"#;
    assert_eq!(fault(source), ObjectFault::Contract);
}

#[test]
fn a_frozen_object_is_readonly_through_every_alias() {
    let source = r#"
let item = object { name: "Ada" }
let alias: dynamic = item
std::object::freeze(item)
alias.other = 1
"#;
    assert_eq!(fault(source), ObjectFault::Readonly);
}

#[test]
fn erased_values_are_checked_when_read_back() {
    let source = r#"
let any: dynamic = "text"
let number: i64 = any
"#;
    assert_eq!(fault(source), ObjectFault::Type);
}

#[test]
fn objects_survive_collections() {
    let source = r#"
let bag = dynamic { total: 0 }
for i in 0..200 {
    let scratch = object { value: i }
    bag.total = bag.total + scratch.value
}
std::io::print(bag.total)
"#;
    assert_eq!(lines(source), ["19900"]);
}

#[test]
fn the_object_examples_run_under_gc_stress() {
    assert_eq!(
        lines(include_str!("../examples/objects/open.ya")),
        ["Grace:true", "ten:true", "Grace:11"]
    );
    assert_eq!(
        lines(include_str!("../examples/objects/native-ic.ya")),
        ["19999900000"]
    );
}
