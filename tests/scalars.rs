//! Numeric casts and character values through the HIR/MIR/AIR pipeline. Characters have no
//! verified-engine lowering, so those programs are only compiled.
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
fn the_casts_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/scalars/casts.yas");
    assert_eq!(run(source), "3.5\n9\n");
    // average, truncate and demo.
    assert_eq!(lowered(source), 3);
}

#[test]
fn only_a_number_can_be_cast_to_a_number() {
    let error = failure("fn f(b: bool) -> i64 { return b as i64 }\nstd::io::print(f(true))");
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn character_literals_and_comparisons_are_lowered() {
    let source = "fn vowel(c: char) -> bool {\n    return c == 'a' || c == 'e'\n}\nfn before(a: char, b: char) -> bool {\n    return a < b\n}\nfn first() -> char {\n    return 'x'\n}\n";
    assert_eq!(lowered(source), 3);
}

#[test]
fn casts_to_sized_integers_are_lowered() {
    let source = "fn byte(n: i64) -> u8 {\n    return n as u8\n}\nfn back(b: u8) -> i64 {\n    return b as i64\n}\n";
    assert_eq!(lowered(source), 2);
}
