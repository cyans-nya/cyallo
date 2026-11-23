use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

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

#[test]
fn evaluates_pure_calls_and_substitutes_scalars() {
    output("fn square(n: i64) -> i64 { return n * n }\nconst value = square(12) + 1\nfn get() -> i64 { const extra = square(2); return value + extra }\nstd::io::print(value, \":\", get())\n", "145:149\n");
}

#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn frozen_aggregates_make_independent_mutable_copies() {
    output("record Pair<T> { a: T, b: std::collections::Vec<i64> }\nfn make() -> std::collections::Vec<i64> { let mut a = Vec[1,2]; a[1] += 1; return a }\nconst items = make()\nconst pair = Pair<i64> { a: 7, b: items }\nlet mut copy = pair\ncopy.b[0] = 99\nlet i = 1\nstd::io::print(items[i], \":\", pair.b[0], \":\", copy.b[0])\n", "3:1:99\n");
}

#[test]
fn rejects_runtime_dependencies_effects_and_mutable_identity() {
    for source in [
        "const n = std::random::uniform(1)\n",
        "proc p() -> i64 { return 1 }\nconst n = p()\n",
        "let n = 4\nconst k = n + 1\n",
        "const n = std::cell::Cell::new(1)\n",
        "record R { n: Cell<i64> }\nconst r = R { n: std::cell::Cell::new(1) }\n",
        "fn f(n: i64) -> i64 { const k = n + 1; return k }\n",
        "const n = 1\nfn f(n: i64) -> i64 { const k = n; return k }\n",
        "fn bad(n: Cell<i64>) -> i64 { return n.* }\n",
        "fn bad(n: &mut Vec<i64>) -> i64 { n.push(1); return n[0] }\n",
        "const a = [1]\na[0] = 2\n",
        "const p = (1, [2])\np.1[0] = 3\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn failures_and_evaluator_limits_are_compile_time_diagnostics() {
    for source in [
        "const n = 9223372036854775807 + 1\n",
        "const n = 1 / 0\n",
        "fn forever() -> i64 { return forever() }\nconst n = forever()\n",
        "fn forever() -> i64 { while true {}; return 1 }\nconst n = forever()\n",
        "const n = std::collections::Vec::from(0..100000)\n",
        "fn grow() -> string { let mut s = \"x\"; for i in 1..=100 { s = s + s }; return s }\nconst s = grow()\n",
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, "E104", "{error}");
    }
}

#[test]
fn deferred_constants_validate_unexecuted_static_callees() {
    let source = "fn bad() -> i64 { std::io::print(1); return 1 }\nfn choose() -> i64 { if false { return bad() }; return 2 }\nconst n = choose()\n";
    assert!(compile(source).is_err());
}

#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn fixed_shape_array_constants_are_copied_per_use() {
    output("fn array() -> [6]i64 { return [0,0,0,0,0,42] }\nconst a = array()\nlet b = a\nlet c = a\nstd::io::print(b[5], c[5], std::array::len(a))\n", "42426\n");
}

#[test]
fn stateless_procedure_values_do_not_execute_during_initialization() {
    output("proc greet() { std::io::print(42) }\nconst callback = greet\ncallback()\n", "42\n");
}

#[test]
fn frozen_routine_references_preserve_identity_under_local_shadowing() {
    output("fn first(n: i64) -> i64 { return n + 1 }\nfn second(n: i64) -> i64 { return n + 2 }\nconst callback = first\nfn apply(n: i64) -> i64 { let first = second; return callback(n) }\nstd::io::print(apply(10))\n", "11\n");
}
