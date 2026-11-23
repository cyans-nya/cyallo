//! Generic routines through the HIR/MIR/AIR pipeline. A template is checked once, with its
//! type parameters opaque, as in Rust: the parameter says nothing about the instance, so a
//! body that needs to know more fails whatever the instance is. Instances over scalar types
//! are then lowered from the template's body.
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

#[test]
fn a_template_that_only_moves_its_values_is_instantiated() {
    let source = "fn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(1, 2))\nstd::io::print(first(\"x\", \"y\"))\nstd::io::print(first(true, false))";
    assert_eq!(run(source), "1\nx\ntrue\n");
}

#[test]
fn a_parameter_gives_the_body_no_information_about_its_type() {
    // Arithmetic needs a bound, and there is none: the template fails even for an instance
    // where `T` is a number.
    let error = failure("fn double<T>(a: T) -> T { return a + a }\nstd::io::print(double(2))");
    assert!(
        matches!(error.code, "E105" | "E118" | "E109"),
        "{} {}",
        error.code,
        error.message
    );
}

#[test]
fn matching_a_parameter_fails_even_when_the_instance_is_an_enum() {
    let source = "enum Foo {\n    Bar: i64,\n    Baz,\n}\n\
        fn first<T>(val: T) -> i64 {\n    return match val {\n        Foo::Bar(a) => a,\n        _ => 0,\n    }\n}\n\
        std::io::print(first(Foo::Bar(1)))";
    let error = failure(source);
    assert_eq!(error.code, "E185", "{}", error.message);
}

#[test]
fn an_unused_template_is_still_checked() {
    let error = failure("fn bad<T>(a: T) -> T { return a + a }\nstd::io::print(1)");
    assert!(
        matches!(error.code, "E105" | "E118" | "E109"),
        "{} {}",
        error.code,
        error.message
    );
}

#[test]
fn a_value_of_an_opaque_type_is_moved_not_copied() {
    let error = failure(
        "fn twice<T>(a: T) -> T {\n    let b = a\n    return a\n}\nstd::io::print(twice(1))",
    );
    assert_eq!(error.code, "E118");
}

fn lowered(source: &str) -> usize {
    let before = cyallo_ir::analysis::bodies_emitted_on_this_thread();
    compile_with_options(source, options()).unwrap();
    cyallo_ir::analysis::bodies_emitted_on_this_thread() - before
}

#[test]
fn instances_over_scalars_are_lowered_from_the_template_body() {
    // One instance per type argument, and the caller is the entry script (not lowered here).
    let source = "fn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(1, 2))\nstd::io::print(first(\"x\", \"y\"))\nstd::io::print(first(true, false))";
    assert_eq!(lowered(source), 3);
}

#[test]
fn an_instance_may_call_a_plain_routine_with_its_values() {
    let source = "fn keep(n: i64) -> i64 { return n }\n\
        fn through<T>(a: T, n: i64) -> i64 { return keep(n) }\n\
        std::io::print(through(\"s\", 4))\nstd::io::print(through(true, 5))";
    assert_eq!(run(source), "4\n5\n");
    assert_eq!(lowered(source), 3);
}

#[test]
fn an_instance_over_a_record_keeps_the_existing_route() {
    let source = "record P {\n    x: i64,\n}\nfn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(P { x: 1 }, P { x: 2 }).x)";
    assert_eq!(run(source), "1\n");
}
