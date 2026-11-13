use cyallo::backend::{CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend};
use cyallo::{Context, Session, Status, compile, compile_for_execution};
use std::sync::Arc;

#[test]
fn evaluates_pure_calls_and_substitutes_scalars() {
    let source = "fn square(n: i64) -> i64 { return n * n }\nconst value = square(12) + 1\nfn get() -> i64 { const extra = square(2); return value + extra }\nstd::io::print(value, \":\", get())\n";
    for deferred in [false, true] {
        let p = Arc::new(
            if deferred {
                compile_for_execution(source, Default::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        assert!(
            !p.functions()[0]
                .code
                .iter()
                .any(|i| matches!(i.op, cyallo::ir::Op::Call(_, 1, _)))
        );
        for engine in [
            Box::new(CraneliftBackend.compile(p.clone()).unwrap()) as Box<dyn Executable>,
            Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
        ] {
            let mut s = Session::new(p.clone()).unwrap();
            engine.run(&mut s, 10000).unwrap();
            assert_eq!(s.status(), &Status::Finished);
            assert_eq!(s.context.output, "145:149\n");
            assert_eq!(s.function_call_count(1), 0);
        }
    }
}
#[test]
fn frozen_aggregates_make_independent_mutable_copies() {
    let source = "record Pair<T> { a: T, b: std::collections::Vec<i64> }\nfn make() -> std::collections::Vec<i64> { let mut a = Vec[1,2]; a[1] += 1; return a }\nconst items = make()\nconst pair = Pair<i64> { a: 7, b: items }\nlet mut copy = pair\ncopy.b[0] = 99\nlet i = 1\nstd::io::print(items[i], \":\", pair.b[0], \":\", copy.b[0])\n";
    let p = Arc::new(compile(source).unwrap());
    for gc in [Default::default(), cyallo::runtime::gc::GcConfig::stress()] {
        let mut s = Session::with_context(
            p.clone(),
            Context {
                gc,
                ..Default::default()
            },
        )
        .unwrap();
        CraneliftSpeedBackend
            .compile(p.clone())
            .unwrap()
            .run(&mut s, 10000)
            .unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "3:1:99\n");
    }
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
        "fn bad(n: Cell<i64>) -> i64 { return *n }\n",
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
        "const n = 1 div 0\n",
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
    assert!(compile_for_execution(source, Default::default()).is_err());
}
#[test]
fn aggregate_payloads_share_a_pool_and_preserve_fixed_shape() {
    let p = Arc::new(compile("fn array() -> [6]i64 { return [0,0,0,0,0,42] }\nconst a = array()\nlet b = a\nlet c = a\nstd::io::print(b[5], c[5], std::array::len(a))\n").unwrap());
    let values: Vec<_> = p.functions()[0]
        .code
        .iter()
        .filter_map(|i| {
            if let cyallo::ir::Op::Frozen(_, value) = &i.op {
                Some(value)
            } else {
                None
            }
        })
        .collect();
    assert!(values.len() >= 4);
    assert!(values.iter().all(|v| Arc::ptr_eq(values[0], v)));
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "42426\n");
}
#[test]
fn compile_time_strings_work_as_service_labels_and_plan_fields() {
    let source = "trait T { proc f(self: &mut Self) }\nconst label = \"service\"\nconst KEY = std::service::ServiceKey::new<dyn T>(label)\nconst FIELD = \"value\"\nrecord R { value: Cell<i64> }\nfn route(r: @gc R, key: i64) -> std::lookup::StoragePath<i64> { return std::lookup::path(r).target().field(FIELD).target() }\nlet r = @gc R { value: std::cell::Cell::new(42) }\nstd::io::print(std::lookup::mapped(r, 0, route).unwrap())\n";
    let p = Arc::new(compile(source).unwrap());
    assert_eq!(p.services().keys.len(), 1);
    assert_eq!(p.services().keys[0].label.as_ref(), "service");
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "42\n");
}
#[test]
fn stateless_procedure_values_do_not_execute_during_initialization() {
    let p = Arc::new(
        compile("proc greet() { std::io::print(42) }\nconst callback = greet\ncallback()\n")
            .unwrap(),
    );
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.context.output, "42\n");
    assert_eq!(s.function_call_count(1), 1);
}
#[test]
fn frozen_routine_references_preserve_identity_under_local_shadowing() {
    let source = "fn first(n: i64) -> i64 { return n + 1 }\nfn second(n: i64) -> i64 { return n + 2 }\nconst callback = first\nfn apply(n: i64) -> i64 { let first = second; return callback(n) }\nstd::io::print(apply(10))\n";
    let p = Arc::new(compile(source).unwrap());
    let mut s = Session::new(p.clone()).unwrap();
    CraneliftSpeedBackend
        .compile(p)
        .unwrap()
        .run(&mut s, 1000)
        .unwrap();
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "11\n");
    assert_eq!(s.function_call_count(2), 0);
}
