use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use std::{sync::Arc, time::Duration};

fn parity(source: &str, expected: &str) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let winch = TieredBackend::default().compile(program.clone()).unwrap();
        for function in 0..program.functions().len() {
            winch.precompile(function, Tier::Winch).unwrap();
        }
        assert!(winch.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(winch)),
            Some(Box::new(CraneliftBackend.compile(program.clone()).unwrap())),
            Some(Box::new(
                CraneliftSpeedBackend.compile(program.clone()).unwrap(),
            )),
        ];
        for engine in engines {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..100_000 {
                if let Some(engine) = &engine
                    && step % 3 != 0
                {
                    engine.run(&mut session, 1).unwrap();
                } else {
                    session.run(1);
                }
                if session.status() != &Status::Paused {
                    break;
                }
            }
            assert_eq!(session.status(), &Status::Finished);
            assert_eq!(session.context.output, expected);
        }
    }
}

#[test]
fn copy_reads_and_replacement_work_through_shared_cells() {
    parity(
        r#"
proc increment(cell: &Cell<i64>) { cell.*.* = cell.*.* + 1 }
record Node { value: Cell<i64> }
let cell = Cell::new(1)
let snapshot = cell.*
increment(&cell)
cell.* = cell.* + 1
let node = gc Node { value: Cell::new(10) }
node.value.* = node.value.* + 1
let shared = gc Cell::new(20)
shared.*.* = shared.*.* + 1
let counted = rc Cell::new(30)
counted.*.* = counted.*.* + 1
std::io::print(snapshot, ":", cell.*, ":", node.value.*, ":", shared.*.*, ":", counted.*.*)
std::mem::drop(counted)
"#,
        "1:3:11:21:31\n",
    );
}

#[test]
fn replace_transfers_noncopy_contents_and_assignment_drops_old_owners() {
    parity(
        r#"
let first = rc 1
let first_weak = first.downgrade()
let cell = Cell::new(first)
let second = rc 2
let second_weak = second.downgrade()
let old = cell.replace(second)
std::io::print(old.*, ":", old.strong_count())
std::mem::drop(old)
std::io::print(first_weak.upgrade().is_none())
cell.* = rc 3
std::io::print(second_weak.upgrade().is_none())
let optional = Cell::new(std::option::some(Vec[4,5]))
let values = optional.replace(std::option::none<Vec<i64>>()).unwrap()
std::io::print(values[0], ":", values[1])
std::mem::drop(cell)
std::mem::drop(first_weak)
std::mem::drop(second_weak)
"#,
        "1:1\ntrue\ntrue\n4:5\n",
    );
}

#[test]
fn cell_contents_never_expose_mutable_or_borrowed_places() {
    for source in [
        "let cell = Cell::new(1); cell.* += 1",
        "let mut cell = Cell::new(1); cell.* += 1",
        "let cell = Cell::new(1); let view = &cell.*",
        "let mut cell = Cell::new(1); let view = &mut cell.*",
        "let cell = Cell::new(Vec[1]); let values = cell.*",
        "let cell = Cell::new(Vec[1]); std::io::print((cell.*)[0])",
        "let cell = Cell::new(Vec[1]); (cell.*).push(2)",
        "let cell = Cell::new(rc 1); let owner = cell.*",
        "let shared = gc Cell::new(1); shared.*.* += 1",
        "fn read(cell: &Cell<i64>) -> i64 { return cell.*.* }",
        "fn replace(cell: &Cell<i64>) -> i64 { return cell.replace(1) }",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn cell_behavior_uses_resolved_types_and_user_methods_keep_their_identity() {
    parity(
        r#"
use std::cell::Cell as Slot
record Cell { value: i64 }
trait Replace { proc replace(self: &Self, value: i64) -> i64 }
impl Replace for Cell { proc replace(self: &Self, value: i64) -> i64 { return self.value + value } }
let ordinary = Cell { value: 10 }
let cell = Slot::new(1)
cell.* = cell.* + 1
std::io::print(ordinary.replace(3), ":", cell.*)
"#,
        "13:2\n",
    );
}
