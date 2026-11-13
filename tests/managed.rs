use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use std::{sync::Arc, time::Duration};
const ALLOCATOR: &str = r#"
use std::mem::{Allocator, Layout, Allocation, AllocError}
record Counting { allocated: @gc Cell<i64>, released: @gc Cell<i64> }
impl Allocator for Counting {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {
        **self.allocated = **self.allocated + 1
        return std::mem::allocate(layout)
    }
    proc deallocate(self: &mut Self, allocation: Allocation) {
        **self.released = **self.released + 1
        std::mem::deallocate(allocation)
    }
}
"#;
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
        for (engine_index, engine) in engines.into_iter().enumerate() {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..100_000 {
                match &engine {
                    Some(engine) if step % 3 != 0 => engine.run(&mut session, 1).unwrap(),
                    _ => {
                        session.run(1);
                    }
                }
                if session.status() != &Status::Paused {
                    break;
                }
            }
            assert_eq!(
                session.status(),
                &Status::Finished,
                "deferred={deferred}, engine={engine_index}, output={:?}",
                session.context.output
            );
            assert_eq!(session.context.output, expected);
        }
    }
}
#[test]
fn gc_sharing_example() {
    parity(
        include_str!("../examples/gc-sharing.ya"),
        "snapshot: 10\nshared: 11:11\nretained: 21\n",
    );
}
#[test]
fn rc_ownership_example() {
    parity(
        include_str!("../examples/rc-ownership.ya"),
        "strong: 2\nsurvivor: 42\nupgraded: 42:2\nexpired: true\n",
    );
}
#[test]
fn combined_gc_rc_example() {
    parity(
        include_str!("../examples/gc-rc.ya"),
        "first: 7\nsecond: 7\nfirst expired: true\nsecond survives: 9\ngc survives: 11\n",
    );
}
#[test]
fn recursive_generic_and_mutual_records_use_resolved_layouts() {
    parity(
        r#"
record Node<T> { value: T, edges: Cell<@gc Vec<@gc Node<T>>> }
record Left { value: i64, right: Cell<@gc Vec<@gc Right>> }
record Right { left: @gc Left }
let first = @gc Node<i64> { value: 1, edges: Cell::new(@gc Vec<@gc Node<i64>>[]) }
let second = @gc Node<i64> { value: 2, edges: Cell::new(@gc Vec[first]) }
*first.edges = @gc Vec[second]
let next = (*first.edges)[0]
std::io::print(next.value, ":", (*next.edges)[0].value)
let left = @gc Left { value: 3, right: Cell::new(@gc Vec<@gc Right>[]) }
let right = @gc Right { left: left }
*left.right = @gc Vec[right]
std::io::print((*left.right)[0].left.value)
record Tree { value: i64, children: Vec<Tree> }
let root = Tree { value: 4, children: Vec[Tree { value: 5, children: Vec<Tree>[] }] }
std::io::print(root.value, ":", root.children[0].value)
"#,
        "2:1\n3\n4:5\n",
    );
}
#[test]
fn recursive_rc_nodes_release_when_backedges_are_weak() {
    parity(
        r#"
record Node { next: Cell<Option<@rc Node>>, previous: Cell<Option<std::rc::Weak<Node>>> }
let first = @rc Node { next: Cell::new(std::option::none<@rc Node>()), previous: Cell::new(std::option::none<std::rc::Weak<Node>>()) }
let second = @rc Node { next: Cell::new(std::option::none<@rc Node>()), previous: Cell::new(std::option::some(first.downgrade())) }
*first.next = std::option::some(std::rc::clone(&second))
let first_weak = first.downgrade()
let second_weak = second.downgrade()
std::mem::drop(second)
std::mem::drop(first)
std::io::print(first_weak.upgrade().is_none(), ":", second_weak.upgrade().is_none())
std::mem::drop(first_weak)
std::mem::drop(second_weak)
"#,
        "true:true\n",
    );
}
#[test]
fn owned_allocator_survives_return_and_deallocates_through_trait() {
    parity(
        &format!(
            r#"{ALLOCATOR}
proc make(allocated: @gc Cell<i64>, released: @gc Cell<i64>) -> @rc string {{
    return @rc(Counting {{ allocated: allocated, released: released }}) "owned"
}}
let allocated = @gc Cell::new(0)
let released = @gc Cell::new(0)
let owner = make(allocated, released)
let weak = owner.downgrade()
std::io::print(*owner, ":", **allocated)
std::mem::drop(owner)
std::io::print(**released, ":", weak.upgrade().is_some())
std::mem::drop(weak)
"#
        ),
        "owned:1\n1:false\n",
    );
}
#[test]
fn fallible_allocation_returns_errors_and_drops_the_payload() {
    parity(
        &format!(
            r#"{ALLOCATOR}
record Denied {{}}
impl Allocator for Denied {{
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {{
        return std::result::err<Allocation, AllocError>(AllocError::new("denied"))
    }}
    proc deallocate(self: &mut Self, allocation: Allocation) {{ std::mem::deallocate(allocation) }}
}}
let allocated = @gc Cell::new(0)
let released = @gc Cell::new(0)
let payload = @rc(Counting {{ allocated: allocated, released: released }}) 3
let result = std::rc::try_new_in(payload, Denied {{}})
std::io::print(result.is_err(), ":", **released)
let success = std::rc::try_new_in(7, Counting {{ allocated: allocated, released: released }}).unwrap()
std::io::print(*success)
std::mem::drop(success)
std::io::print(**allocated, ":", **released)
"#
        ),
        "true:1\n7\n2:2\n",
    );
}
#[test]
fn allocation_tokens_are_move_only_and_allocator_borrows_cannot_escape() {
    for source in [
        format!("{ALLOCATOR}\nlet layout = Layout::new(8,8).unwrap()\nlet allocation = std::mem::allocate(layout).unwrap()\nlet second = std::mem::clone(&allocation)\n"),
        format!("{ALLOCATOR}\nproc bad() -> @rc i64 {{ let mut allocator = Counting {{ allocated: @gc Cell::new(0), released: @gc Cell::new(0) }}; return @rc(&mut allocator) 1 }}\n"),
        format!("{ALLOCATOR}\nlet allocator = Counting {{ allocated: @gc Cell::new(0), released: @gc Cell::new(0) }}\nlet owner = @rc(&allocator) 1\n"),
        "let owner = @rc(1) 2\n".into(),
        "fn bad() { let layout = std::mem::Layout::new(8,8).unwrap(); std::mem::allocate(layout) }\n".into(),
    ] { assert!(compile(&source).is_err(), "accepted {source}"); }
}
#[test]
fn borrowed_allocator_is_kept_until_weak_handles_are_released() {
    parity(
        &format!(
            r#"{ALLOCATOR}
let allocated = @gc Cell::new(0)
let released = @gc Cell::new(0)
let mut allocator = Counting {{ allocated: allocated, released: released }}
let owner = @rc(&mut allocator) 12
let weak = owner.downgrade()
std::mem::drop(owner)
std::io::print(weak.upgrade().is_none(), ":", **released)
std::mem::drop(weak)
std::io::print(**allocator.allocated)
"#
        ),
        "true:1\n1\n",
    );
}

#[test]
fn allocator_can_reuse_owned_backing_storage_after_payload_destruction() {
    parity(
        r#"
use std::mem::{Allocator, Layout, Allocation, AllocError}
record Pool { free: Vec<Allocation>, system: @gc Cell<i64> }
impl Allocator for Pool {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {
        if self.free.len() > 0 {
            return std::result::ok<Allocation, AllocError>(self.free.pop().unwrap())
        }
        **self.system = **self.system + 1
        return std::mem::allocate(layout)
    }
    proc deallocate(self: &mut Self, allocation: Allocation) { self.free.push(allocation) }
}
let system = @gc Cell::new(0)
let mut pool = Pool { free: Vec<Allocation>[], system: system }
for i in 0..=9 {
    let owner = @rc(&mut pool) i
    std::io::print(*owner)
}
std::io::print(**system, ":", pool.free.len())
std::mem::drop(pool)
"#,
        "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n1:1\n",
    );
}
#[test]
fn repl_relinks_allocator_deallocation_methods() {
    let mut repl = cyallo::repl::Repl::new(Default::default()).unwrap();
    for source in [
        ALLOCATOR,
        "let allocated = @gc Cell::new(0); let released = @gc Cell::new(0); let owner = @rc(Counting { allocated: allocated, released: released }) 3",
        "fn added() -> i64 { return 42 }",
        "std::mem::drop(owner); std::io::print(**released, added())",
    ] {
        repl.push_line(source).unwrap();
        repl.run(10000);
        assert_eq!(repl.session().status(), &Status::Finished);
    }
    assert_eq!(repl.session().context.output, "142\n");
}
#[test]
fn parenthesized_rc_payload_is_distinct_from_an_allocator_argument() {
    parity(
        "let owner = @rc(2 + 3)\nstd::io::print(*owner)\nstd::mem::drop(owner)\n",
        "5\n",
    );
}

#[test]
fn weak_results_preserve_borrowed_allocator_lifetimes() {
    parity(
        &format!(
            r#"{ALLOCATOR}
proc weak(allocator: &'a mut Counting) -> std::rc::Weak<'a, i64> {{
    let owner = @rc(allocator) 3
    return owner.downgrade()
}}
let allocated = @gc Cell::new(0)
let released = @gc Cell::new(0)
let mut allocator = Counting {{ allocated: allocated, released: released }}
let handle = weak(&mut allocator)
std::io::print(handle.upgrade().is_none(), ":", **released)
std::mem::drop(handle)
std::io::print(**allocator.allocated)
"#
        ),
        "true:1\n1\n",
    );
    let source = format!(
        r#"{ALLOCATOR}
proc bad() -> std::rc::Weak<i64> {{
    let mut allocator = Counting {{ allocated: @gc Cell::new(0), released: @gc Cell::new(0) }}
    let owner = @rc(&mut allocator) 1
    return owner.downgrade()
}}
"#
    );
    assert!(compile(&source).is_err());
}
