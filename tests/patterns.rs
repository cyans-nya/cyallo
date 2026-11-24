//! Nested patterns through the HIR/MIR/AIR pipeline: variants inside variants, tuple and
//! record patterns, tuple scrutinees, guards over nested bindings, and coverage that looks
//! through every constructor. Each test also checks which route lowered the routines.
//! matching a local or a call's result, wildcard arms, bindings scoped to their arm, and
//! the exhaustiveness and arm-type errors. An enum is an owned value that is not `Copy`.
//! These tests state what the language does, and each checks which route lowered the
//! routines it relies on.
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



const TYPES: &str = "enum Inner {\n    A: i64,\n    B,\n}\n\
enum Outer {\n    Wrap: Inner,\n    Plain: i64,\n}\n\
enum Color {\n    Red,\n    Green,\n}\n";

fn program(rest: &str) -> String {
    format!("{TYPES}{rest}")
}

#[test]
fn a_variant_pattern_nests_inside_another() {
    let source = program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Wrap(Inner::A(n)) => n,\n        Outer::Wrap(Inner::B) => -1,\n        Outer::Plain(n) => n * 2,\n    }\n}\n\
         std::io::print(f(Outer::Wrap(Inner::A(5))))\nstd::io::print(f(Outer::Wrap(Inner::B)))\nstd::io::print(f(Outer::Plain(4)))\n",
    );
    assert_eq!(run(&source), "5\n-1\n8\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_tuple_pattern_takes_a_payload_apart() {
    let source = "enum Wrapped {\n    Item: (i64, i64),\n    Nothing,\n}\n\
        fn total(w: Wrapped) -> i64 {\n    return match w {\n        Wrapped::Item((a, b)) => a * 10 + b,\n        Wrapped::Nothing => 0,\n    }\n}\n\
        std::io::print(total(Wrapped::Item((3, 4))))\nstd::io::print(total(Wrapped::Nothing))\n";
    assert_eq!(run(source), "34\n0\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_tuple_of_values_is_matched_part_by_part() {
    let source = program(
        "fn same(a: Color, b: Color) -> i64 {\n    return match (a, b) {\n        (Color::Red, Color::Red) => 1,\n        (Color::Green, Color::Green) => 2,\n        (Color::Red, Color::Green) => 0,\n        (Color::Green, Color::Red) => 0,\n    }\n}\n\
         std::io::print(same(Color::Red, Color::Red))\nstd::io::print(same(Color::Green, Color::Green))\nstd::io::print(same(Color::Red, Color::Green))\n",
    );
    assert_eq!(run(&source), "1\n2\n0\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_record_pattern_names_some_fields_and_ignores_the_rest() {
    let source = "record Entry {\n    key: i64\n    value: i64\n    label: string\n}\n\
        enum Item {\n    Pair: Entry,\n    Empty,\n}\n\
        fn read(i: Item) -> i64 {\n    return match i {\n        Item::Pair { key, value, .. } => key + value,\n        Item::Empty => 0,\n    }\n}\n\
        fn renamed(i: Item) -> i64 {\n    return match i {\n        Item::Pair { key: k, .. } => k,\n        Item::Empty => 0,\n    }\n}\n\
        std::io::print(read(Item::Pair(Entry { key: 4, value: 5, label: \"y\" })))\n\
        std::io::print(renamed(Item::Pair(Entry { key: 7, value: 0, label: \"z\" })))\n";
    assert_eq!(run(source), "9\n7\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_guard_sees_the_bindings_of_a_nested_pattern() {
    let source = program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Wrap(Inner::A(n)) if n > 10 => 100,\n        Outer::Wrap(Inner::A(n)) => n,\n        Outer::Wrap(Inner::B) => -1,\n        Outer::Plain(_) => 0,\n    }\n}\n\
         std::io::print(f(Outer::Wrap(Inner::A(50))))\nstd::io::print(f(Outer::Wrap(Inner::A(7))))\nstd::io::print(f(Outer::Plain(1)))\n",
    );
    assert_eq!(run(&source), "100\n7\n0\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn alternatives_may_take_values_apart_and_bind_the_same_name() {
    let source = program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Wrap(Inner::A(n)) | Outer::Plain(n) => n,\n        Outer::Wrap(Inner::B) => -1,\n    }\n}\n\
         std::io::print(f(Outer::Wrap(Inner::A(5))))\nstd::io::print(f(Outer::Plain(6)))\nstd::io::print(f(Outer::Wrap(Inner::B)))\n",
    );
    assert_eq!(run(&source), "5\n6\n-1\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_binding_that_is_not_copy_spends_the_matched_value() {
    let ok = "enum Boxed {\n    Text: (string, i64),\n    Empty,\n}\n\
        fn name(b: Boxed) -> string {\n    return match b {\n        Boxed::Text((s, _)) => s,\n        Boxed::Empty => \"none\",\n    }\n}\n\
        std::io::print(name(Boxed::Text((\"hi\", 1))))\nstd::io::print(name(Boxed::Empty))\n";
    assert_eq!(run(ok), "hi\nnone\n");
    assert_lowered_by_the_pipeline(ok);
    let error = failure(
        "enum Boxed {\n    Text: (string, i64),\n    Empty,\n}\n\
         fn twice(b: Boxed) -> string {\n    let first = match b {\n        Boxed::Text((s, _)) => s,\n        Boxed::Empty => \"none\",\n    }\n    let again = match b {\n        Boxed::Text((s, _)) => s,\n        Boxed::Empty => \"none\",\n    }\n    return first\n}\n\
         std::io::print(twice(Boxed::Empty))\n",
    );
    assert_eq!(error.code, "E118", "{}", error.message);
}

#[test]
fn coverage_looks_through_every_constructor() {
    let error = failure(&program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Wrap(Inner::A(n)) => n,\n        Outer::Plain(n) => n,\n    }\n}\nstd::io::print(f(Outer::Plain(1)))\n",
    ));
    assert_eq!(error.code, "E185", "{}", error.message);
    assert!(error.message.contains("Wrap(B)"), "{}", error.message);
    // A guarded arm does not count towards coverage.
    let error = failure(&program(
        "fn f(a: Color, b: Color) -> i64 {\n    return match (a, b) {\n        (Color::Red, _) => 1,\n        (Color::Green, Color::Red) => 2,\n        (Color::Green, Color::Green) if false => 3,\n    }\n}\nstd::io::print(f(Color::Red, Color::Red))\n",
    ));
    assert_eq!(error.code, "E185", "{}", error.message);
    assert!(error.message.contains("(Green, Green)"), "{}", error.message);
}

#[test]
fn a_pattern_that_does_not_fit_its_value_is_an_error() {
    let error = failure(&program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Wrap((a, b)) => a,\n        Outer::Plain(n) => n,\n    }\n}\nstd::io::print(f(Outer::Plain(1)))\n",
    ));
    assert_eq!(error.code, "E185", "{}", error.message);
    let error = failure(&program(
        "fn f(o: Outer) -> i64 {\n    return match o {\n        Outer::Plain => 0,\n        _ => 1,\n    }\n}\nstd::io::print(f(Outer::Plain(1)))\n",
    ));
    assert_eq!(error.code, "E185", "{}", error.message);
}
