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

#[test]
fn procedures_and_call_statements_match() {
    // A procedure runs for its effects; the calls here are the effects the pipeline
    // can see, so the check is that both routes accept, link and run the program.
    parity(
        "proc work(n: i64) {PAD\n    let mut t = 0\n    for i in 0..n {\n        if i > 5 { return }\n        t += i\n    }\n}\n\
         proc outer(n: i64) {PAD\n    work(n)\n    work(n + 1)\n    if n > 100 { return }\n    work(2)\n}\n\
         fn pure_sum(n: i64) -> i64 {PAD\n    let mut t = 0\n    for i in 0..n { t += i }\n    return t\n}\n\
         outer(3)\nouter(500)\nstd::io::print(pure_sum(10))\n",
        "45\n",
    );
}

#[test]
fn procedure_diagnostics_match() {
    for (source, code) in [
        // An argument of the wrong type in a call statement.
        (
            "proc helper(n: i64) {PAD\n}\nproc f(n: i64) {PAD\n    helper(true)\n}\n",
            "E109",
        ),
        // A missing argument.
        (
            "proc helper(n: i64) {PAD\n}\nproc f(n: i64) {PAD\n    helper()\n}\n",
            "E110",
        ),
        // The same in an expression call, reported at the argument.
        (
            "fn g(n: i64) -> i64 {PAD\n    return n\n}\nfn f(n: i64) -> i64 {PAD\n    return g(true)\n}\n",
            "E109",
        ),
    ] {
        same_diagnostic(source, code);
    }
}

#[test]
fn procedures_outside_the_subset_keep_the_existing_route() {
    // Declined by HIR itself.
    for source in ["proc p() { return 1 }", "fn f() -> i64 { return }"] {
        assert_fallback(source);
    }
    // Declined when the linker checks the callee: a function called as a statement is
    // accepted by the existing route, a procedure used as a value is its error.
    let run_ok = |source: &str| compile(&format!("{source}\nstd::io::print(0)\n"));
    assert!(run_ok("fn g() -> i64 { return 1 }\nproc p() { g() }").is_ok());
    assert!(run_ok("proc h() { }\nfn f() -> i64 { return h() }").is_err());
}

#[test]
fn sized_integers_and_floats_match() {
    parity(
        "fn f(a: i32, b: i32) -> i32 {PAD\n    return a * 2 + b - 1\n}\n\
         fn g(x: u8) -> u8 {PAD\n    return x + 1\n}\n\
         fn area(r: f64) -> f64 {PAD\n    return 3.5 * r * r\n}\n\
         fn bump(n: usize) -> usize {PAD\n    return n + 1\n}\n\
         std::io::print(f(3, 4), \" \", g(5), \" \", area(2.0), \" \", bump(9))\n",
        "9 6 14 10\n",
    );
    // Locals, annotations, compound assignment, comparisons and calls in sized types.
    parity(
        "fn step(a: i32) -> i32 {PAD\n    return a + 1\n}\n\
         fn run(n: i32) -> i32 {PAD\n    let y: i32 = n + 1\n    let z = n * 3\n    let mut t: i32 = 0\n    t += 2\n    t -= 1\n    \
         if y > 3 && z < 100 { t += step(y) + step(2) }\n    return t\n}\n\
         fn fl(x: f64) -> f64 {PAD\n    let mut a = x\n    a += 1.5\n    a *= 2.0\n    if a > 4.0 { return a / 2.0 }\n    return a\n}\n\
         std::io::print(run(5), \" \", run(1), \" \", fl(1.0), \" \", fl(3.0))\n",
        "11 1 2.5 4.5\n",
    );
    // Division is integer division for integers, and sized literals on either side.
    parity(
        "fn d(a: i32) -> i32 {PAD\n    return a / 2 + 10 / a\n}\n\
         fn neg(a: i32) -> i32 {PAD\n    let m: i8 = -5\n    return -a + (0 - 1)\n}\n\
         fn big(a: u64) -> u64 {PAD\n    return a * 1000\n}\n\
         std::io::print(d(5), \" \", neg(3), \" \", big(7))\n",
        "4 -4 7000\n",
    );
}

#[test]
fn numeric_typing_reports_the_same_diagnostics() {
    for (source, code) in [
        // A wider value into a narrower local or result.
        (
            "fn f(n: i64) -> i32 {PAD\n    let x: i32 = n\n    return x\n}\n",
            "E109",
        ),
        ("fn f(a: i32) -> i64 {PAD\n    return a\n}\n", "E109"),
        // Literals that do not fit, or that are the wrong kind for the context.
        ("fn f(a: u8) -> u8 {PAD\n    return 300\n}\n", "E10"),
        ("fn f(a: u8) -> u8 {PAD\n    return a + 300\n}\n", "E10"),
        (
            "fn f(a: i32) -> i32 {PAD\n    let x: i32 = 2.5\n    return a\n}\n",
            "E109",
        ),
        ("fn f(a: i32) -> i32 {PAD\n    return a + 2.5\n}\n", "E109"),
        ("fn f(r: f64) -> f64 {PAD\n    return r * 2\n}\n", "E109"),
        (
            "fn f(r: f64) -> f64 {PAD\n    let x: f64 = 2\n    return r\n}\n",
            "E109",
        ),
        ("fn f(n: i64) -> bool {PAD\n    return n < 2.0\n}\n", "E109"),
        // Mixed operand types.
        (
            "fn f(a: i32, b: i64) -> i64 {PAD\n    return a + b\n}\n",
            "E109",
        ),
        (
            "fn f(a: i32, b: u8) -> bool {PAD\n    return a == b\n}\n",
            "E109",
        ),
        (
            "fn f(a: i32) -> i32 {PAD\n    let mut t: i32 = 0\n    t += a * 2.0\n    return t\n}\n",
            "E109",
        ),
        // A range whose bounds differ in type, and a float range.
        (
            "fn f(n: i32) -> i32 {PAD\n    for i in 0..n { }\n    return n\n}\n",
            "E109",
        ),
        (
            "fn f(n: f64) -> f64 {PAD\n    for i in 0.0..n { }\n    return n\n}\n",
            "E109",
        ),
        // An argument of the wrong numeric type.
        (
            "fn g(a: i32) -> i32 {PAD\n    return a\n}\nfn f(n: i64) -> i32 {PAD\n    return g(n)\n}\n",
            "E109",
        ),
        (
            "fn g(a: u8) -> u8 {PAD\n    return a\n}\nfn f(n: u8) -> u8 {PAD\n    return g(999)\n}\n",
            "E10",
        ),
    ] {
        same_diagnostic_prefix(source, code);
    }
}

/// Like `same_diagnostic`, with the code compared by prefix, so one case can cover the
/// conversion errors that share a leading digit group.
fn same_diagnostic_prefix(source: &str, code: &str) {
    let (migrated, fallback) = variants(source);
    assert_migrated(&migrated);
    let a = compile(&migrated).expect_err(&migrated);
    let b = compile(&fallback).expect_err(&fallback);
    assert!(a.code.starts_with(code), "{} for\n{migrated}", a.code);
    let a = failure(&a, a.span.source_text(&migrated));
    let b = failure(&b, b.span.source_text(&fallback));
    assert_eq!(a, b, "migrated:\n{migrated}\nfallback:\n{fallback}");
}

/// A small deterministic generator (no dependency), so a failure reproduces from its seed.
struct Random(u64);

impl Random {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound
    }
}

const TYPES: [&str; 6] = ["i64", "i32", "u8", "i8", "f64", "usize"];

fn expression(random: &mut Random, names: &[String], depth: usize) -> String {
    let leaf = depth == 0 || random.next(3) == 0;
    if leaf {
        return match random.next(5) {
            0 | 1 => names[random.next(names.len())].clone(),
            2 => format!("{}", random.next(300)),
            3 => format!("-{}", random.next(130)),
            _ => format!("{}.5", random.next(20)),
        };
    }
    match random.next(8) {
        0 => format!("({})", expression(random, names, depth - 1)),
        1 => format!("-{}", expression(random, names, depth - 1)),
        _ => {
            let operator = ["+", "-", "*", "/", "%", "<", ">=", "=="][random.next(8)];
            format!(
                "({} {operator} {})",
                expression(random, names, depth - 1),
                expression(random, names, depth - 1)
            )
        }
    }
}

fn generated_function(random: &mut Random, index: usize) -> String {
    let parameters = 1 + random.next(2);
    let mut names = Vec::new();
    let mut signature = Vec::new();
    let mut arguments = Vec::new();
    for position in 0..parameters {
        let name = format!("p{position}");
        let ty = TYPES[random.next(TYPES.len())];
        signature.push(format!("{name}: {ty}"));
        arguments.push(if ty == "f64" { "2.5" } else { "3" });
        names.push(name);
    }
    let mut body = String::new();
    for local in 0..random.next(3) {
        let name = format!("v{local}");
        let annotation = if random.next(2) == 0 {
            format!(": {}", TYPES[random.next(TYPES.len())])
        } else {
            String::new()
        };
        body += &format!(
            "    let mut {name}{annotation} = {}\n",
            expression(random, &names, 2)
        );
        names.push(name.clone());
        if random.next(2) == 0 {
            let operator = ["=", "+=", "-=", "*="][random.next(4)];
            body += &format!("    {name} {operator} {}\n", expression(random, &names, 2));
        }
    }
    body += &format!("    return {}\n", expression(random, &names, 3));
    format!(
        "fn f{index}({}) -> {} {{PAD\n{body}}}\nstd::io::print(f{index}({}))\n",
        signature.join(", "),
        TYPES[random.next(TYPES.len())],
        arguments.join(", ")
    )
}

/// How a program ends and what it printed, for programs that compile.
fn outcome(source: &str) -> (String, String) {
    let program = compile(source).unwrap();
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..64 {
        if !matches!(session.run(1 << 16), Status::Paused) {
            break;
        }
    }
    // The padding shifts source positions, so a runtime failure is compared without its span.
    let status = format!("{:?}", session.status());
    let status = match status.find(", span: Span {") {
        Some(at) => status[..at].to_owned(),
        None => status,
    };
    (status, session.context.output.clone())
}

#[test]
fn generated_numeric_programs_agree_with_the_existing_route() {
    // `ROUTINE_FUZZ_CASES` and `ROUTINE_FUZZ_SEED` widen the search from the command line.
    let variable = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let cases = variable("ROUTINE_FUZZ_CASES", 400) as usize;
    let mut random = Random(variable("ROUTINE_FUZZ_SEED", 0x5eed));
    let (mut compiled, mut rejected, mut migrated_bodies) = (0, 0, 0);
    for index in 0..cases {
        let source = generated_function(&mut random, index);
        let (migrated, fallback) = variants(&source);
        let file =
            cyallo_parser::parse_modern_snapshot(migrated.as_str().into(), false, cyallo::Jobs(1))
                .unwrap();
        for handle in file.units().iter().flat_map(|unit| unit.bodies()) {
            if IndexedRoutineBody::prepare(&handle, 0).unwrap().is_some() {
                migrated_bodies += 1;
            }
        }
        let a = compile(&migrated);
        let b = compile(&fallback);
        match (a, b) {
            (Ok(_), Ok(_)) => {
                compiled += 1;
                assert_eq!(
                    outcome(&migrated),
                    outcome(&fallback),
                    "case {index} runs differently\n{migrated}"
                );
            }
            (Err(a), Err(b)) => {
                rejected += 1;
                assert_eq!(
                    failure(&a, a.span.source_text(&migrated)),
                    failure(&b, b.span.source_text(&fallback)),
                    "case {index}\n{migrated}"
                );
            }
            (a, b) => panic!(
                "case {index}: migrated {:?}, fallback {:?}\n{migrated}",
                a.map(|_| "ok"),
                b.map(|_| "ok")
            ),
        }
    }
    // The corpus must exercise both outcomes and the migrated route.
    assert!(compiled > 20, "only {compiled} programs compile");
    assert!(rejected > 20, "only {rejected} programs are rejected");
    assert!(
        migrated_bodies > 200,
        "only {migrated_bodies} bodies migrated"
    );
}
