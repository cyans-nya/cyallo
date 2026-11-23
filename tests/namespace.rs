use cyallo::{CompileOptions, Dialect, compile, compile_with_options, vm::{Context, Session, Status}};
use std::sync::Arc;

#[test]
fn standard_paths_and_import_aliases() {
    output(
        "use std::num::{abs as magnitude, max}\nuse std::string as text\nlet n = magnitude(-4)\nio::print(n, max(1,2), text::upper(\"ok\"))\n",
        "42OK\n",
    );
}
#[test]
fn prelude_is_a_normal_glob_import() {
    output(
        "use std::prelude::*\nlet value = num::abs(-3)\nio::print(value)\n",
        "3\n",
    );
}
#[test]
fn lexical_bindings_are_case_sensitive_and_mutability_is_explicit() {
    output(
        "let x = 1\nlet X = 2\nlet mut total = 0\nif true { let x = 3; total = x + X }\nio::print(x,total)\n",
        "15\n",
    );
    assert!(compile("let n = 1\nn = 2\n").is_err());
    assert!(compile("n = 1\n").is_err());
    assert!(compile("let n = abs(-1)\n").is_err());
}

#[test]
fn import_globs_reach_a_fixed_point() {
    output(
        r#"
mod last { pub use crate::middle::*; }
mod middle { pub use crate::first::*; }
mod first { pub const ANSWER = 42 }
use crate::last::*;
std::io::print(ANSWER)
"#,
        "42\n",
    );
}
#[test]
fn callable_identity_survives_aliases_and_shadowing() {
    output(
        r#"
use std::num::abs as magnitude
fn abs(n: i64) -> i64 { return n + 100 }
fn work() -> i64 {
    let magnitude = fn(n: i64) -> i64 { return n + 200 }
    return magnitude(3)
}
std::io::print(abs(3), ":", magnitude(-4), ":", work())
"#,
        "103:4:203\n",
    );
    for name in ["abs", "print", "empty", "trim", "parse", "input"] {
        assert!(compile(&format!("let callable = {name}\n")).is_err());
    }
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
