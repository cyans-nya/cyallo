//! Records through the HIR/MIR/AIR pipeline: a literal, `p.a.b` reads and writes on a local,
//! a by-value parameter or a reference, and passing a record by value. A record is an owned
//! value that is not `Copy`, so handing one on moves it. These tests state what the
//! language does, and each checks which route lowered the routines it relies on.
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

const POINT: &str = "record Point {\n    x: i64,\n    y: i64,\n}\n";

#[test]
fn the_example_runs_and_its_routines_are_lowered_by_the_pipeline() {
    let source = include_str!("../examples/records/basic.yas");
    assert_lowered_by_the_pipeline(source);
    assert_eq!(run(source), "7\n23\n12\nstart!\n");
}

#[test]
fn a_literal_names_its_fields_in_any_order_and_nests() {
    let source = format!(
        "{POINT}record Line {{\n    from: Point,\n    to: Point,\n}}\n\
         fn length(line: Line) -> i64 {{ return line.to.x - line.from.x + line.to.y - line.from.y }}\n\
         fn make() -> Line {{ return Line {{ to: Point {{ y: 9, x: 5 }}, from: Point {{ x: 1, y: 2 }} }} }}\n\
         std::io::print(length(make()))\n"
    );
    assert_lowered_by_the_pipeline(&source);
    assert_eq!(run(&source), "11\n");
}

#[test]
fn fields_of_a_local_can_be_read_and_written() {
    let source = format!(
        "{POINT}proc work() -> i64 {{\n    let mut p = Point {{ x: 1, y: 2 }}\n    p.x = 10\n    p.y *= 3\n    p.x += p.y\n    return p.x\n}}\n\
         std::io::print(work())\n"
    );
    assert_lowered_by_the_pipeline(&source);
    assert_eq!(run(&source), "16\n");
}

#[test]
fn writing_a_field_needs_a_mutable_local_or_a_mutable_reference() {
    for (source, code) in [
        (
            format!("{POINT}proc f() {{\n    let p = Point {{ x: 1, y: 2 }}\n    p.x = 3\n}}\n"),
            "E107",
        ),
        (format!("{POINT}proc f(p: &Point) {{ p.x = 3 }}\n"), "E107"),
    ] {
        assert_eq!(failure(&source).code, code, "{source}");
    }
    let ok = format!("{POINT}proc f(p: &mut Point) {{ p.x = 3 }}\n");
    assert_lowered_by_the_pipeline(&ok);
}

#[test]
fn a_record_is_moved_whole_and_not_usable_afterwards() {
    let moved = format!(
        "{POINT}fn eat(p: Point) -> i64 {{ return p.x }}\nfn f(p: Point) -> i64 {{\n    let n = eat(p)\n    return p.y\n}}\n"
    );
    let error = failure(&moved);
    assert_eq!(error.code, "E118");
    assert!(error.message.contains("moved"), "{error:?}");
    // Reading fields leaves the record usable, and a field of Copy type is just read.
    let fine = format!("{POINT}fn f(p: Point) -> i64 {{ return p.x + p.y + p.x }}\n");
    assert_lowered_by_the_pipeline(&fine);
}

#[test]
fn a_field_of_an_owned_type_cannot_be_moved_out_but_can_be_read_in_place() {
    let named = "record Named {\n    label: string,\n    n: i64,\n}\n";
    let error = failure(&format!(
        "{named}fn f(v: Named) -> string {{ return v.label }}\n"
    ));
    assert_eq!(error.code, "E118");
    assert!(error.message.contains("move the whole record"), "{error:?}");
    let ok = format!(
        "{named}fn f(v: Named) -> string {{ return v.label + \"!\" }}\n\
                      std::io::print(f(Named {{ label: \"hi\", n: 1 }}))\n"
    );
    assert_lowered_by_the_pipeline(&ok);
    assert_eq!(run(&ok), "hi!\n");
}

#[test]
fn literals_and_fields_are_checked() {
    for (source, code, what) in [
        (
            format!("{POINT}fn f() -> Point {{ return Point {{ x: 1 }} }}\n"),
            "E116",
            "a missing field",
        ),
        (
            format!("{POINT}fn f() -> Point {{ return Point {{ x: 1, y: 2, z: 3 }} }}\n"),
            "E116",
            "an unknown field",
        ),
        (
            format!("{POINT}fn f() -> Point {{ return Point {{ x: 1, x: 2, y: 3 }} }}\n"),
            "E101",
            "a repeated field",
        ),
        (
            format!("{POINT}fn f() -> Point {{ return Point {{ x: true, y: 2 }} }}\n"),
            "E109",
            "a wrong field type",
        ),
        (
            format!("{POINT}fn f(p: Point) -> i64 {{ return p.z }}\n"),
            "E116",
            "reading an unknown field",
        ),
        (
            format!("{POINT}fn f(p: Point) -> i64 {{ return p.x.y }}\n"),
            "E116",
            "a field of a number",
        ),
    ] {
        assert_eq!(failure(&source).code, code, "{what}");
    }
}

#[test]
fn a_record_type_that_is_not_defined_is_an_error() {
    assert!(
        compile_with_options("fn f() -> i64 { return Missing { x: 1 }.x }\n", options()).is_err()
    );
}
