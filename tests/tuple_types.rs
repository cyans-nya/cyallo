//! Tuples through the HIR/MIR/AIR pipeline: tuple types in signatures, annotations, records
//! and enum payloads; tuple expressions; element reads and writes by position. Each test
//! also checks which route lowered the routines it relies on.
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


#[test]
fn tuples_are_passed_returned_and_read_by_position() {
    let source = "fn swap(p: (i64, i64)) -> (i64, i64) {\n    return (p.1, p.0)\n}\n\
        fn sum(p: (i64, i64)) -> i64 {\n    let t = swap(p)\n    return t.0 * 10 + t.1\n}\n\
        std::io::print(sum((3, 4)))\n";
    assert_eq!(run(source), "43\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_tuple_element_is_assigned_in_place() {
    let source = "fn bump(p: (i64, bool)) -> (i64, bool) {\n    let mut t = p\n    t.0 += 5\n    t.1 = !t.1\n    return t\n}\n\
        let r = bump((1, false))\nstd::io::print(r.0)\nstd::io::print(r.1)\n";
    assert_eq!(run(source), "6\ntrue\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn tuples_mix_element_types_and_nest_inside_records_and_payloads() {
    let source = "record Pair {\n    both: (i64, f64)\n    label: string\n}\n\
        enum Wrapped {\n    Item: (i64, i64),\n    Nothing,\n}\n\
        fn make(a: i64) -> Pair {\n    return Pair { both: (a, 1.5), label: \"p\" }\n}\n\
        fn total(w: Wrapped) -> i64 {\n    return match w {\n        Wrapped::Item(_) => 1,\n        Wrapped::Nothing => 0,\n    }\n}\n\
        let p = make(4)\nstd::io::print(p.both.0)\nstd::io::print(p.both.1)\nstd::io::print(p.label)\n\
        std::io::print(total(Wrapped::Item((1, 2))))\n";
    assert_eq!(run(source), "4\n1.5\np\n1\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_numeric_literal_in_a_tuple_takes_the_element_type_the_context_gives() {
    let source = "fn pick() -> (u8, i32) {\n    return (200, -7)\n}\n\
        let t = pick()\nstd::io::print(t.0)\nstd::io::print(t.1)\n";
    assert_eq!(run(source), "200\n-7\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_tuple_of_the_wrong_shape_is_a_type_error() {
    let error = failure("fn f() -> (i64, i64) {\n    return (1, true)\n}\nstd::io::print(f().0)\n");
    assert_eq!(error.code, "E109", "{}", error.message);
    let error = failure("fn f(p: (i64, i64)) -> i64 {\n    return p.2\n}\nstd::io::print(f((1, 2)))\n");
    assert_eq!(error.code, "E116", "{}", error.message);
}

#[test]
fn a_non_copy_element_cannot_be_moved_out_alone() {
    let error = failure(
        "fn first(p: (string, i64)) -> string {\n    return p.0\n}\nstd::io::print(first((\"a\", 1)))\n",
    );
    assert_eq!(error.code, "E118", "{}", error.message);
}
