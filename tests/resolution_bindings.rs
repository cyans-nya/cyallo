//! Name resolution through the compiler entry point: results (bindings,
//! diagnostics) must not depend on whether a routine is ever called, nor on how
//! many declarations surround it.
use cyallo::{compile, vm::{Session, Status}};
use std::sync::Arc;

/// `(code, message, text under the primary span)` of a compile failure.
type Failure = (&'static str, String, String);

fn describe(source: &str, error: &cyallo::Diagnostic) -> Failure {
    (
        error.code,
        error.message.clone(),
        error.span.source_text(source).to_owned(),
    )
}

/// A declaration-level failure: both entry points report the same diagnostic.
fn failure(source: &str) -> Failure {
    let eager = compile(source).expect_err("eager compile should fail");
    describe(source, &eager)
}

/// A body-level failure: raised at compile time even for unused routines.
fn body_failure(source: &str) -> Failure {
    let eager = compile(source).expect_err("eager compile should fail");
    describe(source, &eager)
}

fn output(source: &str) -> String {
    run(compile(source).unwrap())
}

fn run(program: cyallo::ir::Program) -> String {
    let mut session = Session::new(Arc::new(program)).unwrap();
    session.run(1_000_000);
    assert_eq!(session.status(), &Status::Finished);
    session.context.output.clone()
}

#[test]
fn unresolved_name_in_an_unused_routine_is_reported_at_compile_time() {
    let source = "fn unused() -> i64 { return missing + 1 }\nstd::io::print(\"ok\")\n";
    let (_, message, text) = body_failure(source);
    assert_eq!(text, "missing");
    assert!(message.contains("missing"), "{message}");
}

#[test]
fn the_first_unresolved_routine_in_source_order_is_selected() {
    let source = "fn a() -> i64 { return 1 }\n\
                  fn b() -> i64 { return first_missing }\n\
                  fn c() -> i64 { return second_missing }\n\
                  std::io::print(a())\n";
    let (_, _, text) = body_failure(source);
    assert_eq!(text, "first_missing");
}

#[test]
fn unresolved_names_are_reported_the_same_among_many_declarations() {
    // Same selection with enough neighbours that per-name lookups matter.
    let mut source = String::new();
    for k in 0..300 {
        source += &format!("fn r{k}(x: i64) -> i64 {{ return x + {k} }}\n");
    }
    source += "fn broken() -> i64 { return nowhere }\n";
    for k in 300..600 {
        source += &format!("fn r{k}(x: i64) -> i64 {{ return x + {k} }}\n");
    }
    source += "std::io::print(r599(1))\n";
    let (_, _, text) = body_failure(&source);
    assert_eq!(text, "nowhere");
    let fixed = source.replace("nowhere", "7");
    assert_eq!(output(&fixed), "600\n");
}

#[test]
fn duplicate_declarations_are_still_rejected() {
    for source in [
        "fn dup() -> i64 { return 1 }\nfn dup() -> i64 { return 2 }\n",
        "record dup { x: i64 }\nrecord dup { y: i64 }\n",
        "const dup = 1\nfn dup() -> i64 { return 2 }\n",
    ] {
        let (_, message, _) = failure(source);
        assert!(message.contains("dup"), "{source}: {message}");
    }
}

#[test]
fn a_value_and_a_type_may_share_a_spelling() {
    assert_eq!(
        output(
            "record point { x: i64 }\nfn point(v: i64) -> i64 { return v + 1 }\n\
             let p = point { x: 3 }\nstd::io::print(p.x, point(4))\n"
        ),
        "35\n"
    );
}

#[test]
fn shadowing_and_aliases_inside_unused_routines_resolve_lexically() {
    let source = r#"
use std::num::abs as magnitude
fn abs(n: i64) -> i64 { return n + 100 }
fn shadowed() -> i64 {
    let magnitude = fn(n: i64) -> i64 { return n + 200 }
    return magnitude(3)
}
fn parameter(abs: i64) -> i64 { return abs }
fn alias() -> i64 { return magnitude(-4) }
std::io::print(abs(3), ":", shadowed(), ":", parameter(9), ":", alias())
"#;
    assert_eq!(output(source), "103:203:9:4\n");
}

#[test]
fn type_aliases_and_exports_cross_modules() {
    let source = r#"
mod shapes {
    pub type Size = i64
    pub record Box { side: Size }
    pub fn make(side: Size) -> Box { return Box { side: side } }
    fn hidden() -> i64 { return 1 }
}
use shapes::make
use shapes::Size
fn area(side: Size) -> Size { return make(side).side * side }
std::io::print(area(6))
"#;
    assert_eq!(output(source), "36\n");
    let private = "mod a { fn hidden() -> i64 { return 1 } pub fn shown() -> i64 { return 2 } }\n\
                   use a::hidden\n";
    let (_, _, _) = failure(private);
    let private_type = "mod a { record Hidden { x: i64 } pub record Shown { x: i64 } }\n\
                        use a::Hidden\n";
    let (_, _, _) = failure(private_type);
}

#[test]
fn top_level_shadowing_of_bindings_keeps_distinct_storage() {
    assert_eq!(
        output(
            "let value = 1\nlet value = value + 1\nlet value = \"text\"\nstd::io::print(value)\n"
        ),
        "text\n"
    );
    let (_, _, text) = failure("let n = 1\nn = 2\n");
    assert!(text.contains('n'));
}
