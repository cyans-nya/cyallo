//! Local fixed-size arrays through the HIR/MIR/AIR pipeline: a literal, element reads,
//! element writes and compound writes. The pipeline defines the behavior; `lowered` proves
//! which route ran.
use cyallo::{CompileOptions, Diagnostic, Session, Status, compile_with_options};
use std::sync::Arc;

fn options() -> CompileOptions {
    CompileOptions {
        jobs: 1,
        ..Default::default()
    }
}

fn run_to_end(source: &str) -> Session {
    let program = compile_with_options(source, options())
        .unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..4096 {
        match session.run(1 << 16) {
            Status::Paused => {}
            _ => break,
        }
    }
    session
}

fn run(source: &str) -> String {
    let session = run_to_end(source);
    assert_eq!(session.status(), &Status::Finished, "{source}");
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
fn the_arrays_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/arrays/basic.yas");
    assert_eq!(run(source), "122\n3\n4\n");
    assert_eq!(lowered(source), 3);
}

#[test]
fn elements_read_write_and_compound_assign() {
    let source = "proc f(n: i64) -> i64 {\n    let mut a = [n, 10, 20]\n    a[1] = a[0] + 1\n    a[2] -= 5\n    a[0] *= 3\n    return a[0] + a[1] + a[2]\n}\nstd::io::print(f(2))";
    // a = [6, 3, 15]
    assert_eq!(run(source), "24\n");
    assert_eq!(lowered(source), 1);
}

#[test]
fn an_element_is_typed_by_the_literal() {
    let source = "fn half(x: f64) -> f64 {\n    let mut a = [x, 1.5]\n    a[0] /= 2.0\n    return a[0] + a[1]\n}\nstd::io::print(half(5.0))";
    assert_eq!(run(source), "4\n");
    assert_eq!(lowered(source), 1);
}

#[test]
fn an_index_outside_the_array_fails_at_run_time() {
    let source = "proc f(i: i64) -> i64 {\n    let a = [1, 2, 3]\n    return a[i]\n}\nstd::io::print(f(7))";
    let session = run_to_end(source);
    assert!(matches!(session.status(), Status::Failed(_)), "{:?}", session.status());
}

#[test]
fn an_immutable_array_cannot_be_written() {
    let error = failure(
        "proc f() -> i64 {\n    let a = [1, 2]\n    a[0] = 3\n    return a[0]\n}\nstd::io::print(f())",
    );
    assert_eq!(error.code, "E107", "{}", error.message);
}

#[test]
fn an_index_must_be_an_integer() {
    let error = failure(
        "proc f() -> i64 {\n    let a = [1, 2]\n    return a[1.5]\n}\nstd::io::print(f())",
    );
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn array_elements_must_agree() {
    let error = failure(
        "proc f() -> i64 {\n    let a = [1, true]\n    return a[0]\n}\nstd::io::print(f())",
    );
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn a_value_that_is_not_an_array_cannot_be_indexed() {
    let error = failure("proc f(n: i64) -> i64 {\n    let m = n\n    return m[0]\n}\nstd::io::print(f(1))");
    assert_eq!(error.code, "E108", "{}", error.message);
}

#[test]
fn an_owned_element_cannot_be_moved_out() {
    let error = failure(
        "proc f() -> string {\n    let a = [\"x\", \"y\"]\n    return a[0]\n}\nstd::io::print(f())",
    );
    assert_eq!(error.code, "E118", "{}", error.message);
}
