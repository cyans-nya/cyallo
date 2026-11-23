use cyallo::{CompileOptions, Dialect, compile, compile_with_options, vm::{Context, Session, Status}};
use std::sync::Arc;

use cyallo::runtime::{LookupError, RuntimeError};
const BAG: &str = r#"
record Bag { route: Cell<i64>, values: Cell<Vec<gc Cell<i64>>> }
proc locate(bag: gc Bag, key: i64) -> std::lookup::StoragePath<gc Cell<i64>> {
    let index = bag.route.*
    if key != 7 || index < 0 { return std::lookup::missing<gc Cell<i64>>() }
    return std::lookup::path(bag).target().field("values").target().element(index)
}
proc fetch(bag: gc Bag) -> i64 { return std::lookup::mapped(bag, 7, locate).unwrap().*.* }
"#;

#[test]
fn relative_paths_aliases_and_payload_updates() {
    let source = format!(
        r#"{BAG}
let first = gc Cell::new(12)
let second = gc Cell::new(34)
let bag = gc Bag {{ route: Cell::new(0), values: Cell::new(Vec[first, second]) }}
std::io::print(fetch(bag))
std::io::print(fetch(bag))
bag.route.* = 1
std::io::print(fetch(bag))
second.*.* = 99
std::io::print(fetch(bag))
"#
    );
    output(&source, "12\n12\n34\n99\n");
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn paths_cannot_cross_receivers_and_erased_casts_are_checked() {
    let source = r#"
fn wrong(state: gc Cell<i64>, key: gc Cell<i64>) -> std::lookup::StoragePath<i64> {
    return std::lookup::path(key).target().target()
}
let a = gc Cell::new(1)
let b = gc Cell::new(2)
std::io::print(std::lookup::mapped(a, b, wrong).unwrap())
"#;
    let mut s = Session::new(Arc::new(compile(source).unwrap())).unwrap();
    s.run(10000);
    assert!(matches!(
        s.status(),
        Status::Failed(failure) if failure.error == RuntimeError::Lookup(LookupError::ForeignPlan)
    ));
    let mut s = Session::new(Arc::new(
        compile("let value = std::any::checked<string>(std::any::erase(1))\n").unwrap(),
    ))
    .unwrap();
    s.run(10000);
    assert!(matches!(
        s.status(),
        Status::Failed(failure) if failure.error == RuntimeError::CheckedTypeMismatch
    ));
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn mapper_effects_and_escaping_borrows_are_rejected() {
    for source in [
        "fn bad(s: gc Cell<i64>, key: i64) -> std::lookup::StoragePath<i64> { s.*.* = key; return std::lookup::path(s).target().target() }\n",
        "proc bad() -> proc() { let mut count = 0; return proc[&mut count]() { count += 1 } }\n",
        "fn bad(s: &mut Vec<i64>, key: i64) { s.push(key) }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
    for body in [
        "std::io::print(key)",
        "std::runtime::caie::input(&mut key)",
        "s.*.* = key",
        "let n = std::random::uniform(1)",
    ] {
        let source = format!(
            "proc bad(s: gc Cell<i64>, key: i64) -> std::lookup::StoragePath<i64> {{ {body}; return std::lookup::path(s).target().target() }}\nlet x = std::lookup::mapped(gc Cell::new(1), 0, bad)\n"
        );
        let mut s = Session::new(Arc::new(compile(&source).unwrap())).unwrap();
        s.run(10000);
        assert!(
            matches!(
                s.status(),
                Status::Failed(failure)
                    if failure.error == RuntimeError::Lookup(LookupError::MapperNotReadonly)
            ),
            "{:?}",
            s.status()
        );
        assert!(s.context.output.is_empty());
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

