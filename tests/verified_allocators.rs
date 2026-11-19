//! Counted objects with their own allocator through the verified interpreter.
//! Expected lines are written by hand from the allocator protocol in
//! std/mem.ya: the allocator is asked for a block when the object is made and
//! gets it back when the last owner is released.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> Vec<String> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 1_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_gc_stress(stress);
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    let status = activation.resume(usize::MAX);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines.lock().unwrap().clone()
}

const ALLOCATOR: &str = r#"
use std::mem::{Allocator, Layout, Allocation, AllocError}

record Named { name: string, releases: gc Cell<i64> }
impl Allocator for Named {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {
        return std::mem::allocate(layout)
    }
    proc deallocate(self: &mut Self, allocation: Allocation) {
        self.releases.*.* = self.releases.*.* + 1
        std::io::print("freed ", self.name)
        std::mem::deallocate(allocation)
    }
}
"#;

#[test]
fn the_block_goes_back_when_the_last_owner_is_released() {
    let source = format!(
        "{ALLOCATOR}\n{}",
        r#"
let releases = gc Cell::new(0)
let first = rc(Named { name: "a", releases: releases }) 10
let second = first.clone()
std::mem::drop(first)
std::io::print("count ", first_count(&second), " released ", releases.*.*)
std::mem::drop(second)
std::io::print("released ", releases.*.*)
proc first_count(owner: &rc i64) -> usize { return owner.strong_count() }
"#
    );
    for stress in [false, true] {
        assert_eq!(
            run(&source, stress),
            ["count 1 released 0", "freed a", "released 1"]
        );
    }
}

#[test]
fn owners_released_together_are_deallocated_last_component_first() {
    let source = format!(
        "{ALLOCATOR}\n{}",
        r#"
let releases = gc Cell::new(0)
let pair = (rc(Named { name: "x", releases: releases }) 1, rc(Named { name: "y", releases: releases }) 2)
std::mem::drop(pair)
std::io::print("released ", releases.*.*)
"#
    );
    assert_eq!(run(&source, false), ["freed y", "freed x", "released 2"]);
}

#[test]
fn a_scope_that_ends_releases_its_owner() {
    let source = format!(
        "{ALLOCATOR}\n{}",
        r#"
let releases = gc Cell::new(0)
proc scoped(releases: gc Cell<i64>) {
    let owner = rc(Named { name: "s", releases: releases }) 3
    std::io::print("inside ", owner.*)
}
scoped(releases)
std::io::print("after ", releases.*.*)
"#
    );
    assert_eq!(run(&source, true), ["inside 3", "freed s", "after 1"]);
}

#[test]
fn the_managed_example_runs_under_gc_stress() {
    assert_eq!(
        run(include_str!("../examples/managed.ya"), true),
        ["owned:1", "1:false"]
    );
}
