use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use std::{sync::Arc, time::Duration};
fn output(source: &str, expected: &str) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let baseline = TieredBackend::default().compile(program.clone()).unwrap();
        for i in 0..program.functions().len() {
            baseline.precompile(i, Tier::Winch).unwrap();
        }
        assert!(baseline.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Box<dyn Executable>> = vec![
            Box::new(baseline),
            Box::new(CraneliftBackend.compile(program.clone()).unwrap()),
            Box::new(CraneliftSpeedBackend.compile(program.clone()).unwrap()),
        ];
        let mut instruction_count = None;
        for stress in [false, true] {
            for (engine_index, engine) in std::iter::once(None)
                .chain(engines.iter().map(Some))
                .enumerate()
            {
                for budget in [1, 100_000] {
                    let mut session = Session::with_context(
                        program.clone(),
                        Context {
                            gc: if stress {
                                cyallo::runtime::gc::GcConfig::stress()
                            } else {
                                Default::default()
                            },
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    for step in 0..100_000 {
                        if let Some(engine) = engine
                            && (budget > 1 || step % 3 != 0)
                        {
                            engine.run(&mut session, budget).unwrap();
                        } else {
                            session.run(budget);
                        }
                        if session.status() != &Status::Paused {
                            break;
                        }
                    }
                    assert_eq!(
                        session.status(),
                        &Status::Finished,
                        "deferred={deferred}, stress={stress}, engine={engine_index}, expected={expected}"
                    );
                    assert_eq!(session.context.output, expected);
                    if let Some(count) = instruction_count {
                        assert_eq!(session.instruction_count(), count);
                    } else {
                        instruction_count = Some(session.instruction_count());
                    }
                }
            }
        }
    }
}
#[test]
fn constructor_values_constants_and_generic_payload_storage() {
    output(
        r#"
enum Choice<T> { Empty, Value: T }
enum Other { Value: i64 }
const fixed = Other::Value(8)
fn accept(value: Choice<i64>) -> Choice<i64> { return value }
let make = Other::Value
let a = accept(Choice::Value(3))
let b = std::mem::clone(&a)
let none: Choice<i64> = Choice::Empty
std::io::print(a == b, none == Choice::Empty, fixed == make(8))
enum Packet { Data: record { count: i64, text: string }, Pair: (i64, string), End }
let packet = Packet::Data(record { text: "saved", count: 2 })
let copy = std::mem::clone(&packet)
let records = record { left: packet, right: copy }
std::io::print(records.left == records.right, Packet::End == Packet::End)
"#,
        "truetruetrue\ntruetrue\n",
    );
}
#[test]
fn structural_records_keep_managed_roots_across_handoffs() {
    output(
        r#"
proc make(n: i64) -> record { owner: gc (i64,string), row: record { n: i64 } } {
    return record { row: record { n: n }, owner: gc (n,"rooted") }
}
let mut sum = 0
for n in 1..=12 { let saved = make(n); let garbage = [[n,n+1]]; sum += saved.owner.*.0 + saved.row.n + garbage[0][1] }
std::io::print(sum)
"#,
        "246\n",
    );
}
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
fn standard_option_result_share_constructors_patterns_and_native_producers() {
    output(
        r#"
use std::result::Result::{Ok, Err}
use std::option::Option::{Some, None}
fn divide(a: i64, b: i64) -> Result<i64, record { divisor: i64, message: string }> {
    if b == 0 { return Err(record { message: "zero divisor", divisor: b }) }
    return Ok(a / b)
}
let good = divide(12,3)
let bad = divide(1,0)
std::io::print(match good { Ok(n) => n, Err(_) => -1 })
std::io::print(match &bad { Ok(n) => n.*, Err { divisor, message } => { std::io::print(message.*); divisor.* } })
let mut optional: Option<string> = Some("before")
if let Some(value) = &mut optional { value.* = "after" }
std::io::print(optional.unwrap())
let nothing: Option<i64> = None
std::io::print(match nothing { Some(value) => value, None => 99 })
let parsed = std::num::parse<i64>("42")
std::io::print(match parsed { Ok(n) => n, Err(_) => 0 })
let mut numbers = Vec[4,5]
std::io::print(match numbers.pop() { Some(n) => n, None => 0 })
let legacy: Result<i64,string> = Result::Ok(8)
std::io::print(match legacy { Ok(n) => n, Err(_) => 0 })
let resource: Option<rc string> = Some(rc "owned")
let extracted = match resource { Some(value) => value, None => rc "empty" }
std::io::print(extracted.*, ":", extracted.strong_count())
std::mem::drop(extracted)
"#,
        "4\nzero divisor\n0\nafter\n99\n42\n5\n8\nowned:1\n",
    );
}
#[test]
fn variant_braces_type_aliases_globs_callable_context_and_constants() {
    output(
        r#"
record Entry<T> { n: T, text: string }
enum Packet<T> { Row: record { n: T, text: string }, Named: Entry<T>, End }
use Packet::*
let a = Row { text: "anonymous", n: 3 }
let b = Named { n: 4, text: "nominal" }
let constructor: fn(i64) -> Option<i64> = Option::Some
std::io::print(constructor(9).unwrap())
std::io::print(match a { Row { n, .. } => n, Named(_) => 0, End => 0 })
std::io::print(match b { Named { n, text } => { const factor = 2 + 1; let scaled = n * factor; std::io::print(text); scaled }, _ => 0 })
enum Fixed { Value: i64, Empty }
type Alias = Fixed
const saved = match Fixed::Value(7) { Fixed::Value(n) => { const offset = 2; let computed = n + offset; computed }, Fixed::Empty => 0 }
std::io::print(match Alias::Value(saved) { Alias::Value(n) => n, Alias::Empty => 0 })
"#,
        "9\n3\nnominal\n12\n9\n",
    );
}
#[test]
fn standard_enum_constants_and_recursive_managed_payloads() {
    output(
        r#"
const some = Option::Some(3)
const result: Result<i64,string> = Result::Err("frozen")
std::io::print(match some { Option::Some(n) => n, Option::None => 0 })
std::io::print(match result { Result::Ok(_) => "unexpected", Result::Err(text) => text })
enum List<T> { End, Node: (T, gc List<T>) }
proc sum(value: &List<i64>) -> i64 {
    return match value { List::End => 0, List::Node((n,next)) => n.* + sum(&next.*.*) }
}
let end: gc List<i64> = gc List::End
let list = gc List::Node((3, gc List::Node((2,end))))
std::io::print(sum(&list.*))
"#,
        "3\nfrozen\n5\n",
    );
}
#[test]
fn enum_trait_dispatch_closure_captures_and_dynamic_opacity() {
    output(
        r#"
enum Number { Value: i64, Empty }
trait ReadNumber { proc number(self: &Self) -> i64 }
impl ReadNumber for Number {
    proc number(self: &Self) -> i64 { return match self { Number::Value(n) => n.*, Number::Empty => 0 } }
}
let boxed = Number::Value(4) as dyn ReadNumber
std::io::print(boxed.number())
fn capture(value: Number) -> fn() -> i64 {
    return match value { Number::Value(n) => fn() -> i64 { return n + 1 }, Number::Empty => fn() -> i64 { return 0 } }
}
let callback = capture(Number::Value(7))
std::io::print(callback())
let inner = fn(value: Number) -> i64 { return match value { Number::Value(n) => { let local = n + 2; local }, Number::Empty => 0 } }
std::io::print(inner(Number::Value(3)))
let erased: dynamic = Number::Value(9)
let restored = erased as Number
std::io::print(match restored { Number::Value(n) => n, Number::Empty => 0 })
"#,
        "4\n8\n5\n9\n",
    );
}
#[test]
fn documented_enum_examples_execute() {
    for (source, expected) in [
        (
            include_str!("../examples/enums/result.ya"),
            "4\ncannot divide by 0\n",
        ),
        (
            include_str!("../examples/enums/patterns.ya"),
            "positive: 7\nchanged\n2\n",
        ),
        (
            include_str!("../examples/enums/ownership.ya"),
            "first:1:1:true\n",
        ),
        (include_str!("../examples/enums/recursive.ya"), "5\n"),
    ] {
        output(source, expected);
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

#[test]
fn standard_generator_and_channel_statuses_are_matchable_enums() {
    output(
        r#"
use std::generator::{Generator, Step}
use std::channel::{SendError, ReceiveError}
const ready: Step<i64, string> = Step::Yielded(4)
const empty = ReceiveError::Empty
const full: SendError<string> = SendError::Full("preserved")
std::io::print(match ready { Step::Yielded(n) => n, Step::Complete(_) => 0 })
std::io::print(match empty { ReceiveError::Empty => "empty", ReceiveError::Closed => "closed" })
std::io::print(match full { SendError::Full(value) | SendError::Closed(value) | SendError::Limit(value) => value })
let sequence = gen proc() -> Generator<i64, (), string> { yield 7; return "finished" }
let mut values = sequence()
let mut first = values.start()
if let Step::Yielded(number) = &mut first { number.* += 1 }
std::io::print(first.yielded())
std::io::print(match values.resume() { Step::Yielded(_) => "unexpected", Step::Complete(text) => text })
let error = SendError::Full(rc "second")
let unsent = match error { SendError::Full(value) | SendError::Closed(value) | SendError::Limit(value) => value }
std::io::print(unsent.*, ":", unsent.strong_count())
std::mem::drop(unsent)
let endpoints = std::channel::bounded<string>(1 as usize)
endpoints.0.send("first").unwrap()
let failed = endpoints.0.try_send("recover").unwrap_err()
std::io::print(match failed { SendError::Full(value) | SendError::Closed(value) | SendError::Limit(value) => value })
std::mem::drop(endpoints.1.recv().unwrap())
std::io::print(match endpoints.1.try_recv().unwrap_err() { ReceiveError::Empty => true, ReceiveError::Closed => false })
endpoints.0.close()
std::io::print(match endpoints.1.recv().unwrap_err() { ReceiveError::Closed => true, ReceiveError::Empty => false })
"#,
        "4\nempty\npreserved\n8\nfinished\nsecond:1\nrecover\ntrue\ntrue\n",
    );
}
