use cyallo::runtime::{BoundsError, RuntimeError, UninitializedRead};
use cyallo::{
    CompileOptions, Context, Dialect, Session, Status,
    backend::{CraneliftBackend, Executable, NativeBackend},
    compile_with_options,
};
use std::sync::Arc;
fn program(source: &str) -> Arc<cyallo::ir::Program> {
    Arc::new(
        compile_with_options(
            source,
            CompileOptions {
                dialect: Dialect::Simplified,
                ..Default::default()
            },
        )
        .unwrap(),
    )
}
fn run(source: &str) -> (String, Status) {
    let p = program(source);
    let mut a = Session::new(p.clone()).unwrap();
    let mut b = Session::new(p.clone()).unwrap();
    a.run(100_000);
    CraneliftBackend
        .compile(p)
        .unwrap()
        .run(&mut b, 100_000)
        .unwrap();
    assert_eq!(a.status(), b.status());
    assert_eq!(a.context.output, b.context.output);
    (a.context.output.clone(), a.status().clone())
}
#[test]
fn no_declarations() {
    let (out, status) = run(include_str!("../examples/simplified.psc"));
    assert_eq!(out, "3:11:3:123\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn array_grows_without_allocating_holes() {
    let p = program("Arr[4294967294] <- 123\nOUTPUT LENGTH(Arr), \":\", Arr[4294967294]\n");
    let context = Context {
        max_cells: 4,
        ..Default::default()
    };
    let mut session = Session::with_context(p.clone(), context).unwrap();
    CraneliftBackend
        .compile(p)
        .unwrap()
        .run(&mut session, 1000)
        .unwrap();
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "4294967295:123\n");
}
#[test]
fn missing_reads_fail() {
    for (source, expected) in [
        (
            "Arr[10] <- 123\nOUTPUT Arr[0]\n",
            RuntimeError::Uninitialized(UninitializedRead::Hole(0)),
        ),
        (
            "OUTPUT Count\nCount <- 0\n",
            RuntimeError::Uninitialized(UninitializedRead::Storage),
        ),
        (
            "Arr[10] <- 123\nOUTPUT Arr[11]\n",
            RuntimeError::Uninitialized(UninitializedRead::Hole(11)),
        ),
        (
            "Arr[10] <- 123\nOUTPUT Arr[-1]\n",
            RuntimeError::Bounds(BoundsError::SparseIndex(-1)),
        ),
        (
            "Arr[4294967295] <- 1\n",
            RuntimeError::Bounds(BoundsError::SparseIndex(4294967295)),
        ),
    ] {
        let (_, status) = run(source);
        let Status::Failed(failure) = status else {
            panic!("expected failure: {source}")
        };
        assert_eq!(failure.error, expected, "{source}");
    }
}
#[test]
fn inferred_types_are_fixed() {
    for source in [
        "Count <- 0\nCount <- \"oops\"\n",
        "Arr[10] <- 123\nArr[0] <- \"oops\"\n",
        "Arr[10] <- 123\nArr <- 0\n",
        "Count <- Count + 1\n",
    ] {
        assert!(
            compile_with_options(
                source,
                CompileOptions {
                    dialect: Dialect::Simplified,
                    ..Default::default()
                }
            )
            .is_err(),
            "accepted {source}"
        );
    }
}
#[test]
fn strict_mode_requires_declarations() {
    assert!(compile("Count <- 0\n").is_err());
    assert!(compile("Arr[10] <- 123\n").is_err());
}
#[test]
fn sparse_copies_are_independent() {
    let (out, status) = run(
        "Arr[10] <- 123\nCopy <- Arr\nArr[10] <- 456\nArr[20] <- 789\nOUTPUT Copy[10], \":\", LENGTH(Copy), \":\", Arr[10], \":\", LENGTH(Arr)\n",
    );
    assert_eq!(out, "123:11:456:21\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn nested_sparse_arrays() {
    let (out, status) = run(
        "Grid[3][10] <- 123\nCopy <- Grid\nGrid[3][10] <- 456\nOUTPUT LENGTH(Grid), \":\", LENGTH(Grid[3]), \":\", Copy[3][10]\n",
    );
    assert_eq!(out, "4:11:123\n");
    assert_eq!(status, Status::Finished);
}

#[test]
fn sparse_write_after_rhs_array_reassignment_uses_the_new_array() {
    let (out, status) = run(
        "C[1] <- 7\nC[2] <- 8\nD[1] <- 11\nD[2] <- 22\nFUNCTION Reset() RETURNS INTEGER\n C <- D\n RETURN 99\nENDFUNCTION\nC[5] <- Reset()\nOUTPUT C[5], \":\", LENGTH(C)\n",
    );
    assert_eq!(out, "99:6\n");
    assert_eq!(status, Status::Finished);
}

#[test]
fn globals_created_and_modified_in_calls() {
    let (out, status) = run(
        "Arr[10] <- 123\nPROCEDURE Update()\n Arr[20] <- 456\n Temp <- 7\nENDPROCEDURE\nCALL Update()\nCALL Update()\nOUTPUT Arr[20], \":\", LENGTH(Arr)\n",
    );
    assert_eq!(out, "456:21\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn inferred_variables_work_with_typed_routines() {
    let (out, status) = run(
        "Count <- 1\nArr[10] <- 123\nPROCEDURE Increment(BYREF n : INTEGER)\n n <- n + 1\nENDPROCEDURE\nFUNCTION Twice(n : INTEGER) RETURNS INTEGER\n Result <- n * 2\n RETURN Result\nENDFUNCTION\nCALL Increment(Count)\nCALL Increment(Arr[10])\nAnswer <- Twice(Count)\nOUTPUT Answer, \":\", Arr[10]\n",
    );
    assert_eq!(out, "4:124\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn case_ranges_and_minimum_integer() {
    let (out, status) = run(
        "Count <- 2\nCASE OF Count\n 0 TO 3 : OUTPUT \"small\"\n 4 TO 10 : OUTPUT \"larger\"\n OTHERWISE : OUTPUT \"other\"\nENDCASE\nOUTPUT -9223372036854775808\n",
    );
    assert_eq!(out, "small\n-9223372036854775808\n");
    assert_eq!(status, Status::Finished);
}
#[test]
fn extensions_are_explicit() {
    assert!(compile("OUTPUT LEFT(\"abc\",1)\n").is_err());
    assert!(
        compile_with_options(
            "OUTPUT LEFT(\"abc\",1)\n",
            CompileOptions {
                dialect: cyallo::Dialect::Compatibility,
                convenience_builtins: true,
                ..Default::default()
            }
        )
        .is_ok()
    );
}

fn compile(source: &str) -> Result<cyallo::ir::Program, cyallo::Diagnostic> {
    cyallo::compile_with_options(
        source,
        cyallo::CompileOptions {
            dialect: cyallo::Dialect::Compatibility,
            ..Default::default()
        },
    )
}
