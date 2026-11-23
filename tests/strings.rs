//! Strings through the HIR/MIR/AIR pipeline. A `string` is an owned value that is not
//! `Copy`: handing it on (a `let`, an assignment, a call argument, a return) moves it out of
//! its variable, reading it (an operand, a comparison) does not, and a moved variable may
//! not be used again until it is written. These tests state what the language does.
use cyallo::{Diagnostic, Session, Status, compile};
use cyallo_ir::analysis::IndexedRoutineBody;
use std::sync::Arc;

fn run(source: &str) -> String {
    let program = compile(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
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
    compile(source).expect_err(source)
}

fn assert_all_migrated(source: &str) {
    let file = cyallo_parser::parse_modern_snapshot(source.into(), false, cyallo::Jobs(1)).unwrap();
    let handles: Vec<_> = file.units().iter().flat_map(|unit| unit.bodies()).collect();
    assert!(!handles.is_empty());
    for handle in handles {
        if let Some(reason) = IndexedRoutineBody::decline_reason(&handle) {
            panic!("declined ({reason}):\n{source}");
        }
    }
}

#[test]
fn the_example_runs_and_every_routine_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/strings/basic.yas");
    assert_all_migrated(source);
    assert_eq!(run(source), "abcd\nHello, Ada!\nxyxy\ntrue false\ntwo\n");
}

#[test]
fn literals_concatenation_and_comparison() {
    let source = "fn join(a: string, b: string) -> string { return a + \"-\" + b }\n\
                  fn order(a: string, b: string) -> bool { return a < b }\n\
                  fn equal(a: string, b: string) -> bool { return a == b && !(a != b) }\n\
                  std::io::print(join(\"a\", \"b\"))\n\
                  std::io::print(order(\"apple\", \"banana\"), \" \", order(\"b\", \"a\"), \" \", equal(\"x\", \"x\"))\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "a-b\ntrue false true\n");
}

#[test]
fn reading_a_string_does_not_move_it() {
    // Operands of `+` and of a comparison are read, so the variable stays usable.
    let source = "fn f(s: string) -> string {\n    let both = s + s\n    let same = s == \"x\"\n    if same { return both }\n    return s + both\n}\n\
                  std::io::print(f(\"x\"))\nstd::io::print(f(\"y\"))\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "xx\nyyy\n");
}

#[test]
fn a_moved_string_cannot_be_used_until_it_is_written_again() {
    for (source, what) in [
        (
            "fn f(s: string) -> string {\n    let t = s\n    return s\n}\n",
            "after a let",
        ),
        (
            "fn eat(s: string) -> bool { return s == \"x\" }\n\
             fn f(s: string) -> string {\n    let ate = eat(s)\n    return s\n}\n",
            "after a call",
        ),
        (
            "fn f(s: string) -> string {\n    let t = s\n    return t + s\n}\n",
            "in an operand",
        ),
        (
            "fn f(s: string, c: bool) -> string {\n    if c {\n        let t = s\n    }\n    return s\n}\n",
            "after a branch that moves it",
        ),
        (
            "fn f(s: string, n: i64) -> string {\n    for i in 0..n {\n        let t = s\n    }\n    return \"done\"\n}\n",
            "in a loop, where the next pass would use it again",
        ),
    ] {
        let error = failure(source);
        assert_eq!(error.code, "E118", "{what}");
        assert!(error.message.contains("moved"), "{what}: {error:?}");
    }
    // Writing the variable again gives it a value, on every path.
    for source in [
        "fn f(s: string) -> string {\n    let mut t = s\n    let kept = t\n    t = \"new\"\n    return t + kept\n}\n",
        "fn f(s: string, c: bool) -> string {\n    let mut t = s\n    if c {\n        let kept = t\n        t = \"a\"\n    }\n    return \"x\"\n}\n",
    ] {
        assert!(compile(source).is_ok(), "{source}");
    }
}

#[test]
fn a_string_behind_a_reference_can_be_read_but_not_moved_out() {
    let ok = "fn size_matches(s: &string) -> bool { return s.* + \"!\" == \"hi!\" }\n\
              std::io::print(size_matches(&\"hi\"))\n";
    // A borrow must name a variable, so give it one.
    let ok = ok.replace(
        "std::io::print(size_matches(&\"hi\"))\n",
        "let word = \"hi\"\nstd::io::print(size_matches(&word))\n",
    );
    assert_all_migrated(&ok);
    assert_eq!(run(&ok), "true\n");
    let error = failure("fn take(s: &string) -> string { return s.* }\n");
    assert_eq!(error.code, "E118");
    assert!(error.message.contains("clone it explicitly"), "{error:?}");
}

#[test]
fn a_string_can_be_written_through_a_mutable_reference() {
    let source = "proc shout(text: &mut string) {\n    text.* += \"!\"\n    text.* = text.* + \"?\"\n}\n\
                  let mut word = \"hey\"\nshout(&mut word)\nstd::io::print(word)\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "hey!?\n");
    let error = failure("proc shout(text: &string) { text.* += \"!\" }\n");
    assert_eq!(error.code, "E107");
}

#[test]
fn strings_do_not_mix_with_numbers() {
    assert_eq!(
        failure("fn f(s: string) -> i64 { return s }\n").code,
        "E109"
    );
    assert_eq!(
        failure("fn f() -> i64 {\n    let n: i64 = \"x\"\n    return n\n}\n").code,
        "E109"
    );
    assert!(compile("fn f(s: string) -> string { return s + 1 }\n").is_err());
    assert_eq!(
        failure("fn f(s: string) -> string { return -s }\n").code,
        "E105"
    );
}
