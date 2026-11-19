//! Standard-library intrinsics in the verified interpreter. Expected text is
//! written by hand from the language contract.

use cyallo::{
    bytecode::verified::{ExecutionLimits, OutputSink, RunError, VerifiedModule},
    compile,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 500_000_000,
    frames: 256,
};

fn run(source: &str) -> Result<Vec<String>, RunError> {
    let program = compile(source).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap_or_else(|error| panic!("{error}"));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    module.run_to(&[], LIMITS, writer)?;
    let lines = lines.lock().unwrap().clone();
    Ok(lines)
}

#[test]
fn string_transformations_and_tests_follow_unicode_text() {
    let source = r#"
let padded = "  Ada Lovelace \t"
std::io::print("[", std::string::trim(padded), "]")
std::io::print(std::string::upper("straße ñ"), " ", std::string::lower("ÀBC"))
std::io::print(std::string::contains("lovelace", "love"), " ", std::string::starts_with("lovelace", "love"), " ", std::string::ends_with("lovelace", "ace"), " ", std::string::contains("ada", "z"))
std::io::print(std::string::replace("a-b-c", "-", "+"), " ", std::string::replace("aaa", "a", "bb"), " ", std::string::replace("abc", "z", "y"))
"#;
    assert_eq!(
        run(source).unwrap(),
        [
            "[Ada Lovelace]",
            "STRASSE Ñ àbc",
            "true true true false",
            "a+b+c bbbbbb abc"
        ]
    );
}

#[test]
fn values_are_spelled_as_text_the_way_print_writes_them() {
    let source = r#"
std::io::print(std::string::from(42), "|", std::string::from(-7), "|", std::string::from(2.5), "|", std::string::from(true), "|", std::string::from("same"))
"#;
    assert_eq!(run(source).unwrap(), ["42|-7|2.5|true|same"]);
}

#[test]
fn integer_helpers_check_their_edge() {
    let source = r#"
std::io::print(std::num::abs(-5), " ", std::num::abs(5), " ", std::num::min(3, -2), " ", std::num::max(3, -2))
"#;
    assert_eq!(run(source).unwrap(), ["5 5 -2 3"]);
    let overflow =
        "let minimum = -9223372036854775807 - 1\nstd::io::print(std::num::abs(minimum))\n";
    assert!(matches!(
        run(overflow),
        Err(RunError::IntegerOverflow { .. })
    ));
}

#[test]
fn parsing_accepts_exactly_decimal_integers_in_range() {
    let source = r#"
proc show(text: string) {
    let parsed = std::num::parse<i64>(text)
    if parsed.is_ok() { std::io::print("ok ", parsed.unwrap()) } else { std::io::print("err") }
}
show("42")
show("-17")
show("+5")
show(" 1")
show("1 ")
show("")
show("12.5")
show("9223372036854775807")
show("9223372036854775808")
"#;
    assert_eq!(
        run(source).unwrap(),
        [
            "ok 42",
            "ok -17",
            "ok 5",
            "err",
            "err",
            "err",
            "err",
            "ok 9223372036854775807",
            "err"
        ]
    );
}

#[test]
fn counts_convert_to_and_from_integers_with_checks() {
    let source = r#"
let v = std::collections::Vec[1, 2, 3]
let n = v.len() as i64
let back = n as usize
std::io::print(n + 1, " ", back)
"#;
    assert_eq!(run(source).unwrap(), ["4 3"]);
    let negative = "let n = -1\nlet c = n as usize\nstd::io::print(c)\n";
    assert!(matches!(
        run(negative),
        Err(RunError::ConversionOutOfRange { .. })
    ));
}

#[test]
fn cloning_copies_plain_values_and_adds_an_owner_to_counted_ones() {
    let source = r#"
let original = rc 5
let copy = std::mem::clone(&original)
std::io::print(std::rc::strong_count(&original), " ", copy.*)
std::mem::drop(copy)
std::io::print(std::rc::strong_count(&original))
let words = std::collections::Vec["a", "b"]
let again = std::mem::clone(&words)
std::io::print(again.len(), " ", again[1])
std::mem::drop(original)
"#;
    assert_eq!(run(source).unwrap(), ["2 5", "1", "2 b"]);
}

#[test]
fn a_reference_to_an_element_or_field_borrows_its_owner() {
    let source = r#"
record Pair { left: string
 right: string }
fn shout(text: &string) -> string { return std::string::upper(text.*) }
let names = ["ada", "grace"]
let pair = Pair { left: "x", right: "y" }
std::io::print(shout(&names[1]), " ", shout(&pair.right), " ", shout(&names[0]))
"#;
    assert_eq!(run(source).unwrap(), ["GRACE Y ADA"]);
}

#[test]
fn the_documented_text_and_result_examples_run() {
    for (name, source) in [
        ("parser", include_str!("../examples/parser.ya")),
        ("modern", include_str!("../examples/modern.ya")),
        ("generics", include_str!("../examples/generics.ya")),
        ("patterns", include_str!("../examples/enums/patterns.ya")),
        ("result", include_str!("../examples/enums/result.ya")),
    ] {
        let lines = run(source).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!lines.is_empty(), "{name}");
    }
}
