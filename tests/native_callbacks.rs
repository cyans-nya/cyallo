use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

fn parity(source: &str, expected: &str) {
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


#[test]
fn annotated_callbacks_keep_native_alias_identity_and_user_shadowing() {
    parity(
        r#"
use std::num::abs as magnitude
fn abs(value: i64) -> i64 { return value + 100 }
fn choose() -> fn(i64) -> i64 { return magnitude }
let native = magnitude
let callback: fn(i64) -> i64 = native
let callbacks: [2]fn(i64) -> i64 = [magnitude, magnitude]
std::io::print(callback(-42), ":", choose()(-7), ":", callbacks[1](-8), ":", abs(1), ":", native(-9))
"#,
        "42:7:8:101:9\n",
    );
}

#[test]
fn generic_callbacks_infer_inputs_before_outputs_in_either_argument_order() {
    parity(
        r#"
fn apply<I, O>(value: I, callback: fn(I) -> O) -> O { return callback(value) }
fn apply_first<I, O>(callback: fn(I) -> O, value: I) -> O { return callback(value) }
std::io::print(apply(-42, std::num::abs), ":", apply_first(std::num::abs, -7))
std::io::print(apply("abcd", std::string::len))
"#,
        "42:7\n4\n",
    );
}

#[test]
#[ignore = "verified lowering: function-typed local with Result/enum payload (R900)"]
fn return_only_native_generics_are_determined_by_the_callback_signature() {
    parity(
        r#"
let parse_byte: fn(string) -> Result<u8, std::num::ParseError> = std::num::parse
let empty: fn() -> [0]i64 = std::array::empty
std::io::print(parse_byte("42").unwrap(), ":", std::array::len(empty()))
"#,
        "42:0\n",
    );
}

#[test]
fn descriptor_factory_is_evaluated_once_before_ordinary_callback_use() {
    parity(
        r#"
proc factory<T>(value: T) -> T { std::io::print("factory"); return value }
let callback: fn(i64) -> i64 = factory(std::num::abs)
std::io::print(callback(-7), ":", callback(-8))
"#,
        "factory\n7:8\n",
    );
}

#[test]
fn pure_functions_widen_to_procedures_and_print_packs_remain_typed() {
    parity(
        r#"
proc apply(value: i64, callback: proc(i64) -> i64) -> i64 { return callback(value) }
let emit: proc(string, i64) = std::io::print
emit("answer:", apply(-42, std::num::abs))
"#,
        "answer:42\n",
    );
}

#[test]
fn capture_free_native_callback_can_spawn_and_join_on_one_processor() {
    parity(
        r#"
let worker = std::task::spawn(std::io::print)
worker.join().completed()
std::io::print("joined")
"#,
        "\njoined\n",
    );
}

#[test]
fn incompatible_native_callbacks_are_rejected_before_execution() {
    for (source, reason) in [
        ("let bad: fn(string) -> () = std::io::print\n", "effects"),
        (
            "let bad: fn(bool) -> bool = std::num::abs\n",
            "declared bound",
        ),
        (
            "let bad: fn(std::array::Range<i64>) -> [3]i64 = std::array::from\n",
            "compile-time integer bounds",
        ),
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, "E177", "{source}: {error}");
        assert!(error.message.contains(reason), "{source}: {error}");
    }
}
