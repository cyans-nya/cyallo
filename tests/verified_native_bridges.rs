//! Output and strings in native code, through runtime bridges. The verified
//! interpreter is the reference: results, printed lines and instruction counts
//! must agree, also when a paused native frame continues in the interpreter.

use cyallo::{
    CompileOptions, Dialect,
    backend::verified::{
        CodeStore, CraneliftProfile, EntryPin, NativeTarget, NativeTier, Operation, PrepareError,
    },
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, FunctionId, OutputSink, Scalar, VerifiedModule,
    },
    compile, compile_with_options,
};
use std::sync::{Arc, Mutex};

const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: 100_000,
    frames: 16,
};
const TIERS: [NativeTier; 3] = [
    NativeTier::Winch,
    NativeTier::Cranelift(CraneliftProfile::LowLatency),
    NativeTier::Cranelift(CraneliftProfile::Optimized),
];

fn module(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    let index = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(&program, index).unwrap()
}

fn caie_module(source: &str) -> VerifiedModule {
    let options = CompileOptions {
        dialect: Dialect::Compatibility,
        convenience_builtins: true,
        ..Default::default()
    };
    let program = compile_with_options(source, options).unwrap();
    VerifiedModule::lower(&program, 0).unwrap()
}

fn publish_all(module: &VerifiedModule, tier: NativeTier) -> (CodeStore, EntryPin) {
    let mut store = CodeStore::new(module.clone()).unwrap();
    let mut pending: Vec<FunctionId> = (0..)
        .map(FunctionId)
        .take_while(|&id| module.function(id).is_some())
        .collect();
    let mut stalled = 0;
    let mut skipped = Vec::new();
    while let Some(id) = pending.pop() {
        match store.prepare(id) {
            Ok(plan) => match tier.emit(plan).map(|image| store.publish(image)) {
                Ok(Ok(_)) => stalled = 0,
                _ => {
                    pending.insert(0, id);
                    stalled += 1;
                }
            },
            Err(
                error @ (PrepareError::UnsupportedOperation { .. } | PrepareError::NonScalar(_)),
            ) => skipped.push(error.to_string()),
            Err(error) => panic!("{error}"),
        }
        assert!(
            stalled <= pending.len() + 1,
            "functions cannot be published; skipped: {skipped:?}"
        );
    }
    let entry = store
        .entry(module.entry())
        .unwrap_or_else(|| panic!("the entry is not native; skipped: {skipped:?}"));
    (store, entry)
}

fn capture() -> (OutputSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let lines = lines.clone();
        OutputSink::new(move |line| lines.lock().unwrap().push(line.to_owned()))
    };
    (sink, lines)
}

fn check(source: &str, cases: &[Vec<Scalar>]) {
    check_module(module(source), cases);
}

fn check_module(module: VerifiedModule, cases: &[Vec<Scalar>]) {
    if NativeTarget::host().is_none() {
        return;
    }
    for tier in TIERS {
        let (_store, pin) = publish_all(&module, tier);
        assert!(
            pin.plan()
                .sites()
                .iter()
                .any(|site| matches!(site.operation, Operation::Bridge(_))),
            "{tier:?}: no bridge in the entry"
        );
        for args in cases {
            let (sink, wanted_lines) = capture();
            let mut reference = module.start(args, LIMITS).unwrap();
            reference.set_output(sink);
            let wanted = reference.resume(LIMITS.fuel);

            let (sink, lines) = capture();
            let mut native = pin.start(args, LIMITS).unwrap();
            native.set_output(sink);
            assert_eq!(
                native.resume(LIMITS.fuel).unwrap(),
                wanted,
                "{tier:?} {args:?}"
            );
            assert_eq!(
                *lines.lock().unwrap(),
                *wanted_lines.lock().unwrap(),
                "{tier:?} {args:?}"
            );
            assert_eq!(
                native.instructions_executed(),
                reference.instructions_executed()
            );
            for budget in 1..reference.instructions_executed() {
                let (sink, lines) = capture();
                let mut native = pin.start(args, LIMITS).unwrap();
                native.set_output(sink);
                if !matches!(native.resume(budget).unwrap(), ExecutionStatus::Paused(_)) {
                    continue;
                }
                let mut resumed = native.recover().unwrap();
                assert_eq!(
                    resumed.resume(LIMITS.fuel),
                    wanted,
                    "{tier:?} {args:?} @{budget}"
                );
                assert_eq!(
                    *lines.lock().unwrap(),
                    *wanted_lines.lock().unwrap(),
                    "{tier:?} {args:?} output after recovery at {budget}"
                );
            }
        }
    }
}

fn ints(values: &[i64]) -> Vec<Vec<Scalar>> {
    values.iter().map(|&n| vec![Scalar::I64(n)]).collect()
}

#[test]
fn scalars_are_printed_in_a_native_loop() {
    let source = r#"
proc entry(n: i64) -> i64 {
    let mut i = 0
    let mut total = 0
    while i < n {
        total += i * i
        if i % 2 == 0 { std::io::print(i, " ", total, " ", i < 3) }
        i += 1
    }
    return total
}
"#;
    check(source, &ints(&[0, 1, 6]));
}

#[test]
fn strings_are_built_compared_measured_and_printed() {
    let source = r#"
proc entry(n: i64) -> i64 {
    let greeting = "héllo"
    let mut text = greeting + ", "
    let mut i = 0
    while i < n {
        text = text + "ab"
        i += 1
    }
    std::io::print(text)
    if text == greeting { std::io::print("same") }
    if greeting < text { std::io::print("less") }
    return std::string::len(text)
}
"#;
    check(source, &ints(&[0, 1, 4]));
}

#[test]
fn strings_cross_native_calls_and_composites() {
    let source = r#"
fn twice(s: string) -> string { return s + s }
fn pair(s: string) -> (string, i64) {
    let length = std::string::len(s)
    return (twice(s), length)
}
proc entry(n: i64) -> i64 {
    let p = pair("xy")
    std::io::print(p.0, ":", p.1, ":", n)
    return p.1 + n
}
"#;
    check(source, &ints(&[0, 7]));
}

#[test]
fn string_functions_match_the_interpreter() {
    let source = r#"
proc entry(n: i64) -> i64 {
    let padded = "  Straße ñ  "
    let text = std::string::trim(padded)
    std::io::print("[", text, "]", std::string::upper(text), std::string::lower("ÀBC"))
    std::io::print(std::string::contains(text, "ße"), std::string::starts_with(text, "St"), std::string::ends_with(text, "x"))
    std::io::print(std::string::replace("a-b-c", "-", "+"), std::string::replace("aaa", "a", "bb"))
    std::io::print(std::string::from(n), "|", std::string::from(2.5), "|", std::string::from(n > 0), "|", std::string::from(text))
    return std::string::len(text) + n
}
"#;
    check(source, &ints(&[-4, 0, 99]));
}

#[test]
fn caie_slicing_prints_and_fails_with_the_same_typed_error() {
    let ok = concat!(
        "DECLARE t : STRING\n",
        "t <- \"Cambridge\"\n",
        "OUTPUT LEFT(t, 3), \"|\", RIGHT(t, 3), \"|\", MID(t, 4, 3), \"|\", MID(t, 1, 9), \"|\", LEFT(t, 0)\n",
    );
    check_module(caie_module(ok), &[vec![]]);
    for expression in [
        "MID(t, 0, 1)",
        "MID(t, 2, -1)",
        "MID(t, 3, 2)",
        "LEFT(t, 4)",
        "RIGHT(t, 4)",
        "LEFT(t, -1)",
    ] {
        let source =
            format!("DECLARE t : STRING\nt <- \"abc\"\nOUTPUT \"ok\"\nOUTPUT {expression}\n");
        check_module(caie_module(&source), &[vec![]]);
    }
}
