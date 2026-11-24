use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

#[test]
fn matches_check_nested_patterns_guards_alternatives_and_block_tails() {
    output(
        r#"
enum Message<T> { Empty, Value: T, Pair: (T, bool), Row: record { n: T, text: string } }
use Message::Value as Make
fn number(value: Message<i64>) -> i64 {
    return match value {
        Message::Empty => 0,
        Make(n) => n,
        Message::Pair((n, true)) | Message::Pair((n, false)) => n + 1,
        Message::Row { n, text: _ } if n < 0 => 0,
        Message::Row { n, .. } => { let doubled = n * 2; doubled + 1 },
    }
}
std::io::print(number(Message::Value(3)), number(Message::Pair((4,true))), number(Message::Row(record { n: 5, text: "payload" })))
let value = Message::Value(7)
if let Message::Value(n) = &value { std::io::print(n.*) } else { std::io::print(0) }
let truth = match (true,false) { (true,true) => 1, (true,false) => 2, (false,_) => 3 }
std::io::print(truth)
"#,
        "3511\n7\n2\n",
    );
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn borrowed_payloads_mutate_original_storage_and_guards_commit_once() {
    output(
        r#"
enum Resource { Pair: (rc string, rc string), End }
proc guard(count: &mut i64) -> bool { count.* += 1; let garbage = [[1,2]]; return garbage[0][0] == 0 }
proc select(value: Resource, count: &mut i64) -> rc string {
    return match value {
        Resource::Pair((first, _)) if guard(count) => first,
        Resource::Pair((first, _)) => first,
        Resource::End => rc "empty",
    }
}
let first = rc "first"
let second = rc "second"
let weak_first = first.downgrade()
let weak_second = second.downgrade()
let mut value = Resource::Pair((first, second))
match &mut value {
    Resource::Pair((a,b)) => { a.* = rc "changed"; std::io::print(a.*.*, b.*.*) },
    Resource::End => {},
}
std::io::print(weak_first.upgrade().is_none())
let mut calls = 0
let selected = select(value, &mut calls)
std::io::print(selected.*, ":", selected.strong_count(), ":", calls, ":", weak_second.upgrade().is_none())
std::mem::drop(selected)
std::mem::drop(weak_first)
std::mem::drop(weak_second)
"#,
        "changedsecond\ntrue\nchanged:1:1:true\n",
    );
}
#[test]
fn invalid_patterns_and_borrows_are_rejected() {
    for source in [
        "enum E { A, B }\nlet x = match E::A { E::A => 1 }\n",
        "enum E { A: bool, B }\nlet x = match E::A(true) { E::A(true) => 1, E::B => 2 }\n",
        "let x = match (true,false) { (true,_) => 1, (_,true) => 2 }\n",
        "enum E { A }\nlet x = match E::A { E::A if true => 1 }\n",
        "enum E { A: i64 }\nlet x = match E::A(1) { E::A(a) | E::A(_) => 1 }\n",
        "enum E { A: i64, B: string }\nlet x = match E::A(1) { E::A(a) | E::B(a) => 1 }\n",
        "enum E { A }\nenum F { A }\nlet x = match E::A { F::A => 1 }\n",
        "enum E { A: i64 }\nlet x = match E::A(1) { E::A => 1 }\n",
        "let x = match true { true => 1, false => false }\n",
        "let x = match true { _ if 1 => 1, _ => 2 }\n",
        "enum E { A: string }\nlet x = E::A(\"moved\")\nlet y = match x { E::A(text) => text }\nstd::io::print(x == E::A(\"moved\"))\n",
        "enum E { A: string }\nproc consume(value: string) -> bool { return true }\nlet x = match E::A(\"owned\") { E::A(text) if consume(text) => 1, _ => 2 }\n",
        "enum E { A: i64 }\nproc mutate(value: &mut i64) -> bool { value.* = 2; return true }\nlet mut value = E::A(1)\nmatch &mut value { E::A(n) if mutate(n) => {}, _ => {} }\n",
        "enum E { A: i64 }\nproc bad() -> &i64 { let value = E::A(1); return match &value { E::A(n) => n } }\n",
        "enum E { A: i64 }\nif let E::A(n) = E::A(1) {}\nstd::io::print(n)\n",
    ] {
        assert!(
            compile(source).is_err(),
            "accepted invalid program: {source}"
        );
    }
}
#[test]
fn empty_enums_and_diverging_arms_keep_their_bottom_type() {
    output(
        r#"
enum Empty {}
fn absurd(value: Empty) -> i64 { return match value {} }
enum Choice { A: bool, B }
fn choose(value: Choice) -> i64 {
    return match value {
        Choice::A(true) => { return 1 },
        Choice::A(false) | Choice::B => { return 2 },
    }
}
std::io::print(choose(Choice::A(false)))
"#,
        "2\n",
    );
}
#[test]
fn payload_literals_and_borrowed_capture_escapes_are_checked() {
    output(
        r#"
enum E { Number: i64, Text: string }
let n = E::Number(4)
let value = match n { E::Number(1 | 2) => 0, E::Number(4) => 7, E::Number(_) => 9, E::Text(_) => 1 }
std::io::print(value)
let row = record { a: 2, b: true }
std::io::print(match row { { a, b: true } => a, { b: false, .. } => 0 })
"#,
        "7\n2\n",
    );
    for source in [
        "enum E { Value: i64 }\nfn bad() -> fn() -> i64 { let value = E::Value(3); return match &value { E::Value(n) => fn() -> i64 { return n.* } } }\n",
        "enum E { Value: gc i64 }\nlet value = E::Value(gc 1)\nlet borrowed = match value.* { _ => 0 }\n",
        "enum E { Value: record { n: i64 } }\nlet value = E::Value { bad: 3 }\n",
    ] {
        assert!(
            compile(source).is_err(),
            "accepted invalid payload/capture: {source}"
        );
    }
}

fn output(source: &str, expected: &str) {
    let program = Arc::new(compile(source).unwrap_or_else(|error| panic!("{error}\n{source}")));
    for stress in [false, true] {
        let mut session = Session::with_context(
            program.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig { stress, ..Default::default() },
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..100_000 {
            if session.run(1000) != &Status::Paused {
                break;
            }
        }
        assert_eq!(session.status(), &Status::Finished, "stress={stress}\n{source}");
        assert_eq!(session.context.output, expected, "stress={stress}\n{source}");
    }
}
