use cyallo::{CompileOptions, Dialect, compile, compile_with_options, vm::{Context, Session, Status}};
use std::sync::Arc;

const ALLOCATOR: &str = r#"
use std::mem::{Allocator, Layout, Allocation, AllocError}
record Counting { allocated: gc Cell<i64>, released: gc Cell<i64> }
impl Allocator for Counting {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {
        self.allocated.*.* = self.allocated.*.* + 1
        return std::mem::allocate(layout)
    }
    proc deallocate(self: &mut Self, allocation: Allocation) {
        self.released.*.* = self.released.*.* + 1
        std::mem::deallocate(allocation)
    }
}
"#;
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn fallible_allocation_returns_errors_and_drops_the_payload() {
    output(
        &format!(
            r#"{ALLOCATOR}
record Denied {{}}
impl Allocator for Denied {{
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {{
        return Result::Err(AllocError::new("denied"))
    }}
    proc deallocate(self: &mut Self, allocation: Allocation) {{ std::mem::deallocate(allocation) }}
}}
let allocated = gc Cell::new(0)
let released = gc Cell::new(0)
let payload = rc(Counting {{ allocated: allocated, released: released }}) 3
let result = std::rc::try_new_in(payload, Denied {{}})
std::io::print(result.is_err(), ":", released.*.*)
let success = std::rc::try_new_in(7, Counting {{ allocated: allocated, released: released }}).unwrap()
std::io::print(success.*)
std::mem::drop(success)
std::io::print(allocated.*.*, ":", released.*.*)
"#
        ),
        "true:1\n7\n2:2\n",
    );
}
#[test]
fn allocation_tokens_are_move_only_and_allocator_borrows_cannot_escape() {
    for source in [
        format!("{ALLOCATOR}\nlet layout = Layout::new(8,8).unwrap()\nlet allocation = std::mem::allocate(layout).unwrap()\nlet second = std::mem::clone(&allocation)\n"),
        format!("{ALLOCATOR}\nproc bad() -> rc i64 {{ let mut allocator = Counting {{ allocated: gc Cell::new(0), released: gc Cell::new(0) }}; return rc(&mut allocator) 1 }}\n"),
        format!("{ALLOCATOR}\nlet allocator = Counting {{ allocated: gc Cell::new(0), released: gc Cell::new(0) }}\nlet owner = rc(&allocator) 1\n"),
        "let owner = rc(1) 2\n".into(),
        "fn bad() { let layout = std::mem::Layout::new(8,8).unwrap(); std::mem::allocate(layout) }\n".into(),
    ] { assert!(compile(&source).is_err(), "accepted {source}"); }
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
