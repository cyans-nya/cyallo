use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn records_tuples_and_explicit_clones() {
    output(
        "record Point { x: i64, y: string }\nrecord Row { point: Point, data: (i64, string) }\nfn make() -> Row { return Row { data: (4, \"a\"), point: Point { y: \"b\", x: 2 } } }\nlet mut a = make()\nlet b = std::mem::clone(&a)\na.point.x += 3\na.data.0 = 9\nlet rows = [std::mem::clone(&b), a]\nstd::io::print(b.point.x, b.data.0, rows[1].point.x, make().data.1, (1,).0)\n",
        "245a1\n",
    );
}
#[test]
fn invalid_composites_and_lifetimes() {
    for source in [
        "record P { x: i64, x: i64 }\n",
        "record P { x: Missing }\n",
        "record P { x: P }\n",
        "record P { x: i64 }\nlet p = P {}\n",
        "record P { x: i64 }\nlet p = P { x: 1, x: 2 }\n",
        "record P { x: i64 }\nlet p = P { x: \"bad\" }\n",
        "let p = (1, 2)\nstd::io::print(p.2)\n",
        "let mut p = (1, 2)\np.1 = true\n",
        "let f = fn(n: i64) -> i64 { return n }\nf(\"bad\")\n",
        "fn bad() -> proc() { let mut n = 0; return proc[&mut n]() { n += 1 } }\n",
        "fn bad() -> (proc(), i64) { let mut n = 0; return (proc[&mut n]() { n += 1 }, 0) }\n",
        "let mut sink = proc() {}\nproc bad() { let mut n = 0; sink = proc[&mut n]() { n += 1 } }\n",
        "proc bad(f: &mut proc()) -> proc() { return f }\n",
        "fn bad(f: proc()) -> i64 { f(); return 1 }\n",
        "const n = 0\nlet f = proc[&mut n]() { n += 1 }\n",
        "let mut n = 0\nlet f = fn[&mut n]() -> i64 { n += 1; return n }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn recursive_record_layouts_require_indirection_on_every_cycle() {
    for source in [
        "record Node { next: Node }",
        "record Node { next: Cell<Node> }",
        "record Node { next: [1]Node }",
        "record First { next: gc Second }\nrecord Second { next: Second }",
        "record Node<T> { value: T, next: Node<T> }\nlet root: Node<i64> = 0",
    ] {
        let errors = compile(source).unwrap_err();
        assert!(
            errors
                .message
                .contains("recursive value types require indirection"),
            "{source}: {errors:?}"
        );
    }
    for source in [
        "record Node { next: Vec<Node> }",
        "record Node { next: VecDeque<Node> }",
        "record Node { next: Cell<Option<std::rc::Weak<Node>>> }",
        "record First { next: gc Second }\nrecord Second { next: First }",
        "record Node<T> { value: T, next: Vec<gc Node<T>> }\nproc read(node: &Node<i64>) -> i64 { return node.value }",
    ] {
        compile(source).unwrap_or_else(|errors| panic!("{source}: {errors:?}"));
    }
}

#[test]
fn structural_records_canonicalize_fields_and_preserve_evaluation_order() {
    output(
        r#"
type Row = record { x: i64, y: string }
fn make() -> Row { return record { y: "saved", x: 2 } }
proc next(n: &mut i64) -> i64 { n.* += 1; return n.* }
let mut count = 0
let mut ordered = record { z: next(&mut count), a: next(&mut count) }
let equivalent: record { a: i64, z: i64 } = ordered
let mut row = make()
row.x += equivalent.a
let cloned = std::mem::clone(&row)
row.x = 99
std::io::print(equivalent.z, equivalent.a, ":", cloned.x, cloned.y, make().y)
"#,
        "12:4savedsaved\n",
    );
}

#[test]
fn structural_records_reject_nominal_coercions_and_incompatible_fields() {
    for source in [
        "record P { x: i64 }\nlet value: P = record { x: 1 }\n",
        "record P { x: i64 }\nlet value: record { x: i64 } = P { x: 1 }\n",
        "let value: record { x: i64 } = record { x: true }\n",
        "let value: record { x: i64 } = record { x: 1, y: 2 }\n",
        "fn bad() -> record { f: proc() } { let mut x = 0; return record { f: proc[&mut x]() { x += 1 } } }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn borrowing_temporary_structural_records_checks_their_owned_fields() {
    output(
        "let value = &record { x: 3 }\nstd::io::print(value.*.x)\n",
        "3\n",
    );
    assert!(compile("let owner = rc 3\nlet borrowed = &record { owner: owner }\nstd::io::print(owner.*, borrowed.*.owner.*)\n").is_err());
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
