//! The verified-backed session: the surface programs and embedders use to run
//! a program for a budget, read its output, feed it input and see why it
//! failed. Expected values are written by hand.

use cyallo::{
    compile,
    runtime::{ArithmeticError, GeneratorError, RuntimeError},
    vm::{Context, Session, Status},
};
use std::sync::Arc;

fn session(source: &str) -> Session {
    Session::new(Arc::new(compile(source).unwrap())).unwrap()
}

fn finish(session: &mut Session) {
    while *session.run(1000) == Status::Paused {}
}

#[test]
fn output_accumulates_in_the_context() {
    let mut s = session("std::io::print(\"a\")\nstd::io::print(1 + 2)\n");
    finish(&mut s);
    assert_eq!(*s.status(), Status::Finished);
    assert_eq!(s.context.output, "a\n3\n");
}

#[test]
fn a_budget_pauses_the_program_and_counts_instructions() {
    let mut s = session("let mut i = 0\nwhile i < 1000 { i += 1 }\nstd::io::print(i)\n");
    assert_eq!(*s.run(10), Status::Paused);
    assert_eq!(s.instruction_count(), 10);
    assert!(s.current_span().is_some());
    assert!(s.depth() >= 1);
    finish(&mut s);
    assert_eq!(s.context.output, "1000\n");
}

#[test]
fn one_step_runs_one_instruction() {
    let mut s = session("std::io::print(1)\n");
    assert!(s.step());
    assert_eq!(s.instruction_count(), 1);
}

#[test]
fn a_program_waits_for_input_and_continues_when_it_arrives() {
    let source =
        "let mut text = \"\"\nstd::runtime::caie::input(&mut text)\nstd::io::print(text)\n";
    let mut s = session(source);
    assert_eq!(*s.run(1000), Status::WaitingForInput);
    // Still waiting: nothing is queued, and nothing was consumed.
    assert_eq!(*s.run(1000), Status::WaitingForInput);
    s.context.input.push_back("21".into());
    finish(&mut s);
    assert_eq!(*s.status(), Status::Finished);
    assert_eq!(s.context.output, "21\n");
}

#[test]
fn failures_carry_the_older_error_and_the_span() {
    let mut s = session("let zero = 0\nstd::io::print(1 / zero)\n");
    finish(&mut s);
    let Status::Failed(failure) = s.status() else {
        panic!("{:?}", s.status());
    };
    assert_eq!(
        failure.error,
        RuntimeError::Arithmetic(ArithmeticError::DivisionByZero)
    );
    assert_ne!(failure.span, cyallo::Span::default());
}

#[test]
fn generator_misuse_maps_to_the_generator_errors() {
    let source = r#"
use std::generator::Generator
let once = gen proc() -> Generator<i64, (), i64> {
    yield 1
    return 2
}
let mut values = once()
values.resume()
"#;
    let mut s = session(source);
    finish(&mut s);
    let Status::Failed(failure) = s.status() else {
        panic!("{:?}", s.status());
    };
    assert_eq!(
        failure.error,
        RuntimeError::Generator(GeneratorError::NotStarted)
    );
}

#[test]
fn restart_runs_the_program_again() {
    let mut s = session("std::io::print(7)\n");
    finish(&mut s);
    s.restart().unwrap();
    assert_eq!(*s.status(), Status::Paused);
    finish(&mut s);
    assert_eq!(s.context.output, "7\n7\n");
}

#[test]
fn the_context_configures_frames_and_collection() {
    let source = "proc deep(n: i64) -> i64 { return deep(n + 1) }\nstd::io::print(deep(0))\n";
    let mut s = Session::with_context(
        Arc::new(compile(source).unwrap()),
        Context {
            max_frames: 16,
            ..Default::default()
        },
    )
    .unwrap();
    finish(&mut s);
    assert!(matches!(s.status(), Status::Failed(_)));
}
