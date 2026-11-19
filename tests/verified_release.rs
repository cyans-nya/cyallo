//! Owners held by a routine are released when it returns. Expected counts are
//! written by hand: a call that receives an owner and does not pass it on
//! leaves the caller's count unchanged once it returns.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str) -> Vec<String> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 1_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
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
fn unused_and_used_owned_parameters_are_released() {
    let source = r#"
use std::io::print
proc unused(owner: rc string) {}
proc used(owner: rc string) { print(owner.*) }
proc passed_on(owner: rc string) -> rc string { return owner }
let shared = rc "s"
unused(shared.clone())
print(shared.strong_count())
used(shared.clone())
print(shared.strong_count())
let back = passed_on(shared.clone())
print(shared.strong_count())
std::mem::drop(back)
print(shared.strong_count())
"#;
    assert_eq!(run(source), ["1", "s", "1", "2", "1"]);
}

#[test]
fn a_local_is_released_by_an_explicit_return() {
    let source = r#"
use std::io::print
proc early(shared: &rc string) -> i64 {
    let mine = shared.clone()
    let other = shared.clone()
    return 2
}
let shared = rc "s"
print(early(&shared))
print(shared.strong_count())
"#;
    assert_eq!(run(source), ["2", "1"]);
}

#[test]
fn a_moved_local_is_not_released_twice() {
    let source = r#"
use std::io::print
proc hand_over(shared: &rc string) -> rc string {
    let mine = shared.clone()
    return mine
}
let shared = rc "s"
let kept = hand_over(&shared)
print(shared.strong_count())
std::mem::drop(kept)
print(shared.strong_count())
"#;
    assert_eq!(run(source), ["2", "1"]);
}

#[test]
fn a_generator_releases_its_owner_when_it_returns() {
    let source = r#"
use std::generator::Generator
use std::io::print
let held = gen proc(owner: rc string) -> Generator<i64, (), i64> {
    let mine = owner
    yield 1
    return 2
}
let shared = rc "s"
let mut values = held(shared.clone())
values.start()
print(shared.strong_count())
values.resume()
print(shared.strong_count())
"#;
    assert_eq!(run(source), ["2", "1"]);
}
