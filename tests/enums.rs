//! Enums and `match` through the HIR/MIR/AIR pipeline: unit and scalar-payload variants,
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

const SHAPE: &str =
    "enum Shape {\n    Circle: i64,\n    Square: i64,\n    Empty,\n}\nuse Shape::*\n";

fn program(rest: &str) -> String {
    format!("{SHAPE}{rest}")
}

#[test]
fn the_example_runs_and_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/enums/basic.yas");
    assert_eq!(run(source), "12\n25\n0\ntrue\nfalse\n22\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_payload_binding_is_visible_only_in_its_arm() {
    let source = program(
        "fn pick(s: Shape) -> i64 { return match s { Circle(r) => r, Square(r) => r + 100, Empty => 0 } }\n\
         std::io::print(pick(Circle(3)))\nstd::io::print(pick(Square(3)))\nstd::io::print(pick(Empty))",
    );
    assert_eq!(run(&source), "3\n103\n0\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_local_is_matched_in_place_and_stays_usable() {
    let source = program(
        "fn twice(s: Shape) -> i64 {\n    let a = match s { Circle(r) => r, _ => 0 }\n    let b = match s { Circle(r) => r * 2, _ => 0 }\n    return a + b\n}\n\
         std::io::print(twice(Circle(7)))",
    );
    assert_eq!(run(&source), "21\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_call_result_is_matched_and_a_wildcard_catches_the_rest() {
    let source = program(
        "fn make(n: i64) -> Shape {\n    if n > 0 { return Circle(n) }\n    return Empty\n}\n\
         fn size(n: i64) -> i64 { return match make(n) { Circle(r) => r, _ => -1 } }\n\
         std::io::print(size(5))\nstd::io::print(size(0))",
    );
    assert_eq!(run(&source), "5\n-1\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn arms_may_short_circuit_and_nest_matches() {
    let source = program(
        "fn both(a: Shape, b: Shape) -> bool {\n    return match a { Circle(x) => match b { Circle(y) => x > 0 && y > 0, _ => false }, _ => false }\n}\n\
         std::io::print(both(Circle(1), Circle(2)))\nstd::io::print(both(Circle(1), Empty))\nstd::io::print(both(Circle(0), Circle(2)))",
    );
    assert_eq!(run(&source), "true\nfalse\nfalse\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_match_in_a_loop_body_feeds_a_local() {
    let source = program(
        "fn count(n: i64) -> i64 {\n    let mut total = 0\n    for i in 0..n {\n        let v = match Square(i) { Square(s) => s, _ => 0 }\n        total += v\n    }\n    return total\n}\n\
         std::io::print(count(5))",
    );
    assert_eq!(run(&source), "10\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_missing_variant_is_a_non_exhaustive_match() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Circle(r) => r, Empty => 0 } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("missing"), "{}", error.message);
    assert!(error.message.contains("Square"), "{}", error.message);
}

#[test]
fn arms_must_agree_on_a_type() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Circle(r) => r, _ => true } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
}

#[test]
fn a_payload_pattern_must_match_the_declaration() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Empty(r) => r, _ => 0 } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
}

#[test]
fn an_enum_handed_on_cannot_be_matched_again() {
    let error = failure(&program(
        "fn eat(s: Shape) -> i64 { return 0 }\n\
         fn f(s: Shape) -> i64 {\n    let n = eat(s)\n    return n + match s { Empty => 1, _ => 2 }\n}\nstd::io::print(f(Empty))",
    ));
    // The match cannot sit under `+`, so the program leaves the pipeline: still a move error.
    assert_eq!(error.code, "E118");
}

#[test]
fn a_moved_enum_is_reported_at_the_match() {
    let error = failure(&program(
        "fn eat(s: Shape) -> i64 { return 0 }\n\
         fn f(s: Shape) -> i64 {\n    let n = eat(s)\n    let m = match s { Empty => 1, _ => 2 }\n    return n + m\n}\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E118");
}

const MESSAGE: &str = "record Point {\n    x: i64,\n    y: i64,\n}\nenum Message {\n    Text: string,\n    At: Point,\n    Quiet,\n}\nuse Message::*\n";

fn messages(rest: &str) -> String {
    format!("{MESSAGE}{rest}")
}

#[test]
fn a_string_payload_is_moved_into_its_binding() {
    let source = messages(
        "fn text(m: Message) -> string { return match m { Text(t) => t, _ => \"none\" } }\n\
         std::io::print(text(Text(\"hello\")))\nstd::io::print(text(Quiet))",
    );
    assert_eq!(run(&source), "hello\nnone\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_record_payload_is_moved_and_read_through_its_binding() {
    let source = messages(
        "fn total(m: Message) -> i64 { return match m { At(p) => p.x + p.y, _ => 0 } }\n\
         std::io::print(total(At(Point { x: 3, y: 4 })))\nstd::io::print(total(Quiet))",
    );
    assert_eq!(run(&source), "7\n0\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_payload_ignored_with_a_wildcard_leaves_the_value_intact() {
    let source = messages(
        "fn twice(m: Message) -> i64 {\n    let a = match m { Text(_) => 1, _ => 0 }\n    let b = match m { Text(_) => 10, _ => 0 }\n    return a + b\n}\n\
         std::io::print(twice(Text(\"x\")))",
    );
    assert_eq!(run(&source), "11\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn matching_a_moved_payload_again_is_a_use_of_a_moved_value() {
    let error = failure(&messages(
        "fn f(m: Message) -> string {\n    let a = match m { Text(t) => t, _ => \"none\" }\n    let b = match m { Text(t) => t, _ => \"none\" }\n    return a\n}\nstd::io::print(f(Quiet))",
    ));
    assert_eq!(error.code, "E118");
}

#[test]
fn only_the_arm_that_binds_spends_the_value() {
    // The wildcard arm does not move `m`, but the binding arm does, so a later use is
    // reported: a move on any path counts.
    let error = failure(&messages(
        "fn f(m: Message) -> i64 {\n    let n = match m { Text(t) => 1, _ => 0 }\n    let k = match m { Quiet => 1, _ => 2 }\n    return n + k\n}\nstd::io::print(f(Quiet))",
    ));
    assert_eq!(error.code, "E118");
}

#[test]
fn an_enum_may_carry_another_enum() {
    let source = "enum Inner {\n    One: string,\n    Two,\n}\nenum Outer {\n    Wrap: Inner,\n    Bare,\n}\nuse Inner::*\nuse Outer::*\n\
fn dig(o: Outer) -> string {\n    return match o { Wrap(i) => match i { One(s) => s, Two => \"two\" }, Bare => \"bare\" }\n}\n\
std::io::print(dig(Wrap(One(\"deep\"))))\nstd::io::print(dig(Wrap(Two)))\nstd::io::print(dig(Bare))";
    assert_eq!(run(source), "deep\ntwo\nbare\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn the_guard_example_runs_and_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/enums/guards.yas");
    assert_eq!(run(source), "-1\n0\n9\n100\n1\n-2\napple\nother\nnone\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_failed_guard_falls_through_to_a_later_arm_for_the_same_variant() {
    let source = program(
        "fn f(s: Shape) -> i64 { return match s { Circle(r) if r > 5 => 1, Circle(_) => 2, _ => 3 } }\n\
         std::io::print(f(Circle(9)))\nstd::io::print(f(Circle(1)))\nstd::io::print(f(Empty))",
    );
    assert_eq!(run(&source), "1\n2\n3\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_guard_may_short_circuit_and_use_other_locals() {
    let source = program(
        "fn f(s: Shape, limit: i64) -> i64 { return match s { Circle(r) if r > 0 && r < limit => r, _ => -1 } }\n\
         std::io::print(f(Circle(3), 10))\nstd::io::print(f(Circle(30), 10))\nstd::io::print(f(Empty, 10))",
    );
    assert_eq!(run(&source), "3\n-1\n-1\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn guarded_arms_do_not_make_a_match_exhaustive() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Circle(r) if r > 0 => r, Square(r) => r, Empty => 0 } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("Circle"), "{}", error.message);
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { _ if true => 1 } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
}

#[test]
fn a_guard_must_be_boolean() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Circle(r) if r => 1, _ => 0 } }\nstd::io::print(f(Empty))",
    ));
    assert!(
        error.code == "E185" || error.code == "E109",
        "{}",
        error.code
    );
}

#[test]
fn a_guard_cannot_move_a_payload_before_its_arm_is_chosen() {
    let error = failure(&messages(
        "fn eat(t: string) -> bool { return true }\n\
         fn f(m: Message) -> i64 { return match m { Text(t) if eat(t) => 1, _ => 0 } }\nstd::io::print(f(Quiet))",
    ));
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("guard"), "{}", error.message);
}

#[test]
fn the_block_example_runs_and_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/enums/blocks.yas");
    assert_eq!(run(source), "click 8\nkey a\nlong key ab\n31\n6\n-1\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn a_statement_match_with_block_arms_runs_one_arm() {
    let source = program(
        "proc show(s: Shape) {\n    match s {\n        Circle(r) => {\n            std::io::print(\"circle\")\n            std::io::print(r)\n        }\n        Square(_) => std::io::print(\"square\"),\n        Empty => {}\n    }\n}\n\
         show(Circle(3))\nshow(Square(1))\nshow(Empty)",
    );
    assert_eq!(run(&source), "circle\n3\nsquare\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn break_and_continue_in_an_arm_leave_the_enclosing_loop() {
    let source = program(
        "proc scan(limit: i64) {\n    let mut i = 0\n    while i < limit {\n        i += 1\n        match Circle(i) {\n            Circle(n) if n == 2 => { continue }\n            Circle(n) if n == 4 => { break }\n            Circle(n) => { std::io::print(n) }\n            _ => {}\n        }\n    }\n}\nscan(9)",
    );
    assert_eq!(run(&source), "1\n3\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_value_match_block_may_end_in_a_return() {
    let source = program(
        "fn f(s: Shape) -> i64 {\n    let n = match s {\n        Circle(r) => { r + 1 }\n        Square(_) => { return 99 }\n        Empty => 0\n    }\n    return n * 2\n}\n\
         std::io::print(f(Circle(4)))\nstd::io::print(f(Square(1)))\nstd::io::print(f(Empty))",
    );
    assert_eq!(run(&source), "10\n99\n0\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_statement_match_must_be_exhaustive_and_bind_in_its_arm_only() {
    let error = failure(&program(
        "proc f(s: Shape) {\n    match s {\n        Circle(_) => {}\n        Square(_) => {}\n    }\n}\nf(Empty)",
    ));
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("Empty"), "{}", error.message);
}

#[test]
fn a_block_arm_moves_its_payload_and_spends_the_value() {
    let source = messages(
        "fn len(m: Message) -> i64 {\n    let n = match m {\n        Text(t) => { std::string::len(t) }\n        _ => 0\n    }\n    let k = match m { Quiet => 1, _ => 2 }\n    return n + k\n}\nstd::io::print(len(Quiet))",
    );
    assert_eq!(failure(&source).code, "E118");
}

#[test]
fn the_or_pattern_example_runs_and_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/enums/or_patterns.yas");
    assert_eq!(
        run(source),
        "true\ntrue\nfalse\n4\n5\n6\n0\nangular 7\nangular 8\nnothing\nfalse\ntrue\n"
    );
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn alternatives_cover_their_variants_for_exhaustiveness() {
    let error = failure(&program(
        "fn f(s: Shape) -> i64 { return match s { Circle(_) | Empty => 1 } }\nstd::io::print(f(Empty))",
    ));
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("Square"), "{}", error.message);
    let source = program(
        "fn f(s: Shape) -> i64 { return match s { Circle(_) | Square(_) | Empty => 1 } }\nstd::io::print(f(Empty))",
    );
    assert_eq!(run(&source), "1\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_shared_binding_must_have_one_type() {
    let error = failure(&messages(
        "fn f(m: Message) -> i64 { return match m { Text(x) | At(x) => 1, Quiet => 0 } }\nstd::io::print(f(Quiet))",
    ));
    assert_eq!(error.code, "E185");
}

#[test]
fn an_or_pattern_may_bind_a_moved_payload() {
    let source = "enum Box {\n    Name: string,\n    Title: string,\n    Nothing,\n}\nuse Box::*\n\
fn label(b: Box) -> string {\n    return match b { Name(s) | Title(s) => s, Nothing => \"-\" }\n}\n\
std::io::print(label(Title(\"dr\")))\nstd::io::print(label(Nothing))";
    assert_eq!(run(source), "dr\n-\n");
    assert_lowered_by_the_pipeline(source);
}

#[test]
fn an_or_pattern_may_be_guarded_when_it_binds_nothing() {
    let source = program(
        "fn f(s: Shape, ok: bool) -> i64 { return match s { Empty | Circle(_) if ok => 1, _ => 2 } }\n\
         std::io::print(f(Empty, true))\nstd::io::print(f(Empty, false))\nstd::io::print(f(Square(1), true))",
    );
    assert_eq!(run(&source), "1\n2\n2\n");
    assert_lowered_by_the_pipeline(&source);
}

#[test]
fn a_variant_pattern_may_be_written_with_the_enum_name() {
    let source = "enum Wrapped {\n    Item: i64,\n    Nothing,\n}\n\
        fn total(w: Wrapped) -> i64 {\n    return match w {\n        Wrapped::Item(n) => n,\n        Wrapped::Nothing => 0,\n    }\n}\n\
        std::io::print(total(Wrapped::Item(7)))\nstd::io::print(total(Wrapped::Nothing))\n";
    assert_eq!(run(source), "7\n0\n");
    assert_lowered_by_the_pipeline(source);
    // Another enum's name is still wrong.
    let error = failure(
        "enum A {\n    X,\n}\nenum B {\n    X,\n}\n\
         fn f(a: A) -> i64 {\n    return match a {\n        B::X => 1,\n    }\n}\nstd::io::print(f(A::X))\n",
    );
    assert_eq!(error.code, "E185", "{}", error.message);
}
