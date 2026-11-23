//! Differential checks of the migrated routine pipeline against the existing route.
//!
//! Each program marks function bodies with `PAD`. The migrated variant removes it, so
//! every routine is in the HIR/MIR/AIR subset. The fallback variant replaces it with a
//! statement outside the subset (a char local), which keeps the old route and changes
//! nothing observable. Both must produce the same output or the same diagnostic.
use cyallo::{Diagnostic, Session, Status, compile};
use cyallo_ir::analysis::IndexedRoutineBody;
use std::sync::Arc;

const PAD: &str = "PAD";
const FALLBACK: &str = "let unused_char = 'a'\n";

fn variants(source: &str) -> (String, String) {
    (source.replace(PAD, ""), source.replace(PAD, FALLBACK))
}

fn run(source: &str) -> String {
    let program = compile(source).unwrap_or_else(|e| panic!("{source}\n{e:?}"));
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

/// Every routine of `source` is accepted by the migrated pipeline.
fn assert_migrated(source: &str) {
    let file = cyallo_parser::parse_modern_snapshot(source.into(), false, cyallo::Jobs(1)).unwrap();
    let handles: Vec<_> = file.units().iter().flat_map(|unit| unit.bodies()).collect();
    assert!(!handles.is_empty());
    for handle in handles {
        assert!(
            IndexedRoutineBody::prepare(&handle, 0).unwrap().is_some(),
            "not migrated:\n{source}"
        );
    }
}

fn assert_fallback(source: &str) {
    let file = cyallo_parser::parse_modern_snapshot(source.into(), false, cyallo::Jobs(1)).unwrap();
    for handle in file.units().iter().flat_map(|unit| unit.bodies()) {
        assert!(IndexedRoutineBody::prepare(&handle, 0).unwrap().is_none());
    }
}

fn parity(source: &str, expected: &str) {
    let (migrated, fallback) = variants(source);
    assert_migrated(&migrated);
    assert_fallback(&fallback);
    assert_eq!(run(&migrated), expected, "migrated:\n{migrated}");
    assert_eq!(run(&fallback), expected, "fallback:\n{fallback}");
}

fn failure(error: &Diagnostic, text: &str) -> (&'static str, String, String) {
    (
        error.code,
        error.message.clone(),
        text.replace(FALLBACK, ""),
    )
}

/// Both variants fail identically (code, message and the source text of the span).
fn same_diagnostic(source: &str, code: &str) {
    let (migrated, fallback) = variants(source);
    assert_migrated(&migrated);
    let a = compile(&migrated).expect_err(&migrated);
    let b = compile(&fallback).expect_err(&fallback);
    assert_eq!(a.code, code, "{migrated}");
    let a = failure(&a, a.span.source_text(&migrated));
    let b = failure(&b, b.span.source_text(&fallback));
    assert_eq!(a, b, "migrated:\n{migrated}\nfallback:\n{fallback}");
}

#[test]
fn counting_loops_match() {
    parity(
        "fn total(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 1..=n { t += i }\n    return t\n}\n\
         std::io::print(total(10), total(0), total(1))\n",
        "5501\n",
    );
    parity(
        "fn total(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 0..n { t += i * i }\n    return t\n}\n\
         std::io::print(total(0), total(1), total(5))\n",
        "0030\n",
    );
    // An empty and a backwards range never run the body.
    parity(
        "fn count(a: i64, b: i64) -> i64 {PAD\n    let mut c = 0\n    for i in a..b { c += 1 }\n    for i in a..=b { c += 10 }\n    return c\n}\n\
         std::io::print(count(5, 2), count(2, 5), count(3, 3))\n",
        "04310\n",
    );
}

#[test]
fn while_loops_and_branches_match() {
    parity(
        "fn collatz(n: i64) -> i64 {PAD\n    let mut a = n\n    let mut steps = 0\n    \
         while a > 1 {\n        if a % 2 == 0 { a = a / 2 } else { a = a * 3 + 1 }\n        steps += 1\n    }\n    return steps\n}\n\
         std::io::print(collatz(1), collatz(6), collatz(27))\n",
        "08111\n",
    );
    parity(
        "fn nested(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 0..n {\n        let mut j = 0\n        \
         while j < i { t += j\n            j += 1 }\n    }\n    return t\n}\nstd::io::print(nested(5))\n",
        "10\n",
    );
}

#[test]
fn locals_annotations_and_calls_match() {
    parity(
        "fn pick(n: i64) -> i64 {PAD\n    let x: i64 = n * 2\n    let big: bool = x > 6\n    let y = x + 1\n    \
         if big { return y } else { return x }\n}\nstd::io::print(pick(2), pick(5))\n",
        "411\n",
    );
    parity(
        "fn sq(x: i64) -> i64 {PAD\n    return x * x\n}\nfn sum_sq(n: i64) -> i64 {PAD\n    let mut t = 0\n    \
         for i in 1..=n { t += sq(i) }\n    return t\n}\nstd::io::print(sum_sq(4))\n",
        "30\n",
    );
    // An early return from inside a loop, with a fallthrough return after it.
    parity(
        "fn first_over(n: i64) -> i64 {PAD\n    let mut i = 0\n    while i < 100 {\n        if i * i > n { return i }\n        \
         i += 1\n    }\n    return -1\n}\nstd::io::print(first_over(50), first_over(100000))\n",
        "8-1\n",
    );
}

#[test]
fn the_same_diagnostics_are_reported() {
    for (source, code) in [
        (
            "fn f(n: i64) -> i64 {PAD\n    let x: bool = 1\n    return n\n}\n",
            "E109",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    let mut t = 0\n    t = true\n    return t\n}\n",
            "E109",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    let mut t = 0\n    t += true\n    return t\n}\n",
            "E105",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    for i in true..false { }\n    return n\n}\n",
            "E109",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    for i in 1..true { }\n    return n\n}\n",
            "E109",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    while n { }\n    return n\n}\n",
            "E109",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    let mut t = 0\n    while t < n { t += 1 }\n}\n",
            "E103",
        ),
        (
            "fn f(n: i64) -> i64 {PAD\n    while true { return 1 }\n}\n",
            "E103",
        ),
    ] {
        same_diagnostic(source, code);
    }
}

#[test]
fn programs_outside_the_subset_keep_the_existing_route() {
    // Each is reported or run by the existing route; the subset must decline it.
    for source in [
        // Assignment to a parameter, an immutable local, and a loop counter.
        "fn f(n: i64) -> i64 { n = 1\n return n }",
        "fn f(n: i64) -> i64 { let x = 1\n x = 2\n return x }",
        "fn f(n: i64) -> i64 { for i in 0..n { i = 1 }\n return n }",
        // A name used before or outside its scope, and a repeated name.
        "fn f(n: i64) -> i64 { let y = x\n let x = 1\n return y }",
        "fn f(n: i64) -> i64 { if n > 0 { let x = 1 }\n return x }",
        "fn f(n: i64) -> i64 { let x = 1\n let x = 2\n return x }",
        "fn f(n: i64) -> i64 { for i in 0..n { }\n return i }",
        // A call to a local name, and `break`/`continue`.
        "fn f(n: i64) -> i64 { let g = 1\n return g(n) }",
    ] {
        assert_fallback(source);
    }
}

#[test]
fn break_continue_short_circuit_unary_and_bool_match() {
    // `break` and `continue` in while and for, including nested loops.
    parity(
        "fn f(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 0..n {\n        if i % 2 == 0 { continue }\n        if i > 7 { break }\n        t += i\n    }\n    return t\n}\n\
         fn g(n: i64) -> i64 {PAD\n    let mut a = 0\n    let mut i = 0\n    while true {\n        i += 1\n        if i > n { break }\n        if i % 3 == 0 { continue }\n        a += i\n    }\n    return a\n}\n\
         fn h(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 0..n { for j in 0..n { if j > i { break }\n        t += 1 } }\n    return t\n}\n\
         std::io::print(f(20), \" \", g(10), \" \", h(4))\n",
        "16 37 10\n",
    );
    // `&&` and `||` run the right side only when needed; a side effect is a call that traps.
    parity(
        "fn safe(n: i64) -> bool {PAD\n    return n != 0 && 100 / n > 3\n}\n\
         fn either(a: i64, b: i64) -> bool {PAD\n    return a > 5 || b > 5 && a != b\n}\n\
         fn pick(n: i64) -> i64 {PAD\n    let ok = safe(n) || n == 0\n    if ok && !(n > 50) { return 1 }\n    return 0\n}\n\
         std::io::print(safe(0), \" \", safe(10), \" \", safe(50), \" \", either(1, 9), \" \", either(1, 1), \" \", pick(0), pick(4), pick(60))\n",
        "false true false true false 110\n",
    );
    // Unary minus and not, and bool parameters and results.
    parity(
        "fn neg(n: i64) -> i64 {PAD\n    return -n + -(n * 2)\n}\n\
         fn flip(b: bool, n: i64) -> bool {PAD\n    if !b { return n > 0 }\n    return !(n > 0)\n}\n\
         std::io::print(neg(5), \" \", flip(true, 3), \" \", flip(false, 3), \" \", flip(false, -1))\n",
        "-15 false true false\n",
    );
}

#[test]
fn new_forms_report_the_same_diagnostics() {
    for (source, code) in [
        ("fn f(n: i64) -> i64 {PAD\n    return -true\n}\n", "E105"),
        ("fn f(n: i64) -> bool {PAD\n    return !n\n}\n", "E105"),
        ("fn f(n: i64) -> bool {PAD\n    return n\n}\n", "E109"),
        ("fn f(b: bool) -> i64 {PAD\n    return b\n}\n", "E109"),
        ("fn f(b: bool) -> i64 {PAD\n    return b + 1\n}\n", "E105"),
        (
            "fn f(n: i64) -> i64 {PAD\n    for i in 0..n { break }\n}\n",
            "E103",
        ),
    ] {
        same_diagnostic(source, code);
    }
}
