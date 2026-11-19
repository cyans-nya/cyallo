//! The verified-backed REPL: entries run one after another with their variables
//! kept, old statements are never replayed, errors leave state alone, and an
//! interrupted entry allows another. Expected values are written by hand.

use cyallo::{
    CompileOptions,
    vm::{
        Status,
        repl::{Repl, Submission},
    },
};

fn repl() -> Repl {
    Repl::new(CompileOptions::default()).unwrap()
}

fn entry(repl: &mut Repl, source: &str) {
    assert_eq!(
        repl.push_line(source).unwrap(),
        Submission::Ready,
        "{source}"
    );
    repl.run(1_000_000);
    assert_eq!(repl.session().status(), &Status::Finished, "{source}");
}

#[test]
fn variables_persist_and_output_is_not_replayed() {
    let mut r = repl();
    entry(&mut r, "let mut count = 0\nstd::io::print(\"once\")");
    entry(&mut r, "count = count + 1");
    entry(&mut r, "std::io::print(count)");
    assert_eq!(r.session().context.output, "once\n1\n");
    let globals = r.session().globals();
    assert!(
        globals.contains(&("count".into(), "1".into())),
        "{globals:?}"
    );
}

#[test]
fn functions_and_blocks_span_entries() {
    let mut r = repl();
    assert_eq!(
        r.push_line("proc twice(n: i64) -> i64 {").unwrap(),
        Submission::Incomplete
    );
    assert_eq!(
        r.push_line("    return n * 2").unwrap(),
        Submission::Incomplete
    );
    assert_eq!(r.push_line("}").unwrap(), Submission::Ready);
    r.run(1000);
    entry(&mut r, "std::io::print(twice(21))");
    assert_eq!(r.session().context.output, "42\n");
}

#[test]
fn heap_values_and_closures_survive_entries() {
    let mut r = repl();
    entry(
        &mut r,
        "let shared = rc \"kept\"\nlet cell = gc Cell::new(1)",
    );
    entry(&mut r, "cell.*.* = cell.*.* + 41");
    entry(
        &mut r,
        "std::io::print(shared.*, \" \", cell.*.*, \" \", shared.strong_count())",
    );
    assert_eq!(r.session().context.output, "kept 42 1\n");
}

#[test]
fn a_static_error_does_not_poison_later_entries() {
    let mut r = repl();
    entry(&mut r, "let x = 1");
    assert!(r.push_line("let y = nothing_here").is_err());
    assert!(!r.is_pending());
    entry(&mut r, "std::io::print(x + 1)");
    assert_eq!(r.session().context.output, "2\n");
}

#[test]
fn a_runtime_failure_keeps_earlier_writes() {
    let mut r = repl();
    entry(&mut r, "let mut total = 5\nlet zero = 0");
    assert_eq!(
        r.push_line("total = total + 1\nstd::io::print(1 / zero)")
            .unwrap(),
        Submission::Ready
    );
    r.run(1000);
    assert!(matches!(r.session().status(), Status::Failed(_)));
    entry(&mut r, "std::io::print(total)");
    assert_eq!(r.session().context.output, "6\n");
}

#[test]
fn an_interrupted_entry_allows_recovery_and_reset_clears_everything() {
    let mut r = repl();
    entry(&mut r, "let mut n = 0");
    assert_eq!(
        r.push_line("while true { n = n + 1 }").unwrap(),
        Submission::Ready
    );
    r.run(100);
    assert_eq!(r.session().status(), &Status::Paused);
    assert!(r.push_line("n").is_err());
    r.interrupt();
    entry(&mut r, "std::io::print(n > 0)");
    assert_eq!(r.session().context.output, "true\n");
    r.reset().unwrap();
    assert!(r.session().context.output.is_empty());
    assert!(r.push_line("std::io::print(n)").is_err());
}

#[test]
fn input_is_requested_once_per_entry() {
    let mut r = repl();
    entry(&mut r, "let mut text = \"\"");
    assert_eq!(
        r.push_line("std::runtime::caie::input(&mut text)").unwrap(),
        Submission::Ready
    );
    assert_eq!(*r.run(1000), Status::WaitingForInput);
    r.session_mut().context.input.push_back("hello".into());
    r.run(1000);
    assert_eq!(r.session().status(), &Status::Finished);
    entry(&mut r, "std::io::print(text)");
    assert_eq!(r.session().context.output, "hello\n");
}
