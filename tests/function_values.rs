//! Function values through the HIR/MIR/AIR pipeline: a named function passed, held in a
//! local, called through it and returned. Closures stay on the existing route.
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
fn the_function_values_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/functions/values.yas");
    assert_eq!(run(source), "-2\n100\n");
    // double, square, negate, apply, twice, choose, show and demo.
    assert_eq!(lowered(source), 8);
}

#[test]
fn a_function_value_is_called_with_checked_arguments() {
    let error = failure(
        "fn inc(n: i64) -> i64 { return n + 1 }\nfn run(f: fn(i64) -> i64) -> i64 { return f(1, 2) }\nstd::io::print(run(inc))",
    );
    assert_eq!(error.code, "E110", "{}", error.message);
    let error = failure(
        "fn inc(n: i64) -> i64 { return n + 1 }\nfn run(f: fn(i64) -> i64) -> i64 { return f(true) }\nstd::io::print(run(inc))",
    );
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn a_value_that_is_not_a_function_cannot_be_called() {
    let error = failure("fn run(n: i64) -> i64 { return n(1) }\nstd::io::print(run(3))");
    assert_eq!(error.code, "E110", "{}", error.message);
}

#[test]
fn a_function_of_the_wrong_type_is_rejected() {
    let error = failure(
        "fn flag(n: i64) -> bool { return n > 0 }\nfn run(f: fn(i64) -> i64) -> i64 { return f(1) }\nstd::io::print(run(flag))",
    );
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn a_procedure_value_is_called_as_a_statement() {
    let source = "proc hello(n: i64) { std::io::print(n) }\nproc run(f: proc(i64), n: i64) { f(n)\n f(n + 1) }\nrun(hello, 4)";
    assert_eq!(run(source), "4\n5\n");
}

#[test]
fn a_function_value_behind_a_reference_is_called_through_it() {
    let source = include_str!("../examples/functions/references.yas");
    assert_eq!(run(source), "hello\nhello\n0\n1\n2\n");
    // twice, repeat, hello, count and demo.
    assert_eq!(lowered(source), 5);
}
