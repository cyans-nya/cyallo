use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

fn parity(source: &str, expected: &str) {
    let program = Arc::new(compile(source).unwrap_or_else(|error| panic!("{error}\n{source}")));
    for stress in [false, true] {
        let mut session = Session::with_context(
            program.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig { stress, ..Default::default() },
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..100_000 {
            if session.run(1000) != &Status::Paused {
                break;
            }
        }
        assert_eq!(session.status(), &Status::Finished, "stress={stress}\n{source}");
        assert_eq!(session.context.output, expected, "stress={stress}\n{source}");
    }
}

#[test]
fn buffered_channels_keep_typed_errors_and_unsent_values() {
    parity(
        include_str!("../examples/concurrency/channels/buffered.ya"),
        "true\n42\ntrue\n99\n",
    );
}
#[test]
fn rendezvous_and_task_join_park_on_one_processor() {
    parity(
        include_str!("../examples/concurrency/channels/rendezvous.ya"),
        "42\n7\n",
    );
}
#[test]
fn clock_waits_resume_through_every_engine() {
    parity(
        include_str!("../examples/concurrency/scheduling/clock.ya"),
        "before\nafter\n",
    );
}
#[test]
fn runtime_failures_and_returned_errors_are_distinct() {
    parity(
        r#"
        use std::task::spawn
        use std::io::print
        let failed = spawn(proc() -> i64 { let zero = 0; return 1 / zero })
        print(failed.join().is_failed())
        let ordinary = spawn(proc() -> Result<i64, string> { return Result::Err("ordinary") })
        print(ordinary.join().completed().unwrap_err())
    "#,
        "true\nordinary\n",
    );
}
#[test]
fn channels_reject_local_elements_and_tasks_reject_local_captures() {
    for source in [
        "let bad = std::channel::unbounded<gc i64>()",
        "let bad = std::channel::unbounded<&i64>()",
        "let n = gc 1; let bad = std::task::spawn(proc() -> i64 { return n.* })",
        "let n = 1; let bad = std::task::spawn(proc[&n]() -> i64 { return n })",
        "proc forward(work: proc() -> i64) { let bad = std::task::spawn(work) }",
        "let bad = std::task::spawn(proc() -> gc i64 { return gc 1 })",
        "fn bad() { let c = std::time::clock() }",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
fn multiple_producers_and_consumers_share_a_bounded_channel() {
    parity(
        r#"
        use std::task::spawn
        use std::io::print
        let endpoints = std::channel::bounded<i64>(1 as usize)
        let first_sender = endpoints.0.clone()
        let second_sender = endpoints.0.clone()
        let first_receiver = endpoints.1.clone()
        let second_receiver = endpoints.1.clone()
        let first = spawn(proc() { for i in 1..=3 { first_sender.send(i).unwrap() } })
        let second = spawn(proc() { for i in 4..=6 { second_sender.send(i).unwrap() } })
        let left = spawn(proc() -> i64 { let mut total = 0; for i in 0..3 { total += first_receiver.recv().unwrap() }; return total })
        let right = spawn(proc() -> i64 { let mut total = 0; for i in 0..3 { total += second_receiver.recv().unwrap() }; return total })
        first.join().completed()
        second.join().completed()
        print(left.join().completed() + right.join().completed())
    "#,
        "21\n",
    );
}
#[test]
fn cancellation_releases_the_last_receiver_and_preserves_unsent_values() {
    parity(
        r#"
        use std::task::spawn
        use std::io::print
        let endpoints = std::channel::bounded<i64>(0 as usize)
        let sender = endpoints.0.clone()
        let receiver = endpoints.1.clone()
        std::mem::drop(endpoints)
        let worker = spawn(proc() -> i64 { return receiver.recv().unwrap() })
        worker.cancel()
        print(worker.join().is_cancelled())
        print(sender.is_closed())
        print(sender.send(99).unwrap_err().into_value())
    "#,
        "true\ntrue\n99\n",
    );
}
#[test]
fn owned_aggregates_cross_sessions_without_local_cell_ids() {
    parity(
        r#"
        let values = [10, 20, 30]
        let worker = std::task::spawn(proc() -> [3]i64 { return values })
        let result = worker.join().completed()
        std::io::print(result[0] + result[1] + result[2])
    "#,
        "60\n",
    );
}
#[test]
fn declaration_aliases_and_user_spellings_keep_their_identity() {
    parity(
        r#"
        use std::task::spawn as launch
        proc spawn() -> i64 { return 99 }
        let work = proc() -> i64 { return 42 }
        let task = launch(work)
        std::io::print(task.join().completed())
        std::io::print(spawn())
    "#,
        "42\n99\n",
    );
}

#[test]
fn enum_payloads_cross_sessions_without_heap_cell_indices() {
    parity(
        include_str!("../examples/concurrency/enums.ya"),
        "42:worker\nfinished\n",
    );
    for source in [
        "enum E { Value: gc i64 }\nlet endpoint = std::channel::bounded<E>(1 as usize)\n",
        "enum E { Value: record { data: gc i64 } }\nlet endpoint = std::channel::bounded<E>(1 as usize)\n",
        "enum E { Value: &i64 }\nlet endpoint = std::channel::bounded<E>(1 as usize)\n",
    ] {
        assert!(
            compile(source).is_err(),
            "accepted local enum transport: {source}"
        );
    }
}

#[test]
fn standard_status_enums_cross_sessions_with_their_owned_payloads() {
    parity(
        r#"
use std::generator::Step
use std::channel::{ReceiveError,SendError}
let steps = std::channel::bounded<Step<string,string>>(1 as usize)
let sender = steps.0.clone()
let producer = std::task::spawn(proc() { sender.send(Step::Yielded("yielded")).unwrap() })
let received = steps.1.recv().unwrap()
std::io::print(match received { Step::Yielded(text) | Step::Complete(text) => text })
producer.join().completed()
let errors = std::channel::bounded<SendError<string>>(1 as usize)
let sender = errors.0.clone()
let producer = std::task::spawn(proc() { sender.send(SendError::Full("unsent")).unwrap() })
std::io::print(match errors.1.recv().unwrap() { SendError::Full(text) | SendError::Closed(text) | SendError::Limit(text) => text })
producer.join().completed()
let result = std::task::spawn(proc() -> ReceiveError { return ReceiveError::Closed }).join().completed()
std::io::print(match result { ReceiveError::Closed => "closed", ReceiveError::Empty => "empty" })
"#,
        "yielded\nunsent\nclosed\n",
    );
}

#[test]
fn standard_status_enum_example_executes() {
    parity(
        include_str!("../examples/concurrency/status-enums.ya"),
        "7\ncomplete: finished\nrecovered: second\nempty\nclosed\n",
    );
}
