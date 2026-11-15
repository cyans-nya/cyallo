use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::runtime::{LookupError, RuntimeError};
use cyallo::{Context, Session, Status, compile, runtime::gc::GcConfig};
use std::sync::Arc;
const BAG: &str = r#"
record Bag { route: Cell<i64>, values: Cell<Vec<gc Cell<i64>>> }
proc locate(bag: gc Bag, key: i64) -> std::lookup::StoragePath<gc Cell<i64>> {
    let index = bag.route.*
    if key != 7 || index < 0 { return std::lookup::missing<gc Cell<i64>>() }
    return std::lookup::path(bag).target().field("values").target().element(index)
}
proc fetch(bag: gc Bag) -> i64 { return std::lookup::mapped(bag, 7, locate).unwrap().*.* }
"#;
fn engines(p: &Arc<cyallo::ir::Program>) -> Vec<Box<dyn Executable>> {
    let baseline = TieredBackend::default().compile(p.clone()).unwrap();
    for fi in 0..p.functions().len() {
        baseline.precompile(fi, Tier::Winch).unwrap()
    }
    assert!(baseline.wait_for_compilation(std::time::Duration::from_secs(30)));
    vec![
        Box::new(baseline),
        Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
        #[cfg(feature = "wasm-adapter")]
        Box::new(cyallo::backend::WasmBackend.compile(p.clone()).unwrap()),
    ]
}
fn parity(source: &str, expected: &str) -> Arc<cyallo::ir::Program> {
    for deferred in [false, true] {
        let p = Arc::new(
            if deferred {
                cyallo::compile_for_execution(source, Default::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let mut reference = Session::new(p.clone()).unwrap();
        reference.run(1_000_000);
        assert_eq!(reference.status(), &Status::Finished);
        assert_eq!(reference.context.output, expected);
        for gc in [GcConfig::default(), GcConfig::stress()] {
            for engine in engines(&p) {
                let mut s = Session::with_context(
                    p.clone(),
                    Context {
                        gc,
                        ..Default::default()
                    },
                )
                .unwrap();
                for i in 0..1_000_000 {
                    if i % 3 == 0 {
                        s.run(1);
                    } else {
                        engine.run(&mut s, 1).unwrap();
                    }
                    if s.status() != &Status::Paused {
                        break;
                    }
                }
                assert_eq!(s.status(), reference.status());
                assert_eq!(s.context.output, expected);
                assert_eq!(s.instruction_count(), reference.instruction_count());
                assert_eq!(s.lookup_stats().hits, reference.lookup_stats().hits);
                assert_eq!(s.lookup_stats().misses, reference.lookup_stats().misses);
            }
        }
    }
    Arc::new(compile(source).unwrap())
}
#[test]
fn ordinary_service_library_keeps_paths_during_replacement() {
    use cyallo::modules::{SourceModule, compile_modules};
    let sources = [
        SourceModule::new(
            "registry.ya",
            include_str!("../examples/lookup-services/registry.ya"),
        ),
        SourceModule::new(
            "logging.ya",
            include_str!("../examples/lookup-services/logging.ya"),
        ),
        SourceModule::new(
            "console.ya",
            include_str!("../examples/lookup-services/console.ya"),
        ),
        SourceModule::new(
            "main.ya",
            include_str!("../examples/lookup-services/main.ya"),
        ),
    ];
    let p = Arc::new(compile_modules(3, &sources, Default::default(), false).unwrap());
    // User service code lowers to generic lookup and ordinary calls, never service intrinsics.
    assert!(
        p.functions()
            .iter()
            .flat_map(|f| &f.code)
            .all(|i| !matches!(
                i.op,
                cyallo::ir::Op::Service(..) | cyallo::ir::Op::ServiceKey(..)
            ))
    );
    for engine in engines(&p) {
        let mut s = Session::with_context(
            p.clone(),
            Context {
                gc: GcConfig::stress(),
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..100000 {
            engine.run(&mut s, 1).unwrap();
            if s.status() != &Status::Paused {
                break;
            }
        }
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(
            s.context.output,
            "first: ready\nsecond: replacement\nsecond: cached lookup\nfalse\n"
        );
        assert_eq!(s.lookup_stats().mapper_calls, 1);
        assert_eq!(s.lookup_stats().hits, 2);
        assert_eq!(s.lookup_stats().invalidations, 0);
    }
}

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
    let p = parity(&source, "12\n12\n34\n99\n");
    let engine = CraneliftSpeedBackend.compile(p.clone()).unwrap();
    let mut s = Session::new(p).unwrap();
    engine.run(&mut s, 100000).unwrap();
    assert_eq!(s.lookup_stats().mapper_calls, 2);
    assert_eq!(s.lookup_stats().hits, 2);
    assert_eq!(s.lookup_stats().invalidations, 1);
    assert_eq!(s.lookup_stats().native_hits, 2);
}
#[test]
fn negative_paths_and_receiver_identity() {
    let source = format!(
        r#"{BAG}
proc optional(bag: gc Bag) -> bool {{ return std::lookup::mapped(bag, 7, locate).is_some() }}
let values = Vec[gc Cell::new(12)]
let one = gc Bag {{ route: Cell::new(-1), values: Cell::new(std::mem::clone(&values)) }}
let two = gc Bag {{ route: Cell::new(0), values: Cell::new(values) }}
std::io::print(optional(one))
std::io::print(optional(one))
std::io::print(optional(two))
one.route.* = 0
std::io::print(optional(one))
"#
    );
    let p = parity(&source, "false\nfalse\ntrue\ntrue\n");
    let mut s = Session::new(p).unwrap();
    s.run(100000);
    assert_eq!(s.lookup_stats().mapper_calls, 3);
    assert_eq!(s.lookup_stats().hits, 1);
}
#[test]
fn arbitrary_mappers_captures_and_nested_dependencies() {
    parity(
        r#"
record State { left: Cell<i64>, right: Cell<i64> }
fn select(state: gc State, left: bool) -> std::lookup::StoragePath<i64> {
    if left { return std::lookup::path(state).target().field("left").target() }
    return std::lookup::path(state).target().field("right").target()
}
proc outer(state: gc State, key: bool) -> std::lookup::StoragePath<i64> {
    let selected = std::lookup::mapped(state, key, select).unwrap()
    return select(state, selected > 0)
}
proc query(state: gc State) -> i64 { return std::lookup::mapped(state, true, outer).unwrap() }
let state = gc State { left: Cell::new(1), right: Cell::new(20) }
std::io::print(query(state))
std::io::print(query(state))
state.left.* = -1
std::io::print(query(state))
let offset = false
let mapper = proc(s: gc State, key: bool) -> std::lookup::StoragePath<i64> { return select(s, offset) }
std::io::print(std::lookup::mapped(state, true, mapper).unwrap())
"#,
        "1\n1\n20\n20\n",
    );
}
#[test]
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
#[test]
fn deferred_mapper_errors_keep_the_original_diagnostic() {
    let source = "fn bad(s: gc Cell<i64>, key: i64) -> std::lookup::StoragePath<i64> { std::io::print(key); return std::lookup::path(s).target().target() }\nlet result = std::lookup::mapped(gc Cell::new(1), 0, bad)\n";
    let expected = compile(source).unwrap_err();
    let p = Arc::new(cyallo::compile_for_execution(source, Default::default()).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    let Status::Failed(failure) = s.status() else {
        panic!("expected a deferred mapper failure")
    };
    assert_eq!(failure.error, RuntimeError::DeferredCheck(expected.clone()));
    assert_eq!(failure.diagnostic(), expected);
    assert!(s.context.output.is_empty());
}
#[test]
fn generic_records_routines_and_closure_specializations() {
    parity(
        r#"
record Pair<A,B> { first: A, second: B }
fn make_pair<A,B>(a: A, b: B) -> Pair<A,B> { return Pair<A,B> { first: a, second: b } }
fn make<T>(value: T) -> fn() -> T { return fn() -> T { return std::mem::clone(&value) } }
let a = make(42)
let b = make("hello")
let p = make_pair(a(), b())
std::io::print(p.first, ":", p.second)
"#,
        "42:hello\n",
    );
}
#[test]
fn snapshots_guard_aliased_child_cells() {
    parity(
        r#"
record Entry { n: Cell<i64> }
record Table { values: Vec<gc Entry> }
proc route(table: gc Table, key: i64) -> std::lookup::StoragePath<i64> {
    let index = table.values[0].n.*
    return std::lookup::path(table).target().field("values").element(index).target().field("n").target()
}
proc query(table: gc Table) -> i64 { return std::lookup::mapped(table, 0, route).unwrap() }
let first = gc Entry { n: Cell::new(0) }
let second = gc Entry { n: Cell::new(9) }
let table = gc Table { values: Vec[first, second] }
std::io::print(query(table))
std::io::print(query(table))
first.n.* = 1
std::io::print(query(table))
"#,
        "0\n0\n9\n",
    );
}
#[test]
fn erased_readonly_methods_guard_shared_cell_writes() {
    parity(
        r#"
trait Selector { proc index(self: &Self) -> i64; proc advance(self: &Self) }
record Counter { n: gc Cell<i64> }
impl Selector for Counter {
    proc index(self: &Self) -> i64 { return self.n.*.* }
    proc advance(self: &Self) { self.n.*.* = self.n.*.* + 1 }
}
record State { selector: dyn Selector, values: Vec<i64> }
proc route(state: gc State, key: i64) -> std::lookup::StoragePath<i64> {
    return std::lookup::path(state).target().field("values").element(state.selector.index())
}
proc query(state: gc State) -> i64 { return std::lookup::mapped(state, 0, route).unwrap() }
let selector = Counter { n: gc Cell::new(0) } as dyn Selector
let state = gc State { selector: std::mem::clone(&selector), values: Vec[10,20] }
std::io::print(query(state))
std::io::print(query(state))
selector.advance()
std::io::print(query(state))
"#,
        "10\n10\n20\n",
    );
}
#[test]
fn bounded_cases_do_not_grow_for_megamorphic_sites() {
    let source = r#"
proc route(values: gc Cell<Vec<i64>>, key: i64) -> std::lookup::StoragePath<i64> { return std::lookup::path(values).target().target().element(key) }
proc query(values: gc Cell<Vec<i64>>, key: i64) -> i64 { return std::lookup::mapped(values, key, route).unwrap() }
let values = gc Cell::new(Vec::from(0..6))
for i in 0..=5 { std::io::print(query(values, i)) }
for i in 0..=5 { std::io::print(query(values, i)) }
"#;
    let p = parity(source, "0\n1\n2\n3\n4\n5\n0\n1\n2\n3\n4\n5\n");
    let mut s = Session::new(p).unwrap();
    s.run(100000);
    assert_eq!(s.lookup_stats().max_cases, 4);
    assert_eq!(s.lookup_stats().mapper_calls, 8);
    assert_eq!(s.lookup_stats().hits, 4);
}
#[test]
fn repl_abandons_failed_mappers_and_relinks_erased_closures() {
    let mut repl = cyallo::repl::Repl::new(Default::default()).unwrap();
    for source in [
        "fn broken(s: gc Cell<i64>, key: i64) -> std::lookup::StoragePath<i64> { let n = 1 / key; return std::lookup::path(s).target().target() }",
        "let value = gc Cell::new(10)",
        "let f = std::option::some(std::any::erase(fn() -> i64 { return 42 }))",
        "let result = std::lookup::mapped(value, 0, broken)",
    ] {
        repl.push_line(source).unwrap();
        repl.run(10000);
    }
    assert!(matches!(repl.session().status(), Status::Failed(_)));
    repl.push_line("fn okay(s: gc Cell<i64>, key: i64) -> std::lookup::StoragePath<i64> { return std::lookup::path(s).target().target() }").unwrap();
    repl.run(10000);
    repl.push_line("std::io::print(std::lookup::mapped(value, 0, okay).unwrap())")
        .unwrap();
    repl.run(10000);
    repl.push_line("let restored = std::any::checked<fn() -> i64>(f.unwrap())")
        .unwrap();
    repl.run(10000);
    repl.push_line("std::io::print(restored())").unwrap();
    repl.run(10000);
    assert_eq!(repl.session().status(), &Status::Finished);
    assert!(repl.session().context.output.ends_with("10\n42\n"));
}
#[test]
fn directory_snapshots_need_one_warm_version_guard() {
    let source = r#"
record State { directory: Cell<gc Vec<i64>>, values: Vec<i64> }
proc route(state: gc State, key: i64) -> std::lookup::StoragePath<i64> {
    let directory = state.directory.*
    if directory[key] != key { return std::lookup::missing<i64>() }
    return std::lookup::path(state).target().field("values").element(0)
}
proc query(state: gc State) -> i64 { return std::lookup::mapped(state, 200, route).unwrap() }
let state = gc State { directory: Cell::new(gc Vec::from(0..256)), values: Vec[42] }
std::io::print(query(state))
std::io::print(query(state))
"#;
    let p = Arc::new(compile(source).unwrap());
    for engine in engines(&p) {
        let mut s = Session::new(p.clone()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "42\n42\n");
        assert_eq!(s.lookup_stats().mapper_calls, 1);
        assert_eq!(s.lookup_stats().guard_checks, 1);
    }
}
#[test]
fn retained_field_location_outlives_its_parent_binding() {
    parity(
        r#"
record Item { n: i64 }
proc make() -> std::lookup::Location<i64> {
    let root = gc Item { n: 1 }
    return std::lookup::field(&root, "n")
}
let location = make()
let allocations = gc Cell::new(10)
location.store(2)
std::io::print(location.load())
"#,
        "2\n",
    );
}
