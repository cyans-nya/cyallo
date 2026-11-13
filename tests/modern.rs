use cyallo::{
    CompileOptions, Dialect, Session, Status,
    backend::{CraneliftBackend, Executable, NativeBackend},
    compile, compile_with_options,
};
use std::sync::Arc;
fn output(source: &str, expected: &str) {
    let p = Arc::new(compile(source).unwrap());
    let mut a = Session::new(p.clone()).unwrap();
    let mut b = Session::new(p.clone()).unwrap();
    a.run(2_000_000);
    CraneliftBackend
        .compile(p)
        .unwrap()
        .run(&mut b, 2_000_000)
        .unwrap();
    assert_eq!(a.status(), b.status());
    assert_eq!(a.context.output, b.context.output);
    assert_eq!(a.status(), &Status::Finished);
    assert_eq!(a.context.output, expected);
}
#[test]
fn modern_example() {
    output(
        include_str!("../examples/modern.ya"),
        "ADA\n288212784234102776:10:3\n",
    );
}
#[test]
fn algorithm_examples() {
    output(
        include_str!("../examples/quicksort.ya"),
        "513509887:1072:998045\n",
    );
    output(
        include_str!("../examples/mergesort.ya"),
        "398403790:1921:999272\n",
    );
    output(include_str!("../examples/fib.ya"), "17711\n");
    output(include_str!("../examples/knapsack.ya"), "1054\n3400\n");
    let dfs = include_str!("../examples/dfs.ya").replace("let n = 300", "let n = 50");
    output(&dfs, "50:0:50\n");
    let parser = include_str!("../examples/parser.ya").replace("let reps = 200", "let reps = 2");
    output(&parser, "416:12:330464\n");
}
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
        "const factor = 2 + 3\nfn make(n: i64) -> std::collections::Vec<i64> { return Vec[n, n * factor] }\nproc add(n: &mut i64) { *n += 1; return }\nlet mut a = make(2)\nadd(&mut a[0])\nstd::io::print(a[0], \":\", a[1])\n",
        "3:10\n",
    );
}
#[test]
fn loops_else_if_and_boolean_operators() {
    output(
        "let mut n = 0; let mut total = 0\nwhile n < 6 {\n n += 1\n if n == 2 { continue } else if n == 5 { break } else { total += n }\n}\nstd::io::print(total, true && !false, 7 % 3, 1 != 2)\n",
        "8TRUE1TRUE\n",
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
fn utilities() {
    output(
        "std::io::print(std::string::trim(\" 水 \"), std::string::upper(\"straße\"), std::string::lower(\"ABC\"), std::string::contains(\"water\",\"at\"), std::string::starts_with(\"water\",\"wa\"), std::string::ends_with(\"water\",\"er\"), std::string::replace(\"a-b-a\",\"a\",\"x\"))\nstd::io::print(std::num::abs(-3), std::num::min(2,5), std::num::max(2.5,5), std::num::clamp(20,0,10), std::num::clamp(5,0,3.5), std::num::round(1.6), std::num::floor(-1.2), std::num::ceil(1.2), std::num::sqrt(9), std::num::pow(2,3))\nlet a = Vec[5,3,1]\nstd::io::print(std::array::sum(a),std::array::len(a),a.len())\nlet mut values = Vec[10]\nvalues.push(30)\nstd::io::print(values.len(),values[1],std::array::sum(values))\n",
        "水STRASSEabcTRUETRUETRUEx-b-x\n325103.52-2238\n933\n23040\n",
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
fn modern_rejects_caie_syntax_in_all_compilation_modes() {
    for source in [
        "DECLARE n : INTEGER\n",
        "CONSTANT n = 1\n",
        "OUTPUT 1\n",
        "let n = 0\nINPUT n\n",
        "n <- 1\n",
        "n ← 1\n",
        "if true THEN\nstd::io::print(1)\nENDIF\n",
        "while true\nstd::io::print(1)\nENDWHILE\n",
        "FOR i <- 1 TO 3\nstd::io::print(i)\nNEXT i\n",
        "REPEAT\nstd::io::print(1)\nUNTIL true\n",
        "CASE OF 1\n1 : std::io::print(1)\nENDCASE\n",
        "FUNCTION f() RETURNS INTEGER\nRETURN 1\nENDFUNCTION\n",
        "PROCEDURE p()\nENDPROCEDURE\n",
        "proc p() {}\nCALL p()\n",
        "proc p() { OUTPUT 1 }\n",
        "fn f() -> i64 { return 1 = 1 }\n",
        "std::io::print(1 <> 2)\n",
        "std::io::print(true AND false)\n",
        "std::io::print(true OR false)\n",
        "std::io::print(NOT false)\n",
        "std::io::print(7 MOD 3)\n",
        "let n: INTEGER = 1\n",
        "let b: BOOLEAN = true\n",
        "let a: ARRAY[1:2] OF INTEGER = [1,2]\n",
        "proc p(BYREF n: i64) {}\n",
    ] {
        for jobs in [1, 4] {
            let options = CompileOptions {
                jobs,
                ..Default::default()
            };
            assert!(
                compile_with_options(source, options).is_err(),
                "accepted {source}"
            );
            assert!(
                cyallo::compile_for_execution(source, options).is_err(),
                "deferred accepted {source}"
            );
            assert!(
                cyallo::repl::Repl::new(options)
                    .unwrap()
                    .push_line(source)
                    .is_err(),
                "REPL accepted {source}"
            );
        }
    }
}
#[test]
fn compatibility_rejects_modern_syntax_and_utilities() {
    for source in [
        "let n = 1\n",
        "OUTPUT len(\"abc\")\n",
        "IF TRUE { OUTPUT 1 }\n",
        "OUTPUT 1 == 1\n",
        "OUTPUT trim(\"a\")\n",
    ] {
        assert!(
            compile_with_options(
                source,
                CompileOptions {
                    dialect: Dialect::Compatibility,
                    ..Default::default()
                }
            )
            .is_err(),
            "accepted {source}"
        );
    }
    assert!(
        compile_with_options(
            "DECLARE n : INTEGER\nn <- 1\nOUTPUT n\n",
            CompileOptions {
                dialect: Dialect::Compatibility,
                ..Default::default()
            }
        )
        .is_ok()
    );
}
#[test]
fn invalid_utility_domains_match() {
    for source in [
        "std::io::print(std::num::sqrt(-1))\n",
        "std::io::print(std::num::abs(-9223372036854775808))\n",
        "std::io::print(std::num::clamp(1,10,0))\n",
        "std::io::print(std::array::sum([9223372036854775807,1]))\n",
    ] {
        let p = Arc::new(compile(source).unwrap());
        let mut a = Session::new(p.clone()).unwrap();
        let mut b = Session::new(p.clone()).unwrap();
        a.run(1000);
        CraneliftBackend
            .compile(p)
            .unwrap()
            .run(&mut b, 1000)
            .unwrap();
        assert!(matches!(a.status(), Status::Failed(_)));
        assert_eq!(a.status(), b.status());
    }
}
#[test]
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
fn casts_bind_looser_than_unary_operators() {
    output(
        "let x: i64 = -5\nlet y: f64 = 2.5\nstd::io::print(-x as u8, \":\", -5 as i32, \":\", -y as i64)\n",
        "5:-5:-2\n",
    );
}
#[test]
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
