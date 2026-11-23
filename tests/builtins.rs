//! Built-in string and numeric helpers through the HIR/MIR/AIR pipeline. Each is typed from
//! its operands alone and emitted as one operation; numeric literals take the type of the
//! other arguments, and a wrong operand type is the same E112 the existing route reports.
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
    let source = include_str!("../examples/builtins/basic.yas");
    assert_eq!(run(source), "HELLO\n6\ntrue\nfalse\n10\n0\n5\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn literals_take_the_type_of_the_other_numeric_arguments() {
    let source = "fn small(x: i32) -> i32 { return std::num::min(x, 3) }\nstd::io::print(small(9))\nstd::io::print(small(1))";
    assert_eq!(run(source), "3\n1\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_helper_result_feeds_a_local_and_a_condition() {
    let source = "fn f(s: string) -> i64 {\n    let n = std::string::len(s)\n    if std::string::ends_with(s, \"x\") { return n + 100 }\n    return n\n}\nstd::io::print(f(\"abx\"))\nstd::io::print(f(\"ab\"))";
    assert_eq!(run(source), "103\n2\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_helper_reads_its_string_arguments_without_moving_them() {
    let source = "fn f(s: string) -> i64 {\n    let a = std::string::len(s)\n    let b = std::string::len(s)\n    return a + b\n}\nstd::io::print(f(\"abc\"))";
    assert_eq!(run(source), "6\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_wrong_operand_type_is_an_error_naming_the_arguments() {
    let error = failure("fn f(n: i64) -> i64 { return std::string::len(n) }\nstd::io::print(f(1))");
    assert_eq!(error.code, "E112");
    assert!(error.message.contains("i64"), "{}", error.message);
}

#[test]
fn a_moved_string_cannot_be_measured() {
    let source = "fn eat(s: string) -> i64 { return 0 }\nfn f(s: string) -> i64 {\n    let n = eat(s)\n    return n + std::string::len(s)\n}\nstd::io::print(f(\"a\"))";
    assert_eq!(failure(source).code, "E118");
}
