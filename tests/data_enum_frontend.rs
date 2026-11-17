//! Declarations survive source resolution, including when not instantiated.
use cyallo::{CompileOptions, compile, compile_for_execution};
#[test]
fn unused_enum_and_structural_declarations_are_checked_in_both_modes() {
    for source in [
        "enum E { A, B: i64 }\n",
        "mod nested { pub enum E { A } }\n",
        "let value = record { x: 1 }\n",
        "type R = record { x: i64 }\n",
    ] {
        compile(source).unwrap();
        compile_for_execution(source, CompileOptions::default()).unwrap();
    }
}
