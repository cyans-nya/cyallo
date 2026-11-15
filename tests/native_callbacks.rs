use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn parity(source: &str, expected: &str) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap_or_else(|error| panic!("deferred={deferred}: {error}\n{source}")),
        );
        let winch = TieredBackend::default().compile(program.clone()).unwrap();
        for function in 0..program.functions().len() {
            winch.precompile(function, Tier::Winch).unwrap();
        }
        assert!(winch.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(winch)),
            Some(Box::new(CraneliftBackend.compile(program.clone()).unwrap())),
            Some(Box::new(
                CraneliftSpeedBackend.compile(program.clone()).unwrap(),
            )),
        ];
        for (index, engine) in engines.into_iter().enumerate() {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    concurrency: cyallo_scheduler::Config {
                        processors: 1,
                        quantum: 7,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            for step in 0..100_000 {
                if let Some(engine) = &engine
                    && step % 3 != 0
                {
                    engine.run(&mut session, 1).unwrap();
                } else {
                    session.run(1);
                }
                if session.status() == &Status::Waiting {
                    session.wait_for_wakeup(Duration::from_millis(5));
                } else if session.status() != &Status::Paused {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "native callback timed out: deferred={deferred}, engine={index}, output={:?}",
                    session.context.output
                );
            }
            assert_eq!(
                session.status(),
                &Status::Finished,
                "deferred={deferred}, engine={index}, output={:?}",
                session.context.output
            );
            assert_eq!(
                session.context.output, expected,
                "deferred={deferred}, engine={index}"
            );
        }
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
        match compile_for_execution(source, CompileOptions::default()) {
            Err(deferred) => assert_eq!(deferred.code, error.code),
            Ok(program) => assert_eq!(program.validate_all().unwrap_err().code, error.code),
        }
    }
}
