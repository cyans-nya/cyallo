//! Native members of library enums called on parameters through the HIR/MIR/AIR pipeline.
//! The module graph accepts a call only when the parameter's declared type has a native
//! member of that name and no trait of the program names a method of that name.
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

const PRELUDE: &str = "use std::option::Option::{Some, None}\nuse std::result::Result::{Ok, Err}\n";

#[test]
fn the_members_example_runs_on_the_pipeline() {
    let source = include_str!("../examples/members/option_result.yas");
    assert_eq!(run(source), "4:9\nfalse:1\ntrue:false:false:true\n");
    // first_or, empty, required, succeeded and failed; `demo` holds annotated locals, which
    // the existing route still lowers.
    assert_eq!(lowered(source), 5);
}

#[test]
fn unwrapping_an_absent_value_fails_at_run_time() {
    let source = format!(
        "{PRELUDE}fn take(value: Option<i64>) -> i64 {{\n    return value.unwrap()\n}}\nproc demo() {{\n    let none: Option<i64> = None\n    std::io::print(take(none))\n}}\ndemo()"
    );
    let program = compile_with_options(&source, options()).unwrap();
    let mut session = Session::new(Arc::new(program)).unwrap();
    session.run(1 << 16);
    assert!(matches!(session.status(), Status::Failed(_)), "{:?}", session.status());
}

#[test]
fn a_value_consumed_by_a_member_cannot_be_used_again() {
    let error = failure(&format!(
        "{PRELUDE}fn twice(value: Option<i64>) -> i64 {{\n    let first = value.unwrap()\n    return first + value.unwrap()\n}}\nstd::io::print(0)"
    ));
    assert_eq!(error.code, "E118", "{}", error.message);
}

#[test]
fn a_member_that_consumes_cannot_be_called_through_a_reference() {
    let error = failure(&format!(
        "{PRELUDE}fn take(value: &Option<i64>) -> i64 {{\n    return value.unwrap()\n}}\nstd::io::print(0)"
    ));
    assert_eq!(error.code, "E177", "{}", error.message);
}

#[test]
fn a_method_the_type_does_not_have_is_unknown() {
    let error = failure(&format!(
        "{PRELUDE}fn probe(value: Option<i64>) -> bool {{\n    return value.nothing_like_this()\n}}\nstd::io::print(0)"
    ));
    assert!(matches!(error.code, "E116" | "E106"), "{} {}", error.code, error.message);
}

#[test]
fn a_trait_method_of_the_same_name_keeps_the_existing_route() {
    // The program names `is_some` in a trait, so the call is not taken for the native member.
    let source = format!(
        "{PRELUDE}trait Present {{\n    proc is_some(self: &Self) -> bool\n}}\nimpl Present for i64 {{\n    proc is_some(self: &Self) -> bool {{\n        return true\n    }}\n}}\nfn check(value: Option<i64>) -> bool {{\n    return value.is_some()\n}}\nproc demo() {{\n    let some: Option<i64> = Some(1)\n    std::io::print(check(some))\n}}\ndemo()"
    );
    assert_eq!(run(&source), "true\n");
    // `check` stays on the existing route; `demo` holds an annotated local, which does too.
    assert_eq!(lowered(&source), 0);
}

#[test]
fn option_and_result_are_built_matched_returned_and_passed_on_the_pipeline() {
    let source = include_str!("../examples/members/option_flow.yas");
    assert_eq!(run(source), "11:-1\nok:divide by zero\n");
    // find, unwrap_or, divide, describe, bump, chain and demo.
    assert_eq!(lowered(source), 7);
}

#[test]
fn a_match_on_an_option_must_cover_both_variants() {
    let error = failure(&format!(
        "{PRELUDE}fn f(value: Option<i64>) -> i64 {{\n    return match value {{\n        Some(v) => v,\n    }}\n}}\nstd::io::print(0)"
    ));
    assert_eq!(error.code, "E185", "{}", error.message);
}

#[test]
fn a_variant_payload_must_have_the_options_type() {
    let error = failure(&format!(
        "{PRELUDE}fn f() -> Option<i64> {{\n    return Some(true)\n}}\nstd::io::print(0)"
    ));
    assert_eq!(error.code, "E109", "{}", error.message);
}

#[test]
fn a_variant_of_another_enum_is_not_an_option() {
    let error = failure(&format!(
        "{PRELUDE}fn f() -> Option<i64> {{\n    return Ok(1)\n}}\nstd::io::print(0)"
    ));
    assert_eq!(error.code, "E109", "{}", error.message);
}
