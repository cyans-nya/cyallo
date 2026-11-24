use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

#[test]
fn multiline_literals_and_escapes() {
    output(
        "let a = [\n 1,\n 2,\n 3,\n]\nstd::io::print(\n std::array::sum(a),\n \"\\nhello\\twater 水\",\n)\n",
        "6\nhello\twater 水\n",
    );
}
#[test]
fn typed_arrays_ref_and_expression_constants() {
    output(
        "const factor = 2 + 3\nfn make(n: i64) -> std::collections::Vec<i64> { return Vec[n, n * factor] }\nproc add(n: &mut i64) { n.* += 1; return }\nlet mut a = make(2)\nadd(&mut a[0])\nstd::io::print(a[0], \":\", a[1])\n",
        "3:10\n",
    );
}
#[test]
fn loops_else_if_and_boolean_operators() {
    output(
        "let mut n = 0; let mut total = 0\nwhile n < 6 {\n n += 1\n if n == 2 { continue } else if n == 5 { break } else { total += n }\n}\nstd::io::print(total, true && !false, 7 % 3, 1 != 2)\n",
        "8true1true\n",
    );
}
#[test]
fn compound_target_is_evaluated_once() {
    output(
        "let mut calls = 0\nlet mut a = [10]\nproc index() -> i64 { calls += 1; return 0 }\na[index()] += 3\nstd::io::print(calls, \":\", a[0])\n",
        "1:13\n",
    );
}

#[test]
fn collection_push_evaluates_its_argument_before_growth() {
    output(
        "let mut a = Vec[1, 2]\na.push(std::array::len(a))\nstd::io::print(a[2], \":\", std::array::len(a))\nlet mut b = Vec[1, 2]\nb.push(std::array::sum(b))\nstd::io::print(b[2], \":\", std::array::sum(b))\n",
        "2:3\n3:6\n",
    );
}

#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn utilities() {
    output(
        "std::io::print(std::string::trim(\" 水 \"), std::string::upper(\"straße\"), std::string::lower(\"ABC\"), std::string::contains(\"water\",\"at\"), std::string::starts_with(\"water\",\"wa\"), std::string::ends_with(\"water\",\"er\"), std::string::replace(\"a-b-a\",\"a\",\"x\"))\nstd::io::print(std::num::abs(-3), std::num::min(2,5), std::num::max(2.5,5.0), std::num::clamp(20,0,10), std::num::clamp(5.0,0.0,3.5), std::num::round(1.6), std::num::floor(-1.2), std::num::ceil(1.2), std::num::sqrt(9.0), std::num::pow(2.0,3.0))\nlet a = Vec[5,3,1]\nstd::io::print(std::array::sum(a),std::array::len(a),a.len())\nlet mut values = Vec[10]\nvalues.push(30)\nstd::io::print(values.len(),values[1],std::array::sum(values))\n",
        "水STRASSEabctruetruetruex-b-x\n325103.52-2238\n933\n23040\n",
    );
}
#[test]
fn invalid_modern_programs() {
    for source in [
        "const n = 1\nn += 1\n",
        "const n = 1\nconst n = 2\n",
        "let a = [1,\"x\"]\n",
        "let a = []\n",
        "break\n",
        "continue\n",
        "let mut a = [1]\na[0] = \"x\"\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn invalid_utility_domains_match() {
    for source in [
        "std::io::print(std::char::from_code(-1))\n",
        "std::io::print(std::num::abs(-9223372036854775808))\n",
        "std::io::print(std::num::clamp(1,10,0))\n",
        "std::io::print(std::array::sum([9223372036854775807,1]))\n",
    ] {
        let p = Arc::new(compile(source).unwrap());
        let mut a = Session::new(p).unwrap();
        a.run(1000);
        assert!(matches!(a.status(), Status::Failed(_)));
    }
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn for_in_ranges_iterate_without_overflow_in_both_directions() {
    output(
        "let mut up = \"\"\nfor i in 0..4 { up = up + std::string::from(i) }\nlet mut down = \"\"\nfor i in (0..4).rev() { down = down + std::string::from(i) }\nlet mut closed = \"\"\nfor i in (1..=3).rev() { closed = closed + std::string::from(i) }\nstd::io::print(up, \":\", down, \":\", closed)\n",
        "0123:3210:321\n",
    );
    // Empty ranges skip the body; bounds at the integer limits never step past them.
    output(
        "let mut n = 0\nfor i in 5..5 { n += 1 }\nfor i in 5..=4 { n += 1 }\nfor i in (3..3).rev() { n += 1 }\nfor i in 9223372036854775806..=9223372036854775807 { n += 1 }\nfor i in (-9223372036854775807 - 1..=-9223372036854775807).rev() { n += 1 }\nstd::io::print(n)\n",
        "4\n",
    );
    output(
        "let mut total: u8 = 0u8\nfor i in 250u8..=255u8 { total = i }\nstd::io::print(total)\n",
        "255\n",
    );
    output(
        "let mut seen = 0\nfor i in 0..10 { if i == 2 { continue }\n if i == 5 { break }\n seen += i }\nstd::io::print(seen)\n",
        "8\n",
    );
    output(
        "let v = std::collections::Vec::from((1..=3).rev())\nstd::io::print(v[0], v[1], v[2])\n",
        "321\n",
    );
}
#[test]
fn for_in_rejects_non_ranges_and_mutating_its_variable() {
    for source in [
        "let a = [1, 2]\nfor i in a { std::io::print(i) }\n",
        "for i in 0.5..2.5 { std::io::print(i) }\n",
        "for i in 0..3 { i = 2 }\n",
        "for i = 0 to 3 { std::io::print(i) }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn casts_bind_looser_than_unary_operators() {
    output(
        "let x: i64 = -5\nlet y: f64 = 2.5\nstd::io::print(-x as u8, \":\", -5 as i32, \":\", -y as i64)\n",
        "5:-5:-2\n",
    );
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn strings_concatenate_with_plus() {
    output(
        "let mut s = \"a\" + \"b\"\ns += \"c\"\ns = s + 'd'\nstd::io::print(s)\n",
        "abcd\n",
    );
    for source in [
        "let s = \"a\" & \"b\"\n",
        "let c = 'a' + 'b'\n",
        "let x = 3\nlet ok = 1 < x < 5\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn numbers_follow_rust_like_rules() {
    // Integer `/` truncates; float arithmetic is IEEE; `%` keeps the dividend's sign.
    output(
        "let a = 7\nlet b = -2\nstd::io::print(a / b, \":\", a % b, \":\", -7 % 2, \":\", 7.5 % 2.0)\n",
        "-3:1:-1:1.5\n",
    );
    output(
        "let zero = 0.0\nstd::io::print(1.0 / zero, \":\", -1.0 / zero, \":\", std::num::sqrt(-1.0), \":\", 0.0 / zero == 0.0 / zero)\n",
        "inf:-inf:NaN:false\n",
    );
    output(
        "let x: f32 = 2.0\nlet y: u8 = 200\nstd::io::print(std::num::pow(x, 3.0), \":\", y / 3u8, \":\", std::num::max(y, 7u8))\n",
        "8:66:200\n",
    );
    for source in [
        "let a: f64 = 1\n",
        "let a = 2.5 * 2\n",
        "let a = 1\nlet b = 2.5\nlet c = a + b\n",
        "let a = 7 div 2\n",
        "let a = std::num::sqrt(4)\n",
        "let a: i64 = 1\nlet b: f64 = 2.0\nlet c = std::num::max(a, b)\n",
        "let a = 1u8 + 1u16\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
    // Integer division is checked; casts to integers stay checked.
    for source in [
        "let a = 0\nstd::io::print(1 / a)\n",
        "let a = -9223372036854775807 - 1\nstd::io::print(a / -1)\n",
        "let a = 1.0 / 0.0\nstd::io::print(a as i64)\n",
    ] {
        let p = Arc::new(compile(source).unwrap());
        let mut a = Session::new(p).unwrap();
        a.run(1000);
        assert!(matches!(a.status(), Status::Failed(_)), "{source}");
    }
}
#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn tuples_records_and_arrays_compare_structurally() {
    output(
        "record P { x: i64, y: string }\nlet p = P { x: 1, y: \"a\" }\nlet q = P { x: 1, y: \"a\" }\nlet r = P { x: 2, y: \"a\" }\nlet n1 = ((1, 2.5), [P { x: 1, y: \"a\" }])\nlet n2 = ((1, 2.5), [P { x: 1, y: \"b\" }])\nstd::io::print((1, \"x\") == (1, \"x\"), p == q, p == r, p != r, [1, 2] == [1, 2], n1 == n2)\n",
        "truetruefalsetruetruefalse\n",
    );
    for source in [
        "let a = gc 1\nlet b = gc 1\nlet same = a == b\n",
        "let a = (1, 2)\nlet b = (1, 2.0)\nlet same = a == b\n",
        "let a = (1, 2)\nlet less = a < (2, 3)\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
fn diagnostics_use_source_names_and_types() {
    let message = |source: &str| compile(source).unwrap_err().message;
    assert_eq!(
        message("let a = (1, 2)\nlet b = a < a\n"),
        "operator `<` cannot be applied to `(i64, i64)` and `(i64, i64)`"
    );
    let mismatch = message("let a: f64 = 1\n");
    assert!(
        mismatch
            .starts_with("mismatched types: expected `f64`, found `i64`; integers never convert"),
        "{mismatch}"
    );
    let unknown = message("let s = missing + 1\n");
    assert!(!unknown.contains('$'), "{unknown}");
    assert!(unknown.contains("`missing`"), "{unknown}");
    assert_eq!(
        message("let a = std::num::sqrt(4)\n"),
        "`i64` does not satisfy declared bound `Floating`"
    );
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
