//! Mutable script variables used by routines through the HIR/MIR/AIR pipeline: the linker
//! accepts a `let mut` of the entry script whose type is plain from the source, and the
//! routine reads and writes its global slot.
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
fn the_script_variables_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/script_variables.yas");
    assert_eq!(run(source), "2:7:true\n3\n101:2\n");
    // record, scaled and shadowed; the entry script is not lowered here.
    assert_eq!(lowered(source), 3);
}

#[test]
fn a_function_may_not_touch_a_mutable_script_variable() {
    let error = failure("let mut n = 1\nfn f() -> i64 { return n }\nstd::io::print(f())");
    assert_eq!(error.code, "E120", "{}", error.message);
}

#[test]
fn a_write_is_checked_against_the_variables_type() {
    let error = failure("let mut n = 1\nproc f() {\n    n = true\n}\nf()");
    assert_eq!(error.code, "E109", "{}", error.message);
    let error = failure("let mut n = 1\nproc f() {\n    n += 1.5\n}\nf()");
    assert!(matches!(error.code, "E109" | "E105"), "{} {}", error.code, error.message);
}

#[test]
fn an_annotated_variable_has_its_declared_type() {
    let source = "let mut big: i64 = 250\nproc bump() {\n    big += 3\n}\nbump()\nstd::io::print(big)";
    assert_eq!(run(source), "253\n");
    assert_eq!(lowered(source), 1);
}

#[test]
fn an_immutable_script_value_is_not_a_variable() {
    // Constants are substituted by the existing route, not read from a slot.
    let source = "let base = 10\nproc f(n: i64) -> i64 {\n    return n + base\n}\nstd::io::print(f(1))";
    assert_eq!(run(source), "11\n");
}
