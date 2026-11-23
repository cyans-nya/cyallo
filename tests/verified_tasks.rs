//! Tasks, joins, cancellation and clocks through the verified interpreter.
//! Expected lines are written by hand from std/task.ya and docs/concurrency.md:
//! a spawned task runs concurrently with its parent, a blocked task lets
//! others run, `join` waits for the outcome, a cancelled task ends the next time
//! it is scheduled and releases what it owns, and a program in which every
//! task waits for another is a deadlock.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn run(source: &str, stress: bool, slice: usize) -> (Vec<String>, ExecutionStatus) {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
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
    loop {
        match activation.resume(slice) {
            ExecutionStatus::Paused(_) => {}
            status => return (lines.lock().unwrap().clone(), status),
        }
    }
}

fn lines(source: &str) -> Vec<String> {
    let (lines, status) = run(source, true, usize::MAX);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines
}

const PRELUDE: &str =
    "use std::channel::{bounded, unbounded}\nuse std::task::spawn\nuse std::io::print\n";

#[test]
fn a_task_returns_its_result_through_join() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let worker = spawn(proc() -> i64 {
    let mut total = 0
    for i in 1..=10 { total += i }
    return total
})
let outcome = worker.join()
print(outcome.is_completed())
print(outcome.completed())
"#
    );
    assert_eq!(lines(&source), ["true", "55"]);
}

#[test]
fn a_task_that_waits_lets_the_others_run() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(0 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
let producer = spawn(proc() {
    for i in 1..=5 { tx.send(i * i).unwrap() }
})
let mut total = 0
for i in 1..=5 { total += rx.recv().unwrap() }
producer.join().completed()
print(total)
"#
    );
    for slice in [usize::MAX, 1, 7] {
        let (output, status) = run(&source, true, slice);
        assert!(
            matches!(status, ExecutionStatus::Returned(None)),
            "{status:?}"
        );
        assert_eq!(output, ["55"]);
    }
}

#[test]
fn a_producer_and_a_consumer_pass_values_in_order_through_a_small_buffer() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(2 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
std::mem::drop(ends)
let producer = spawn(proc() {
    for i in 0..20 { tx.send(i).unwrap() }
})
let consumer = spawn(proc() -> i64 {
    let mut last = -1
    let mut ordered = 1
    while true {
        let next = rx.recv()
        if next.is_err() { return ordered * 100 + last }
        let value = next.unwrap()
        if value != last + 1 { ordered = 0 }
        last = value
    }
    return 0
})
producer.join().completed()
print(consumer.join().completed())
"#
    );
    assert_eq!(lines(&source), ["119"]);
}

#[test]
fn cancelling_a_waiting_task_releases_what_it_captured() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(0 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
std::mem::drop(ends)
let worker = spawn(proc() -> i64 { return rx.recv().unwrap() })
print(worker.is_finished())
worker.cancel()
let outcome = worker.join()
print(outcome.is_cancelled(), " ", outcome.is_completed(), " ", outcome.is_failed())
print(tx.is_closed())
"#
    );
    assert_eq!(lines(&source), ["false", "true false false", "true"]);
}

#[test]
fn a_task_that_fails_reports_failure_and_the_parent_goes_on() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(1 as usize)
let rx = ends.1.clone()
std::mem::drop(ends)
let failing = spawn(proc() -> i64 { return rx.recv().unwrap() })
let outcome = failing.join()
print(outcome.is_failed(), " ", outcome.is_completed())
"#
    );
    assert_eq!(lines(&source), ["true false"]);
}

#[test]
fn tasks_that_all_wait_for_each_other_are_a_deadlock() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let first = bounded<i64>(0 as usize)
let second = bounded<i64>(0 as usize)
let a = first.1.clone()
let b = second.1.clone()
let worker = spawn(proc() -> i64 { return a.recv().unwrap() })
print(b.recv().unwrap())
"#
    );
    let (output, status) = run(&source, false, usize::MAX);
    assert!(output.is_empty());
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::Deadlock { .. })
    ));
}

#[test]
fn a_task_cannot_capture_a_handle() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
record Box { value: i64 }
let shared = gc Box { value: 1 }
let worker = spawn(proc() -> i64 { return shared.value })
print(worker.join().completed())
"#
    );
    // The checker may refuse this before it runs; if it compiles, the engine
    // refuses the capture.
    if let Ok(program) = compile(&source)
        && let Ok(module) = VerifiedModule::lower(&program, 0)
    {
        let limits = ExecutionLimits {
            fuel: 100_000,
            frames: 64,
        };
        let mut activation = module.start(&[], limits).unwrap();
        assert!(matches!(
            activation.resume(usize::MAX),
            ExecutionStatus::Failed(RunError::NotTransferable { .. })
        ));
    }
}

#[test]
fn sleeping_waits_without_stopping_other_tasks() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let clock = std::time::clock()
let ends = unbounded<i64>()
let tx = ends.0.clone()
let rx = ends.1.clone()
let napper = spawn(proc() -> i64 {
    clock.sleep(30 as u64)
    tx.send(2).unwrap()
    return 0
})
print("before")
print(rx.recv().unwrap())
napper.join().completed()
"#
    );
    let start = Instant::now();
    assert_eq!(lines(&source), ["before", "2"]);
    assert!(start.elapsed() >= Duration::from_millis(30));
}

#[test]
fn the_concurrency_examples_run_under_gc_stress_and_in_tiny_slices() {
    for (path, source, expected) in [
        (
            "rendezvous",
            include_str!("../examples/concurrency/channels/rendezvous.yas"),
            vec!["42", "7"],
        ),
        (
            "mpmc",
            include_str!("../examples/concurrency/channels/mpmc.yas"),
            vec!["21"],
        ),
        (
            "cancellation",
            include_str!("../examples/concurrency/scheduling/cancellation.yas"),
            vec!["true", "true", "99"],
        ),
        (
            "clock",
            include_str!("../examples/concurrency/scheduling/clock.yas"),
            vec!["before", "after"],
        ),
    ] {
        for slice in [usize::MAX, 1] {
            let (output, status) = run(source, true, slice);
            assert!(
                matches!(status, ExecutionStatus::Returned(None)),
                "{path}: {status:?}"
            );
            assert_eq!(output, expected, "{path}");
        }
    }
}
