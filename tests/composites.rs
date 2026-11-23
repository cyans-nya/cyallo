use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Session, Status, compile, compile_for_execution};
use std::{sync::Arc, time::Duration};
fn output(source: &str, expected: &str) {
    for deferred in [false, true] {
        let p = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let mut interpreter = Session::new(p.clone()).unwrap();
        interpreter.run(100_000);
        assert_eq!(interpreter.status(), &Status::Finished);
        assert_eq!(interpreter.context.output, expected);
        let baseline = TieredBackend::default().compile(p.clone()).unwrap();
        for i in 0..p.functions().len() {
            baseline.precompile(i, Tier::Winch).unwrap();
        }
        assert!(baseline.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Box<dyn Executable>> = vec![
            Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
            Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
            Box::new(baseline),
        ];
        for engine in engines {
            let mut session = Session::new(p.clone()).unwrap();
            engine.run(&mut session, 100_000).unwrap();
            assert_eq!(session.status(), interpreter.status());
            assert_eq!(session.context.output, expected);
            assert_eq!(session.instruction_count(), interpreter.instruction_count());
        }
    }
}
#[test]
fn composites_example() {
    output(include_str!("../examples/composites.yas"), "1:7\n2\n");
}
#[test]
fn named_function_values_and_higher_order_results() {
    output(
        "fn double(n: i64) -> i64 { return n * 2 }\nfn choose() -> fn(i64) -> i64 { return double }\nlet f = double\nlet fs = [f, choose()]\nstd::io::print(f(3), fs[1](4), choose()(5), (double)(6))\n",
        "681012\n",
    );
}
#[test]
fn records_tuples_and_explicit_clones() {
    output(
        "record Point { x: i64, y: string }\nrecord Row { point: Point, data: (i64, string) }\nfn make() -> Row { return Row { data: (4, \"a\"), point: Point { y: \"b\", x: 2 } } }\nlet mut a = make()\nlet b = std::mem::clone(&a)\na.point.x += 3\na.data.0 = 9\nlet rows = [std::mem::clone(&b), a]\nstd::io::print(b.point.x, b.data.0, rows[1].point.x, make().data.1, (1,).0)\n",
        "245a1\n",
    );
}
#[test]
fn escaping_value_captures_and_nested_closures() {
    output(
        "fn make(n: i64) -> fn(i64) -> fn(i64) -> i64 { return fn(x: i64) -> fn(i64) -> i64 { return fn(y: i64) -> i64 { return n + x + y } } }\nlet f = make(3)(4)\nstd::io::print(f(5))\nlet mut x = 10\nlet snapshot = fn() -> i64 { return x }\nx = 20\nstd::io::print(snapshot())\n",
        "12\n10\n",
    );
}
#[test]
fn scoped_ref_captures_and_borrowed_callbacks() {
    output(
        "proc twice(f: &mut proc()) { f(); f() }\nproc work() -> i64 {\n let mut n = 0\n let mut inc = proc[&mut n]() { n += 1 }\n twice(&mut inc)\n let callbacks = (inc,)\n callbacks.0()\n return n\n}\nstd::io::print(work())\n",
        "3\n",
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
fn captures_survive_gc_and_instruction_handoffs() {
    let source = r#"
record Item { pair: (i64, string), data: std::collections::Vec<i64> }
fn make(n: i64) -> fn(i64) -> i64 {
    let item = Item { pair: (n, "saved"), data: std::collections::Vec[n, n + 1] }
    return fn(x: i64) -> i64 { return item.pair.0 + item.data[1] + x }
}
proc run() -> i64 {
    let mut count = 0
    let bump = proc[&mut count]() { let garbage = [1,2,3]; count += garbage[0] }
    let functions = (make(4), bump)
    for i in 1..=12 { functions.1() }
    return functions.0(count)
}
std::io::print(run())
"#;
    let p = Arc::new(compile(source).unwrap());
    let baseline = TieredBackend::default().compile(p.clone()).unwrap();
    for i in 0..p.functions().len() {
        baseline.precompile(i, Tier::Winch).unwrap();
    }
    assert!(baseline.wait_for_compilation(Duration::from_secs(30)));
    let engines: Vec<Box<dyn Executable>> = vec![
        Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
        Box::new(baseline),
    ];
    for gc in [
        cyallo::runtime::gc::GcConfig::default(),
        cyallo::runtime::gc::GcConfig::stress(),
    ] {
        for engine in &engines {
            let mut s = Session::with_context(
                p.clone(),
                cyallo::Context {
                    gc,
                    max_cells: 256,
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..10_000 {
                if step % 2 == 0 {
                    engine.run(&mut s, 1).unwrap();
                } else {
                    s.run(1);
                }
                if s.status() != &Status::Paused {
                    break;
                }
            }
            assert_eq!(s.status(), &Status::Finished);
            assert_eq!(s.context.output, "21\n");
            if gc.enabled {
                assert!(s.gc_stats().collections > 10);
            }
        }
    }
}
#[test]
fn repl_preserves_functions_when_declaration_ids_move() {
    let mut repl = cyallo::repl::Repl::new(Default::default()).unwrap();
    for source in [
        "fn make(n: i64) -> fn() -> i64 { return fn() -> i64 { return n } }",
        "let saved = make(7)",
        "let mut n = 0; let increment = proc[&mut n]() { n += 1 }",
        "fn another() -> i64 { return 999 }",
        "let unrelated = fn() -> i64 { return 456 }",
        "increment(); std::io::print(saved(), n, another(), unrelated())",
    ] {
        assert_eq!(
            repl.push_line(source).unwrap(),
            cyallo::repl::Submission::Ready
        );
        repl.run(10000);
        assert_eq!(repl.session().status(), &Status::Finished);
    }
    assert_eq!(repl.session().context.output, "71999456\n");
}
#[test]
fn indirect_deferred_errors_are_reported_before_effects() {
    let p = Arc::new(compile_for_execution("proc bad() -> i64 { std::io::print(\"must not run\"); return missing }\nlet f = bad\nstd::io::print(f())\n",Default::default()).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(10000);
    assert!(matches!(s.status(), Status::Failed(_)));
    assert_eq!(s.context.output, "");
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
