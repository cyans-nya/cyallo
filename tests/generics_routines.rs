//! Generic routines through the HIR/MIR/AIR pipeline. A template is checked once, with its
//! type parameters opaque, as in Rust: the parameter says nothing about the instance, so a
//! body that needs to know more fails whatever the instance is. Instances over scalar types
//! are then lowered from the template's body.
use cyallo::{CompileOptions, Diagnostic, Session, Status, compile_with_options};
use std::sync::Arc;

fn options() -> CompileOptions {
    CompileOptions {
        jobs: 1,
        ..Default::default()
    }
}

fn run(source: &str) -> String {
    let program = compile_with_options(source, options())
        .unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..4096 {
        match session.run(1 << 16) {
            Status::Finished => break,
            Status::Paused => {}
            other => panic!("unexpected status {other:?}\n{source}"),
        }
    }
    session.context.output
}

fn failure(source: &str) -> Diagnostic {
    compile_with_options(source, options()).expect_err(source)
}

#[test]
fn a_template_that_only_moves_its_values_is_instantiated() {
    let source = "fn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(1, 2))\nstd::io::print(first(\"x\", \"y\"))\nstd::io::print(first(true, false))";
    assert_eq!(run(source), "1\nx\ntrue\n");
}

#[test]
fn a_parameter_gives_the_body_no_information_about_its_type() {
    // Arithmetic needs a bound, and there is none: the template fails even for an instance
    // where `T` is a number.
    let error = failure("fn double<T>(a: T) -> T { return a + a }\nstd::io::print(double(2))");
    assert!(
        matches!(error.code, "E105" | "E118" | "E109"),
        "{} {}",
        error.code,
        error.message
    );
}

#[test]
fn matching_a_parameter_fails_even_when_the_instance_is_an_enum() {
    let source = "enum Foo {\n    Bar: i64,\n    Baz,\n}\n\
        fn first<T>(val: T) -> i64 {\n    return match val {\n        Foo::Bar(a) => a,\n        _ => 0,\n    }\n}\n\
        std::io::print(first(Foo::Bar(1)))";
    let error = failure(source);
    assert_eq!(error.code, "E185", "{}", error.message);
}

#[test]
fn an_unused_template_is_still_checked() {
    let error = failure("fn bad<T>(a: T) -> T { return a + a }\nstd::io::print(1)");
    assert!(
        matches!(error.code, "E105" | "E118" | "E109"),
        "{} {}",
        error.code,
        error.message
    );
}

#[test]
fn a_value_of_an_opaque_type_is_moved_not_copied() {
    let error = failure(
        "fn twice<T>(a: T) -> T {\n    let b = a\n    return a\n}\nstd::io::print(twice(1))",
    );
    assert_eq!(error.code, "E118");
}

fn lowered(source: &str) -> usize {
    let before = cyallo_ir::analysis::bodies_emitted_on_this_thread();
    compile_with_options(source, options()).unwrap();
    cyallo_ir::analysis::bodies_emitted_on_this_thread() - before
}

#[test]
fn instances_over_scalars_are_lowered_from_the_template_body() {
    // One instance per type argument, and the caller is the entry script (not lowered here).
    let source = "fn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(1, 2))\nstd::io::print(first(\"x\", \"y\"))\nstd::io::print(first(true, false))";
    assert_eq!(lowered(source), 3);
}

#[test]
fn an_instance_may_call_a_plain_routine_with_its_values() {
    let source = "fn keep(n: i64) -> i64 { return n }\n\
        fn through<T>(a: T, n: i64) -> i64 { return keep(n) }\n\
        std::io::print(through(\"s\", 4))\nstd::io::print(through(true, 5))";
    assert_eq!(run(source), "4\n5\n");
    assert_eq!(lowered(source), 3);
}

#[test]
fn an_instance_over_a_record_keeps_the_existing_route() {
    let source = "record P {\n    x: i64,\n}\nfn first<T>(a: T, b: T) -> T { return a }\n\
        std::io::print(first(P { x: 1 }, P { x: 2 }).x)";
    assert_eq!(run(source), "1\n");
}

#[test]
fn a_numeric_bound_gives_a_parameter_arithmetic_and_comparison() {
    let source = "fn add<T: std::num::Numeric>(a: T, b: T) -> T { return a + b }\n\
        fn bigger<T: std::num::Numeric>(a: T, b: T) -> bool { return a > b }\n\
        std::io::print(add(1, 2))\nstd::io::print(add(1.5, 2.0))\nstd::io::print(bigger(3, 2))";
    assert_eq!(run(source), "3\n3.5\ntrue\n");
    assert!(lowered(source) >= 3);
}

#[test]
fn a_numeric_parameter_is_copy_so_it_may_be_used_twice() {
    let source = "fn twice<T: std::num::Numeric>(a: T) -> T {\n    let b = a\n    return a + b\n}\nstd::io::print(twice(4))";
    assert_eq!(run(source), "8\n");
}

#[test]
fn a_bound_gives_nothing_beyond_its_own_capabilities() {
    // A literal is an `i64`, not a `T`.
    let error =
        failure("fn inc<T: std::num::Numeric>(a: T) -> T { return a + 1 }\nstd::io::print(inc(1))");
    assert_eq!(error.code, "E105", "{}", error.message);
    // `Numeric` is not `Format`.
    let error = failure("proc show<T: std::num::Numeric>(a: T) { std::io::print(a) }\nshow(1)");
    assert_eq!(error.code, "E112", "{}", error.message);
}

#[test]
fn format_allows_print_and_a_string_satisfies_it() {
    let source = "proc show<T: std::fmt::Format>(a: T) { std::io::print(a) }\nshow(7)\nshow(\"s\")\nshow(true)";
    assert_eq!(run(source), "7\ns\ntrue\n");
}

#[test]
fn a_type_argument_must_satisfy_every_bound() {
    let error = failure(
        "fn add<T: std::num::Numeric>(a: T, b: T) -> T { return a + b }\nstd::io::print(add(\"a\", \"b\"))",
    );
    assert_eq!(error.code, "E151");
    assert!(error.message.contains("Numeric"), "{}", error.message);
    let error = failure(
        "fn half<T: std::num::Integer>(a: T) -> T { return a / a }\nstd::io::print(half(1.5))",
    );
    assert_eq!(error.code, "E151");
}

#[test]
fn a_bound_that_is_not_a_trait_the_language_gives_meaning_to_is_still_rejected() {
    // A record is not a trait, and `Cloneable` is a trait the pipeline has no meaning for.
    for source in [
        "record R {\n    n: i64\n}\nfn f<T: R>(a: T) -> T { return a }\nstd::io::print(1)",
        "fn f<T: std::mem::Cloneable>(a: T) -> T { return a }\nstd::io::print(1)",
        "trait Pair<T> {}\nfn f<U: Pair<i64>>(a: U) -> U { return a }\nstd::io::print(1)",
    ] {
        let error = failure(source);
        assert_eq!(error.code, "E173", "{source}: {}", error.message);
    }
}

#[test]
fn each_operator_is_its_own_capability() {
    let ok =
        "fn sum<T: std::ops::Add>(a: T, b: T) -> T { return a + b }\nstd::io::print(sum(2, 3))";
    assert_eq!(run(ok), "5\n");
    // `Add` gives `+` and nothing else.
    let error = failure(
        "fn diff<T: std::ops::Add>(a: T, b: T) -> T { return a - b }\nstd::io::print(diff(2, 3))",
    );
    assert_eq!(error.code, "E105", "{}", error.message);
    let error =
        failure("fn flip<T: std::ops::Add>(a: T) -> T { return -a }\nstd::io::print(flip(2))");
    assert_eq!(error.code, "E105", "{}", error.message);
}

#[test]
fn comparison_is_eq_and_ord_and_neither_is_copy() {
    let ok = "fn same<T: std::cmp::Eq>(a: T, b: T) -> bool { return a == b }\n\
        fn below<T: std::cmp::Ord>(a: T, b: T) -> bool { return a < b }\n\
        std::io::print(same(1, 1))\nstd::io::print(same(\"a\", \"b\"))\nstd::io::print(below(1, 2))";
    assert_eq!(run(ok), "true\nfalse\ntrue\n");
    let error = failure(
        "fn below<T: std::cmp::Eq>(a: T, b: T) -> bool { return a < b }\nstd::io::print(below(1, 2))",
    );
    assert_eq!(error.code, "E105", "{}", error.message);
    // Without `Copy` a value is moved: `Eq` alone does not give it.
    let error = failure(
        "fn twice<T: std::cmp::Eq>(a: T) -> T {\n    let b = a\n    return a\n}\nstd::io::print(twice(1))",
    );
    assert_eq!(error.code, "E118");
    let ok = "fn twice<T: std::mem::Copy>(a: T) -> T {\n    let b = a\n    return a\n}\nstd::io::print(twice(4))";
    assert_eq!(run(ok), "4\n");
}

#[test]
fn neg_is_not_satisfied_by_an_unsigned_type() {
    let ok = "fn flip<T: std::ops::Neg>(a: T) -> T { return -a }\nstd::io::print(flip(2))\nstd::io::print(flip(1.5))";
    assert_eq!(run(ok), "-2\n-1.5\n");
    let error = failure(
        "fn flip<T: std::ops::Neg>(a: T) -> T { return -a }\nfn go(x: u8) -> u8 { return flip(x) }\nstd::io::print(go(1))",
    );
    assert_eq!(error.code, "E151");
    assert!(error.message.contains("Neg"), "{}", error.message);
}

#[test]
fn several_bounds_add_their_capabilities() {
    let source = "proc show_sum<T: std::ops::Add + std::fmt::Format>(a: T, b: T) { std::io::print(a + b) }\nshow_sum(1, 2)\nshow_sum(1.5, 1.0)";
    assert_eq!(run(source), "3\n2.5\n");
}

#[test]
fn debug_is_a_bound_with_the_same_types_as_format() {
    let ok = "fn keep<T: std::fmt::Debug>(a: T) -> T { return a }\nstd::io::print(keep(3))";
    assert_eq!(run(ok), "3\n");
    // It gives no way to print yet, and does not stand in for `Format`.
    let error = failure("proc show<T: std::fmt::Debug>(a: T) { std::io::print(a) }\nshow(1)");
    assert_eq!(error.code, "E112", "{}", error.message);
}

const SHAPES: &str = "trait Shape {\n    proc area(self: &Self) -> i64\n}\n\
record Circle {\n    r: i64\n}\nrecord Square {\n    side: i64\n}\nrecord Blob {\n    n: i64\n}\n\
impl Shape for Circle {\n    proc area(self: &Self) -> i64 {\n        return self.r * self.r * 3\n    }\n}\n\
impl Shape for Square {\n    proc area(self: &Self) -> i64 {\n        return self.side * self.side\n    }\n}\n";

fn shapes(rest: &str) -> String {
    format!("{SHAPES}{rest}")
}

#[test]
fn a_user_trait_bound_gives_its_methods_and_each_instance_calls_its_own_impl() {
    let source = shapes(
        "proc double_area<T: Shape>(shape: &T) -> i64 {\n    return shape.area() * 2\n}\n\
         let c = Circle { r: 2 }\nlet s = Square { side: 5 }\n\
         std::io::print(double_area(&c))\nstd::io::print(double_area(&s))",
    );
    assert_eq!(run(&source), "24\n50\n");
    // One instance per type argument, lowered from the template's body.
    // (The `impl` methods take a receiver, which the pipeline does not lower itself.)
    assert_eq!(lowered(&source), 2);
}

#[test]
fn a_method_outside_the_bounds_is_unresolved() {
    let error = failure(&shapes(
        "proc f<T: Shape>(shape: &T) -> i64 {\n    return shape.perimeter()\n}\nlet c = Circle { r: 1 }\nstd::io::print(f(&c))",
    ));
    assert_eq!(error.code, "E106", "{}", error.message);
    // With no bound at all a parameter has no methods, even where every instance has one.
    let error = failure(&shapes(
        "proc g<T>(shape: &T) -> i64 {\n    return shape.area()\n}\nlet c = Circle { r: 1 }\nstd::io::print(g(&c))",
    ));
    assert_eq!(error.code, "E106", "{}", error.message);
}

#[test]
fn a_type_argument_must_implement_the_trait() {
    let error = failure(&shapes(
        "proc f<T: Shape>(shape: &T) -> i64 {\n    return shape.area()\n}\nlet b = Blob { n: 1 }\nstd::io::print(f(&b))",
    ));
    assert_eq!(error.code, "E151", "{}", error.message);
}

#[test]
fn a_method_taking_a_mutable_receiver_needs_a_mutable_reference() {
    let grow = "trait Grow {\n    proc grow(self: &mut Self, by: i64)\n    proc size(self: &Self) -> i64\n}\n\
record Pile {\n    n: i64\n}\n\
impl Grow for Pile {\n    proc grow(self: &mut Self, by: i64) {\n        self.n += by\n    }\n    proc size(self: &Self) -> i64 {\n        return self.n\n    }\n}\n";
    let ok = format!(
        "{grow}proc bump<T: Grow>(pile: &mut T) -> i64 {{\n    pile.grow(4)\n    pile.grow(1)\n    return pile.size()\n}}\n\
         let mut p = Pile {{ n: 10 }}\nstd::io::print(bump(&mut p))\nstd::io::print(p.n)"
    );
    assert_eq!(run(&ok), "15\n15\n");
    let error = failure(&format!(
        "{grow}proc bump<T: Grow>(pile: &T) {{\n    pile.grow(4)\n}}\nlet mut p = Pile {{ n: 1 }}\nbump(&p)"
    ));
    assert_eq!(error.code, "E107", "{}", error.message);
}

#[test]
fn a_trait_bound_combines_with_the_operator_bounds() {
    let source = shapes(
        "proc total<T: Shape>(a: &T, b: &T) -> i64 {\n    return a.area() + b.area()\n}\n\
         let x = Square { side: 2 }\nlet y = Square { side: 3 }\nstd::io::print(total(&x, &y))",
    );
    assert_eq!(run(&source), "13\n");
}
