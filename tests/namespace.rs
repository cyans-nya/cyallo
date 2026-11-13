use cyallo::{Session, Status, compile};
use std::sync::Arc;

fn output(source: &str, expected: &str) {
    output_context(source, expected, cyallo::Context::default());
}
fn output_context(source: &str, expected: &str, context: cyallo::Context) {
    let p = Arc::new(compile(source).unwrap());
    use cyallo::backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
    };
    let baseline = TieredBackend::default().compile(p.clone()).unwrap();
    for i in 0..p.functions().len() {
        baseline.precompile(i, Tier::Winch).unwrap();
    }
    assert!(
        baseline.wait_for_compilation(std::time::Duration::from_secs(2)),
        "{:?}",
        baseline.tiers()
    );
    let engines: Vec<Option<Box<dyn Executable>>> = vec![
        None,
        Some(Box::new(baseline)),
        Some(Box::new(CraneliftBackend.compile(p.clone()).unwrap())),
        Some(Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap())),
    ];
    for engine in engines {
        let mut s = Session::with_context(p.clone(), context.clone()).unwrap();
        if let Some(engine) = engine {
            engine.run(&mut s, 100_000).unwrap();
        } else {
            s.run(100_000);
        }
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, expected);
    }
}
#[test]
fn standard_paths_and_import_aliases() {
    output(
        "use std::num::{abs as magnitude, max}\nuse std::string as text\nlet n = magnitude(-4)\nio::print(n, max(1,2), text::upper(\"ok\"))\n",
        "42OK\n",
    );
}
#[test]
fn prelude_is_a_normal_glob_import() {
    output(
        "use std::prelude::*\nlet value = num::abs(-3)\nio::print(value)\n",
        "3\n",
    );
}
#[test]
fn lexical_bindings_are_case_sensitive_and_mutability_is_explicit() {
    output(
        "let x = 1\nlet X = 2\nlet mut total = 0\nif true { let x = 3; total = x + X }\nio::print(x,total)\n",
        "15\n",
    );
    assert!(compile("let n = 1\nn = 2\n").is_err());
    assert!(compile("n = 1\n").is_err());
    assert!(compile("let n = abs(-1)\n").is_err());
}
#[test]
fn inline_modules_and_reexports() {
    output(
        "mod math {\npub fn answer() -> i64 { return 42 }\n}\nmod facade {\npub use crate::math::answer as result\n}\nuse facade::result\nio::print(result())\n",
        "42\n",
    );
    assert!(compile("mod a { fn hidden() -> i64 { return 1 } }\nuse a::hidden\n").is_err());
}
#[test]
fn explicit_readonly_and_writable_references() {
    output(
        "fn read(n: &i64) -> i64 { return *n + 1 }\nproc bump(n: &mut i64) { *n += 1 }\nlet mut n = 4\nbump(&mut n)\nio::print(read(&n),n)\n",
        "65\n",
    );
    assert!(compile("proc bad(n: &i64) { *n = 2 }\n").is_err());
    assert!(compile("fn bad(n: &mut i64) -> i64 { *n = 2; return *n }\n").is_err());
}
#[test]
fn native_reference_store_uses_pointee_tag() {
    output(
        "proc bump(n: &mut i64) { *n += 1 }\nlet mut n = 4\nbump(&mut n)\nio::print(n)\n",
        "5\n",
    );
}
#[test]
fn sized_numbers_and_checked_casts() {
    output(
        "let mut n: u8 = 254\nn += 1\nlet big = 18446744073709551615u64\nlet small = -128i8\nlet f: f32 = 16777217.0\nio::print(n,\":\",big,\":\",small,\":\",f,\":\",n as u32)\n",
        "255:18446744073709551615:-128:16777216:255\n",
    );
    assert!(compile("let n: u8 = 256\n").is_err());
    assert!(compile("let n = 1u8 + 1u16\n").is_err());
    let p = Arc::new(compile("let n = 255u8\nio::print(n + 1u8)\n").unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert!(matches!(s.status(), Status::Failed(_)));
}
#[test]
fn typed_collection_literals_and_ring_deque() {
    output(
        r#"
use std::collections::{Vec as List, VecDeque as Queue}
let mut values = List<u8>[1, 2]
values.push(3)
values[0usize] = 4
let mut queue = Queue[10, 20]
queue.push_front(5)
queue.push_back(30)
io::print(values.len(), ":", values[0], ":", values.pop().unwrap())
io::print(queue.pop_front().unwrap(), ":", queue.pop_back().unwrap(), ":", queue[0])
let empty: List<u32> = List[]
io::print(empty.len())
"#,
        "3:4:3\n5:30:10\n0\n",
    );
    assert!(compile("let values = Vec[1]\nvalues.push(2)\n").is_err());
    assert!(compile("let values = Vec[]\n").is_err());
}
#[test]
fn collection_ranges_and_borrowed_receivers() {
    output(
        r#"
proc append(values: &mut Vec<i64>) { values.push(7); values[0] += 1 }
fn private_value() -> i64 {
    let mut values = Vec[1]
    values.push(2)
    return values[0] + values[1]
}
let mut values = Vec::from(2..5)
append(&mut values)
io::print(values[0], ":", values[3], ":", private_value())
let queue = VecDeque::from(1u8..=3u8)
io::print(queue.len(), ":", queue[2])
"#,
        "3:7:3\n3:3\n",
    );
    assert!(compile("proc bad(values: &Vec<i64>) { values.push(2) }\n").is_err());
}
#[test]
fn parsing_numbers_returns_typed_recoverable_results() {
    output(
        r#"
let value: Result<u8, num::ParseError> = num::parse<u8>("255")
let overflow = num::parse<u8>("256")
let malformed = num::parse<i32>("no")
let floating = num::parse<f32>("1.25")
io::print(value.is_ok(), ":", value.unwrap(), ":", overflow.is_err(), ":", malformed.is_err(), ":", floating.unwrap())
"#,
        "true:255:true:true:1.25\n",
    );
    assert!(compile("let value = num::parse<bool>(\"true\")\n").is_err());
}

#[test]
fn fixed_array_builders_preserve_static_shape() {
    output(
        r#"
const N = 4
let values: [4]i64 = array::from(0..N)
let empty: [0]u8 = array::empty<u8>()
let also_empty: [0]u16 = []
io::print(values[3], ":", array::len(empty), ":", array::len(also_empty))
"#,
        "3:0:0\n",
    );
    assert!(compile("let n = 3\nlet values = array::from(0..n)\n").is_err());
    assert!(compile("let values: []i64 = [1,2]\n").is_err());
}

#[test]
fn borrowed_slices_keep_runtime_extent_and_share_elements() {
    output(
        r#"
fn count<T>(values: &[]T) -> usize { return values.len() }
proc bump(values: &mut []u8) { values[1] += 1 }
let mut fixed = [1u8, 2u8, 3u8]
let readonly: &[]u8 = &fixed
io::print(count(readonly), ":", readonly[2])
bump(&mut fixed)
let mut dynamic = Vec<u8>[4, 5]
let borrowed = dynamic.as_mut_slice()
borrowed[0] += 1
io::print(fixed[1], ":", borrowed[0], ":", borrowed.len())
"#,
        "3:3\n3:5:2\n",
    );
    assert!(compile("proc bad(values: &[]i64) { values[0] = 2 }\n").is_err());
}

#[test]
fn ownership_and_boundary_loans_are_checked() {
    for source in [
        "fn bad() -> &i64 { let value = 1; return &value }\n",
        "proc edit(a: &mut i64, b: &i64) { *a += *b }\nlet mut value = 1\nedit(&mut value, &value)\n",
        "let mut values = Vec[1,2]\nlet view = values.as_slice()\nvalues.push(3)\nio::print(view[0])\n",
        "let values = Vec[1]\nlet moved = values\nio::print(values.len())\n",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    output(
        r#"
proc edit(a: &mut i64, b: &i64) { *a += *b }
let mut pair = (1, 2)
edit(&mut pair.0, &pair.1)
let mut value = 4
let a = &mut value
let b = &mut value
*a += 1
*b += 1
io::print(pair.0, ":", value)
let mut values = Vec[1,2]
let view = values.as_slice()
io::print(view[0])
values.push(3)
io::print(values.len())
"#,
        "3:6\n1\n3\n",
    );
}

#[test]
fn dedicated_managed_syntax_and_reference_counting() {
    let source = r#"
record Pair { value: i64 }
fn make() -> @gc Pair { return @gc Pair { value: 7 } }
fn read(value: &@rc i64) -> i64 { return **value }
proc expired() -> std::rc::Weak<i64> { let owner = @rc 8; return owner.downgrade() }
let managed = make()
let cell = @gc Cell::new(2)
**cell = 5
io::print(managed.value, ":", **cell)
let owner = @rc 9
let cloned = rc::clone(&owner)
let weak = rc::downgrade(&owner)
io::print(rc::strong_count(&owner), ":", read(&cloned))
mem::drop(cloned)
io::print(rc::strong_count(&owner))
mem::drop(owner)
io::print(weak.upgrade().is_none(), ":", expired().upgrade().is_none())
"#;
    output(source, "7:5\n2:9\n1\ntrue:true\n");
    output_context(
        source,
        "7:5\n2:9\n1\ntrue:true\n",
        cyallo::Context {
            gc: cyallo::runtime::gc::GcConfig::stress(),
            ..Default::default()
        },
    );
    assert!(compile("fn bad(value: @rc i64) -> i64 { return *value }\n").is_err());
    assert!(compile("fn bad() -> (@rc i64,i64) { return (@rc 1,2) }\n").is_err());
}

#[test]
fn import_globs_reach_a_fixed_point() {
    output(
        r#"
mod last { pub use crate::middle::*; }
mod middle { pub use crate::first::*; }
mod first { pub const ANSWER = 42 }
use crate::last::*;
std::io::print(ANSWER)
"#,
        "42\n",
    );
}
#[test]
fn callable_identity_survives_aliases_and_shadowing() {
    output(
        r#"
use std::num::abs as magnitude
fn abs(n: i64) -> i64 { return n + 100 }
fn work() -> i64 {
    let magnitude = fn(n: i64) -> i64 { return n + 200 }
    return magnitude(3)
}
std::io::print(abs(3), ":", magnitude(-4), ":", work())
"#,
        "103:4:203\n",
    );
    for name in ["abs", "print", "empty", "trim", "parse", "input"] {
        assert!(compile(&format!("let callable = {name}\n")).is_err());
    }
}
#[test]
fn input_accepts_a_resolved_reference_value() {
    output_context(
        "let mut value: u16 = 0\nlet destination = &mut value\nstd::runtime::caie::input(destination)\nstd::io::print(value)\n",
        "42\n",
        cyallo::Context {
            input: ["42".into()].into(),
            ..Default::default()
        },
    );
}

#[test]
fn top_level_bindings_shadow_with_distinct_storage() {
    output(
        "let value = 1\nlet value = value + 1\nlet value = \"text\"\nstd::io::print(value)\n",
        "text\n",
    );
    let mut repl = cyallo::repl::Repl::new(Default::default()).unwrap();
    for line in [
        "let value = 1",
        "let value = value + 1",
        "std::io::print(value)",
    ] {
        repl.push_line(line).unwrap();
        repl.run(10000);
        assert_eq!(repl.session().status(), &Status::Finished);
    }
    assert_eq!(repl.session().context.output, "2\n");
}
#[test]
fn reference_captures_preserve_mutability() {
    output(
        r#"
let mut value = 10
let read = fn[&value]() -> i64 { return value }
std::io::print(read())
let change = proc[&mut value]() { value += 1 }
change()
std::io::print(value)
"#,
        "10\n11\n",
    );
    for source in [
        "let mut n = 0\nlet f = proc[&n]() { n += 1 }\nf()\n",
        "record R { n: i64 }\nlet mut r = R { n: 0 }\nlet f = proc[&r]() { r.n += 1 }\nf()\n",
        "let n = 0\nlet f = proc[&mut n]() { n += 1 }\nf()\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn rc_temporaries_and_loop_exits_release_owners() {
    output_context(
        r#"
let owner = @rc 9
let weak = owner.downgrade()
std::io::print(weak.upgrade().is_some(), ":", owner.strong_count())
owner.clone()
std::io::print(owner.strong_count())
for i in 0..=3 {
    let local = owner.clone()
    std::io::print(local.strong_count())
    if i == 0 { continue }
    break
}
std::io::print(owner.strong_count())
"#,
        "true:1\n1\n2\n2\n1\n",
        cyallo::Context {
            gc: cyallo::runtime::gc::GcConfig::stress(),
            ..Default::default()
        },
    );
}
