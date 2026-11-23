//! Scalar references through the HIR/MIR/AIR pipeline: `&T` and `&mut T` parameters over
//! `i64`, `bool`, `f64` and the sized numbers, `p.*` reads and writes, borrowing a local as
//! a call argument, and passing a reference on. These tests state what the language does;
//! they do not compare with any other route.
use cyallo::{Diagnostic, Session, Status, compile};
use cyallo_ir::analysis::IndexedRoutineBody;
use std::sync::Arc;

fn run(source: &str) -> String {
    let program = compile(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..4096 {
        match session.run(1 << 16) {
            Status::Finished => break,
            Status::Paused => {}
            other => panic!("unexpected status {other:?}\n{source}"),
        }
    }
    assert!(matches!(session.status(), Status::Finished), "{source}");
    session.context.output
}

fn failure(source: &str) -> Diagnostic {
    compile(source).expect_err(source)
}

/// Every routine body in `source` is lowered by the pipeline, not the existing route.
fn assert_all_migrated(source: &str) {
    let file = cyallo_parser::parse_modern_snapshot(source.into(), false, cyallo::Jobs(1)).unwrap();
    let handles: Vec<_> = file.units().iter().flat_map(|unit| unit.bodies()).collect();
    assert!(!handles.is_empty());
    for handle in handles {
        if let Some(reason) = IndexedRoutineBody::decline_reason(&handle) {
            panic!("declined ({reason}):\n{source}");
        }
    }
}

#[test]
fn the_example_runs_and_every_routine_is_lowered_by_the_pipeline() {
    let source = include_str!("../examples/references/scalar.yas");
    assert_all_migrated(source);
    assert_eq!(run(source), "16 17\n2 1\ntrue 3\n13\n");
}

#[test]
fn a_shared_reference_reads_the_value_it_refers_to() {
    let source = "fn twice(value: &i64) -> i64 { return value.* * 2 }\n\
                  fn flag(value: &bool) -> bool { return !value.* }\n\
                  fn half(value: &f64) -> f64 { return value.* / 2.0 }\n\
                  let n = 21\nlet b = true\nlet x = 5.0\n\
                  std::io::print(twice(&n), \" \", flag(&b), \" \", half(&x), \" \", n)\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "42 false 2.5 21\n");
}

#[test]
fn a_mutable_reference_writes_and_updates_in_place() {
    let source = "proc set(value: &mut i64) { value.* = 7 }\n\
                  proc grow(value: &mut i64, by: i64) { value.* += by\n value.* *= 2 }\n\
                  proc narrow(value: &mut i32) { value.* += 1 }\n\
                  let mut n = 1\nset(&mut n)\nstd::io::print(n, \" \")\n\
                  grow(&mut n, 3)\nstd::io::print(n, \" \")\n\
                  let mut small: i32 = 40\nnarrow(&mut small)\nstd::io::print(small)\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "7 \n20 \n41\n");
}

#[test]
fn a_reference_can_be_passed_on_and_a_mutable_one_can_become_shared() {
    let source = "fn peek(value: &i64) -> i64 { return value.* }\n\
                  proc bump(value: &mut i64) { value.* += 1 }\n\
                  proc twice(value: &mut i64) {\n    bump(value)\n    bump(value)\n    value.* += peek(value)\n}\n\
                  let mut n = 1\ntwice(&mut n)\nstd::io::print(n)\n";
    assert_all_migrated(source);
    // 1 -> 3 after two bumps, then 3 + 3.
    assert_eq!(run(source), "6\n");
}

#[test]
fn a_routine_may_borrow_its_own_locals_and_parameters() {
    let source = "proc bump(value: &mut i64) { value.* += 1 }\n\
                  fn read(value: &i64) -> i64 { return value.* }\n\
                  proc count(start: i64) -> i64 {\n    let mut local = start\n    bump(&mut local)\n    bump(&mut start)\n    return read(&local) + read(&start)\n}\n\
                  std::io::print(count(10))\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "22\n");
}

#[test]
fn a_swap_through_two_distinct_references_is_allowed() {
    let source = "proc swap(a: &mut i64, b: &mut i64) {\n    let held = a.*\n    a.* = b.*\n    b.* = held\n}\n\
                  let mut x = 1\nlet mut y = 2\nswap(&mut x, &mut y)\nstd::io::print(x, \" \", y)\n";
    assert_all_migrated(source);
    assert_eq!(run(source), "2 1\n");
}

#[test]
fn writing_through_a_shared_reference_is_rejected() {
    let error = failure("proc set(value: &i64) { value.* = 1 }\n");
    assert_eq!(error.code, "E107");
    assert!(error.message.contains("read-only reference"), "{error:?}");
    let error = failure("proc grow(value: &i64) { value.* += 1 }\n");
    assert_eq!(error.code, "E107");
}

#[test]
fn a_mutable_borrow_of_an_immutable_local_is_rejected() {
    let source = "proc set(value: &mut i64) { value.* = 1 }\n\
                  proc caller() {\n    let n = 1\n    set(&mut n)\n}\n";
    let error = failure(source);
    assert_eq!(error.code, "E107");
    assert!(
        error.message.contains("cannot modify a constant"),
        "{error:?}"
    );
    // The same borrow of a `let mut` local is fine.
    assert!(
        compile(
            "proc set(value: &mut i64) { value.* = 1 }\nproc caller() {\n    let mut n = 1\n    set(&mut n)\n}\n"
        )
        .is_ok()
    );
}

#[test]
fn overlapping_loans_in_one_call_must_all_be_shared() {
    let two = "proc two(a: &mut i64, b: &i64) { a.* = b.* }\n\
               fn both(a: &i64, b: &i64) -> i64 { return a.* + b.* }\n";
    let error = failure(&format!(
        "{two}proc caller() {{\n    let mut n = 1\n    two(&mut n, &n)\n}}\n"
    ));
    assert_eq!(error.code, "E118");
    assert!(error.message.contains("overlap"), "{error:?}");
    // Two mutable loans of one place overlap as well.
    let error = failure(
        "proc swap(a: &mut i64, b: &mut i64) { a.* = b.* }\nproc caller() {\n    let mut n = 1\n    swap(&mut n, &mut n)\n}\n",
    );
    assert_eq!(error.code, "E118");
    // Shared loans of one place do not conflict, and distinct places never do.
    assert!(
        compile(&format!(
            "{two}proc ok() {{\n    let n = 1\n    both(&n, &n)\n}}\n"
        ))
        .is_ok()
    );
    assert!(
        compile(&format!(
            "{two}proc ok() {{\n    let mut a = 1\n    let b = 2\n    two(&mut a, &b)\n}}\n"
        ))
        .is_ok()
    );
}

#[test]
fn a_reference_argument_must_match_the_parameter() {
    // The wrong pointee, and a shared reference where a mutable one is wanted.
    let error = failure(
        "proc set(value: &mut i64) { value.* = 1 }\nproc caller() {\n    let mut flag = true\n    set(&mut flag)\n}\n",
    );
    assert_eq!(error.code, "E109");
    let error = failure(
        "proc set(value: &mut i64) { value.* = 1 }\nproc caller() {\n    let mut n = 1\n    set(&n)\n}\n",
    );
    assert_eq!(error.code, "E109");
    assert!(error.message.contains("mismatched types"), "{error:?}");
}

#[test]
fn a_pure_function_cannot_write_through_a_reference() {
    assert!(
        compile("fn bad(value: &mut i64) -> i64 {\n    value.* = 2\n    return value.*\n}\n")
            .is_err()
    );
    // Reading through one is fine for a function.
    assert!(compile("fn good(value: &mut i64) -> i64 { return value.* }\n").is_ok());
}

#[test]
fn a_reference_is_not_a_value() {
    // Using a reference parameter where a number is expected is a type error, not a read.
    assert!(compile("fn f(value: &i64) -> i64 { return value + 1 }\n").is_err());
    assert!(compile("fn f(value: &i64) -> i64 { return value }\n").is_err());
}
