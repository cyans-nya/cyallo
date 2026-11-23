//! `print` inside routines through the HIR/MIR/AIR pipeline: it lowers to an output with no
//! callee, takes any number of numbers, strings and booleans, and rejects other types with
//! the same E112 the existing route reports.
use cyallo::{CompileOptions, Diagnostic, Session, Status, compile_with_options};
use cyallo_ir::analysis::{IndexedRoutineBody, bodies_emitted_on_this_thread};
use std::sync::Arc;

fn options() -> CompileOptions {
    // One job keeps compilation on this thread, so the lowering counter is readable.
    CompileOptions {
        jobs: 1,
        ..Default::default()
    }
}

fn function_bodies(source: &str) -> usize {
    let file = cyallo_parser::parse_modern_snapshot(source.into(), false, cyallo::Jobs(1)).unwrap();
    file.units()
        .iter()
        .flat_map(|unit| unit.bodies())
        .filter(|handle| IndexedRoutineBody::decline_reason(handle).is_none())
        .count()
}

/// Compile, returning the program and how many bodies the pipeline lowered.
fn compiled(source: &str) -> (cyallo::ir::Program, usize) {
    let before = bodies_emitted_on_this_thread();
    let program = compile_with_options(source, options())
        .unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    (program, bodies_emitted_on_this_thread() - before)
}

fn run(source: &str) -> String {
    let (program, _) = compiled(source);
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..4096 {
        match session.run(1 << 16) {
            Status::Finished => break,
            Status::Paused => {}
            other => panic!("unexpected status {other:?}\n{source}"),
        }
    }
    assert!(matches!(session.status(), Status::Finished), "{source}");
    session.context.output
}

fn failure(source: &str) -> Diagnostic {
    compile_with_options(source, options()).expect_err(source)
}

/// Every routine that could be lowered by the pipeline was.
fn assert_lowered_by_the_pipeline(source: &str) {
    let eligible = function_bodies(source);
    assert!(eligible > 0);
    let (_, lowered) = compiled(source);
    assert_eq!(lowered, eligible, "{source}");
}

#[test]
fn the_example_runs_and_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/print/basic.yas");
    assert_eq!(
        run(source),
        "n = 7\nready: yes\ndone: no\nn = 3\nn = 2\nn = 1\n"
    );
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn print_takes_several_arguments_of_scalar_and_string_types() {
    let source = "proc all(a: i64, b: f64, c: bool, s: string) { std::io::print(a, \" \", b, \" \", c, \" \", s) }\nall(1, 2.5, true, \"x\")";
    assert_eq!(run(source), "1 2.5 true x\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_procedure_prints_between_calls() {
    let source =
        "proc twice(n: i64) {\n    std::io::print(n)\n    std::io::print(n * 2)\n}\ntwice(4)";
    assert_eq!(run(source), "4\n8\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_pure_function_cannot_print() {
    let error = failure(
        "fn twice(n: i64) -> i64 {\n    std::io::print(n)\n    return n * 2\n}\nstd::io::print(twice(4))",
    );
    assert_eq!(error.code, "E120");
}

#[test]
fn printing_a_record_is_rejected_like_the_existing_route() {
    let source =
        "record P {\n    x: i64,\n}\nproc show(p: P) { std::io::print(p) }\nshow(P { x: 1 })";
    let error = failure(source);
    assert_eq!(error.code, "E112");
}

#[test]
fn a_moved_string_cannot_be_printed() {
    let source = "fn eat(s: string) -> i64 { return 0 }\nproc show(s: string) {\n    let n = eat(s)\n    std::io::print(s)\n}\nshow(\"a\")";
    assert_eq!(failure(source).code, "E118");
}
