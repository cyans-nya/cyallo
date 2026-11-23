//! Generators through the verified interpreter. Expected lines are written by
//! hand from the generator contract in std/generator.ya: `start` runs a new
//! generator to its first yield, `resume(v)` continues it with `v` as the value
//! of the yield expression, a return completes it, and `close` releases it.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, GeneratorFault, OutputSink, RunError, VerifiedModule,
    },
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool, slice: usize) -> (Vec<String>, ExecutionStatus) {
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

const PRELUDE: &str = "use std::generator::Generator\nuse std::io::print\n";

#[test]
fn a_generator_yields_in_order_then_completes() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let counter = gen proc(limit: i64) -> Generator<i64, (), i64> {
    let mut n = 0
    while n < limit {
        yield n * n
        n += 1
    }
    return limit
}
let mut values = counter(3)
let mut step = values.start()
while step.is_yielded() {
    print(step.yielded())
    step = values.resume()
}
print("done ", step.completed())
"#
    );
    assert_eq!(lines(&source), ["0", "1", "4", "done 3"]);
}

#[test]
fn resume_values_become_the_value_of_the_yield() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let total = gen proc() -> Generator<i64, i64, i64> {
    let mut sum = 0
    let mut input = yield 0
    while input >= 0 {
        sum += input
        input = yield sum
    }
    return sum
}
let mut running = total()
print(running.start().yielded())
print(running.resume(5).yielded())
print(running.resume(7).yielded())
print(running.resume(-1).completed())
"#
    );
    assert_eq!(lines(&source), ["0", "5", "12", "12"]);
}

#[test]
fn generators_made_by_one_factory_are_independent() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let base = 100
let make = gen proc(step: i64) -> Generator<i64, (), i64> {
    let mut at = base
    while true {
        at += step
        yield at
    }
    return 0
}
let mut slow = make(1)
let mut fast = make(10)
print(slow.start().yielded())
print(fast.start().yielded())
print(slow.resume().yielded())
print(fast.resume().yielded())
"#
    );
    assert_eq!(lines(&source), ["101", "110", "102", "120"]);
}

#[test]
fn a_loan_in_the_suspended_frame_follows_it_to_a_new_stack_depth() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let borrower = gen proc() -> Generator<i64, (), i64> {
    let mut cell = 1
    let view = &mut cell
    yield 0
    view.* = view.* + 41
    yield cell
    return 0
}
proc deeper(depth: i64, values: &mut Generator<i64, (), i64>) -> i64 {
    if depth == 0 {
        return values.resume().yielded()
    }
    return deeper(depth - 1, values)
}
let mut values = borrower()
values.start()
print(deeper(3, &mut values))
"#
    );
    assert_eq!(lines(&source), ["42"]);
}

#[test]
fn advancing_in_the_wrong_state_is_a_typed_failure() {
    let make = |body: &str| {
        format!(
            "{PRELUDE}{}{body}",
            r#"
let once = gen proc() -> Generator<i64, (), i64> {
    yield 1
    return 2
}
let mut values = once()
"#
        )
    };
    let fault = |body: &str| match run(&make(body), false, usize::MAX).1 {
        ExecutionStatus::Failed(RunError::Generator { fault, .. }) => fault,
        other => panic!("{other:?}"),
    };
    assert_eq!(fault("values.resume()\n"), GeneratorFault::NotStarted);
    assert_eq!(
        fault("values.start()\nvalues.start()\n"),
        GeneratorFault::AlreadyStarted
    );
    assert_eq!(
        fault("values.start()\nvalues.resume()\nvalues.resume()\n"),
        GeneratorFault::Finished
    );
    assert_eq!(
        fault("values.start()\nvalues.close()\nvalues.resume()\n"),
        GeneratorFault::Finished
    );
    let (output, status) = run(
        &make("values.start()\nvalues.close()\nvalues.close()\n"),
        false,
        7,
    );
    assert!(output.is_empty());
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
}

#[test]
fn closing_releases_what_the_suspended_frame_owns() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
let held = gen proc(owner: rc string) -> Generator<i64, (), i64> {
    let mine = owner
    yield 1
    print(mine.*)
    return 2
}
let shared = rc "shared"
let mut values = held(shared.clone())
values.start()
print(shared.strong_count())
values.close()
print(shared.strong_count())
"#
    );
    assert_eq!(lines(&source), ["2", "1"]);
}

#[test]
fn a_suspended_generator_keeps_its_handles_alive_across_collections() {
    let source = format!(
        "{PRELUDE}{}",
        r#"
record Box { value: i64 }
let keeper = gen proc() -> Generator<i64, (), i64> {
    let mine = gc Box { value: 5 }
    yield 0
    let noise = gc Box { value: 9 }
    yield mine.value
    return noise.value
}
let mut values = keeper()
values.start()
print(values.resume().yielded())
print(values.resume().completed())
"#
    );
    assert_eq!(lines(&source), ["5", "9"]);
}

#[test]
fn the_generator_examples_run_under_gc_stress_and_in_tiny_slices() {
    for (path, source, expected) in [
        (
            "sequence",
            include_str!("../examples/concurrency/generators/sequence.yas"),
            vec!["10", "11", "12", "complete: 3"],
        ),
        (
            "delegation",
            include_str!("../examples/concurrency/generators/delegation.yas"),
            vec!["yielded: 7", "complete: 42"],
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
