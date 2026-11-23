//! Channels through the verified interpreter. Expected lines are written by
//! hand from std/channel.ya and docs/concurrency.md: a bounded channel holds
//! up to its capacity, closing keeps what is queued for receivers unless the
//! receiving end closes, the last sender going away closes the channel for
//! receivers once the queue drains, and a failed send hands the value back.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> (Vec<String>, ExecutionStatus) {
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

const PRELUDE: &str = "use std::channel::{bounded, unbounded}\nuse std::io::print\n";

#[test]
fn values_come_out_in_order_and_try_send_reports_full() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(2 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
tx.send(1).unwrap()
tx.send(2).unwrap()
let full = tx.try_send(3).unwrap_err()
print(full.is_full(), " ", full.is_closed())
print(full.into_value())
print(rx.recv().unwrap(), rx.recv().unwrap())
print(rx.try_recv().unwrap_err().is_empty())
"#
    );
    assert_eq!(lines(&source), ["true false", "3", "12", "true"]);
}

#[test]
fn an_unbounded_channel_never_reports_full() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = unbounded<i64>()
let tx = ends.0.clone()
let rx = ends.1.clone()
for i in 0..100 { tx.send(i).unwrap() }
let mut total = 0
for i in 0..100 { total += rx.recv().unwrap() }
print(total)
"#
    );
    assert_eq!(lines(&source), ["4950"]);
}

#[test]
fn a_closed_sender_end_keeps_the_queue_for_receivers() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(4 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
tx.send(7).unwrap()
tx.close()
print(tx.is_closed(), " ", rx.is_closed())
print(tx.send(8).unwrap_err().is_closed())
print(rx.recv().unwrap())
print(rx.recv().unwrap_err().is_closed())
"#
    );
    assert_eq!(lines(&source), ["true true", "true", "7", "true"]);
}

#[test]
fn a_closed_receiver_end_discards_the_queue() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(4 as usize)
let tx = ends.0.clone()
let rx = ends.1.clone()
tx.send(7).unwrap()
rx.close()
print(rx.recv().unwrap_err().is_closed())
print(tx.send(9).unwrap_err().into_value())
"#
    );
    assert_eq!(lines(&source), ["true", "9"]);
}

#[test]
fn the_last_sender_going_away_closes_after_the_queue_drains() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(4 as usize)
let rx = ends.1.clone()
let first = ends.0.clone()
let second = first.clone()
first.send(1).unwrap()
second.send(2).unwrap()
std::mem::drop(ends)
std::mem::drop(first)
print(rx.is_closed())
std::mem::drop(second)
print(rx.is_closed())
print(rx.recv().unwrap(), rx.recv().unwrap())
print(rx.recv().unwrap_err().is_closed())
"#
    );
    assert_eq!(lines(&source), ["false", "true", "12", "true"]);
}

#[test]
fn a_receive_that_can_never_finish_is_a_deadlock() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let ends = bounded<i64>(1 as usize)
let rx = ends.1.clone()
print(rx.recv().unwrap())
"#
    );
    let (output, status) = run(&source, false);
    assert!(output.is_empty());
    assert!(matches!(
        status,
        ExecutionStatus::Failed(RunError::Deadlock { .. })
    ));
}

#[test]
fn the_buffered_example_runs_under_gc_stress() {
    assert_eq!(
        lines(include_str!(
            "../examples/concurrency/channels/buffered.yas"
        )),
        ["true", "42", "true", "99"]
    );
}
