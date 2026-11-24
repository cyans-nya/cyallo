//! Tuples through the HIR/MIR/AIR pipeline: construction, element reads and writes, and
//! tuples as parameters and results. Patterns that destructure a tuple stay on the old route.
use cyallo::{CompileOptions, Diagnostic, Session, Status, compile_with_options};
use std::sync::Arc;

fn options() -> CompileOptions {
    CompileOptions {
        jobs: 1,
        ..Default::default()
    }
}

fn run(source: &str) -> String {
    let program = compile_with_options(source, options())
        .unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..4096 {
        match session.run(1 << 16) {
            Status::Finished => break,
            Status::Paused => {}
            other => panic!("unexpected status {other:?}\n{source}"),
        }
    }
    session.context.output
}

fn failure(source: &str) -> Diagnostic {
    compile_with_options(source, options()).expect_err(source)
}

fn lowered(source: &str) -> usize {
    let before = cyallo_ir::analysis::bodies_emitted_on_this_thread();
    compile_with_options(source, options()).unwrap();
    cyallo_ir::analysis::bodies_emitted_on_this_thread() - before
}

#[test]
fn the_tuples_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/tuples/basic.yas");
    assert_eq!(run(source), "2110\n");
    // swapped, total, mixed and demo.
    assert_eq!(lowered(source), 4);
}

#[test]
fn a_tuple_literal_takes_its_element_types_from_the_expected_tuple() {
    let source = "fn small() -> (u8, i64) {\n    return (3, 4)\n}\nfn read() -> i64 {\n    let t = small()\n    return t.1\n}\nstd::io::print(read())";
    assert_eq!(run(source), "4\n");
}

#[test]
fn an_element_past_the_end_is_unknown() {
    let error = failure("fn f() -> i64 {\n    let t = (1, 2)\n    return t.2\n}\nstd::io::print(f())");
    assert_eq!(error.code, "E116", "{}", error.message);
}

#[test]
fn a_tuple_result_must_match_its_declared_type() {
    let error = failure("fn f() -> (i64, bool) {\n    return (1, 2)\n}\nstd::io::print(f().0)");
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn an_owned_element_cannot_be_moved_out_of_a_tuple() {
    let error = failure("fn f() -> string {\n    let t = (\"a\", 1)\n    return t.0\n}\nstd::io::print(f())");
    assert_eq!(error.code, "E118", "{}", error.message);
}
