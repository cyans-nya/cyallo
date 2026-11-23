//! Generic enums and records through the HIR/MIR/AIR pipeline. A routine that is not itself
//! generic may take and return instances such as `Maybe<i64>`, because the module graph
//! resolves signature types and the specialized instance is an ordinary enum or record.
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

const MAYBE: &str = "enum Maybe<T> {\n    Some: T,\n    None,\n}\nuse Maybe::*\n";

#[test]
fn a_routine_takes_and_returns_an_enum_instance() {
    let source = format!(
        "{MAYBE}fn unwrap_or(m: Maybe<i64>, fallback: i64) -> i64 {{\n    return match m {{ Some(v) => v, None => fallback }}\n}}\n\
         fn wrap(n: i64) -> Maybe<i64> {{ return Some(n) }}\n\
         std::io::print(unwrap_or(wrap(4), 0))\nstd::io::print(unwrap_or(None, 9))"
    );
    assert_eq!(run(&source), "4\n9\n");
    assert_lowered_by_the_pipeline(&source);
}

const RESULT: &str = "enum Either<A, B> {\n    Left: A,\n    Right: B,\n}\nuse Either::*\n";

#[test]
fn several_instances_of_one_enum_coexist() {
    let source = format!(
        "{MAYBE}fn count(m: Maybe<i64>) -> i64 {{ return match m {{ Some(v) => v, None => 0 }} }}\n\
         fn text(m: Maybe<string>) -> string {{ return match m {{ Some(s) => s, None => \"nothing\" }} }}\n\
         fn pack(s: string) -> Maybe<string> {{ return Some(s) }}\n\
         std::io::print(count(Some(5)))\nstd::io::print(text(pack(\"hi\")))\nstd::io::print(text(None))"
    );
    assert_eq!(run(&source), "5\nhi\nnothing\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn an_enum_with_two_parameters_and_nested_instances() {
    let source = format!(
        "{RESULT}fn pick(e: Either<i64, string>) -> string {{ return match e {{ Left(n) => \"number\", Right(s) => s }} }}\n\
         fn nest(n: i64) -> Either<Either<i64, bool>, string> {{ return Left(Right(n > 0)) }}\n\
         fn flag(e: Either<Either<i64, bool>, string>) -> bool {{ return match e {{ Left(Right(b)) => b, _ => false }} }}\n\
         std::io::print(pick(Left(1)))\nstd::io::print(pick(Right(\"r\")))"
    );
    // A nested pattern leaves `flag` to the existing route; the rest is the pipeline's.
    assert_eq!(run(&source), "number\nr\n");
}

#[test]
fn a_variant_cannot_stand_where_another_enum_is_expected() {
    let source = format!(
        "{MAYBE}{RESULT}fn count(m: Maybe<i64>) -> i64 {{ return match m {{ Some(v) => v, None => 0 }} }}\n\
         std::io::print(count(Left(1)))"
    );
    let error = failure(&source);
    assert!(
        matches!(error.code, "E106" | "E109" | "E112" | "E114"),
        "{} {}",
        error.code,
        error.message
    );
}

#[test]
fn a_variant_without_context_is_left_to_the_existing_route() {
    // `let m = Some(3)` has no annotation to fix the instance; the existing route infers
    // it from the later use, and the pipeline does not take the routine.
    let source = format!(
        "{MAYBE}fn count(m: Maybe<i64>) -> i64 {{ return match m {{ Some(v) => v, None => 0 }} }}\n\
         fn run_it() -> i64 {{\n    let m = Some(3)\n    return count(m)\n}}\nstd::io::print(run_it())"
    );
    assert_eq!(run(&source), "3\n");
}

#[test]
fn the_standard_option_and_result_still_work() {
    let source = include_str!("../examples/sdk/prelude.yas");
    assert_eq!(run(source), "4\nnone\n");
}
