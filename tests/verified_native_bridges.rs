//! Output, strings and collections in native code. The verified
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
    compile, compile_for_execution, compile_with_options,
    ir::Program,
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
    lower_entry(&compile(source).unwrap())
}

fn lower_entry(program: &Program) -> VerifiedModule {
    let index = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(program, index).unwrap()
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
            reference.set_gc_stress(true);
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
                assert!(
                    matches!(native.resume(budget).unwrap(), ExecutionStatus::Paused(_)),
                    "{tier:?} {args:?} should pause at {budget}"
                );
                let mut resumed = native.recover().unwrap();
                resumed.set_gc_stress(true);
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
                assert_eq!(
                    resumed.instructions_executed(),
                    reference.instructions_executed()
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

fn caie_check(source: &str) {
    check_module(caie_module(source), &[vec![]]);
}

#[test]
fn caie_arrays_are_filled_summed_and_printed() {
    caie_check(include_str!("../examples/arrays.psc"));
    caie_check(concat!(
        "DECLARE a : ARRAY[1:10] OF INTEGER\n",
        "DECLARE i : INTEGER\n",
        "DECLARE total : INTEGER\n",
        "total <- 0\n",
        "FOR i <- 1 TO 10\n",
        "    a[i] <- i * i\n",
        "NEXT i\n",
        "FOR i <- 10 TO 1 STEP -3\n",
        "    total <- total + a[i]\n",
        "    OUTPUT i, \":\", a[i]\n",
        "NEXT i\n",
        "OUTPUT total\n",
    ));
}

#[test]
fn caie_two_dimensional_arrays_and_element_types() {
    caie_check(concat!(
        "DECLARE grid : ARRAY[1:3, 0:2] OF INTEGER\n",
        "DECLARE flags : ARRAY[1:4] OF BOOLEAN\n",
        "DECLARE reals : ARRAY[0:2] OF REAL\n",
        "DECLARE r : INTEGER\n",
        "DECLARE c : INTEGER\n",
        "FOR r <- 1 TO 3\n",
        "    FOR c <- 0 TO 2\n",
        "        grid[r, c] <- r * 10 + c\n",
        "    NEXT c\n",
        "NEXT r\n",
        "FOR r <- 1 TO 4\n",
        "    flags[r] <- (r MOD 2) = 0\n",
        "NEXT r\n",
        "FOR c <- 0 TO 2\n",
        "    reals[c] <- c / 4\n",
        "NEXT c\n",
        "OUTPUT grid[1, 0], \" \", grid[3, 2], \" \", grid[2, 1]\n",
        "OUTPUT flags[1], \" \", flags[2], \" \", reals[0], \" \", reals[2]\n",
    ));
}

#[test]
fn caie_array_failures_match_the_interpreter() {
    for access in ["a[0]", "a[4]", "a[2]"] {
        caie_check(&format!(
            "DECLARE a : ARRAY[1:3] OF INTEGER\na[1] <- 5\nOUTPUT \"start\"\nOUTPUT {access}\n"
        ));
    }
    caie_check(concat!(
        "DECLARE a : ARRAY[1:3] OF INTEGER\n",
        "DECLARE i : INTEGER\n",
        "i <- 7\n",
        "OUTPUT \"before\"\n",
        "a[i] <- 1\n",
        "OUTPUT \"after\"\n",
    ));
}

#[test]
fn element_loans_and_by_value_arrays_cross_native_calls() {
    let source = r#"
proc bump(x: &mut i64, by: i64) { x.* += by }
proc entry(n: i64) -> i64 {
    let mut a = [1, 2, 3]
    let mut i = 0
    while i < 3 {
        bump(&mut a[i], n + i)
        i += 1
    }
    bump(&mut a[0], 100)
    std::io::print(a[0], " ", a[1], " ", a[2])
    return a[0] * 10 + a[2]
}
"#;
    check(source, &ints(&[0, 5, -4]));
}

#[test]
fn caie_arrays_are_passed_by_value_to_native_functions() {
    caie_check(concat!(
        "FUNCTION Total(v : ARRAY[1:3] OF INTEGER) RETURNS INTEGER\n",
        "    DECLARE i : INTEGER\n",
        "    DECLARE s : INTEGER\n",
        "    s <- 0\n",
        "    FOR i <- 1 TO 3\n",
        "        s <- s + v[i]\n",
        "        v[i] <- 0\n",
        "    NEXT i\n",
        "    RETURN s\n",
        "ENDFUNCTION\n",
        "DECLARE a : ARRAY[1:3] OF INTEGER\n",
        "DECLARE i : INTEGER\n",
        "FOR i <- 1 TO 3\n",
        "    a[i] <- i * 7\n",
        "NEXT i\n",
        "OUTPUT Total(a), \" \", Total(a), \" \", a[1], \" \", a[2], \" \", a[3]\n",
    ));
}

#[test]
fn scalar_vectors_grow_and_keep_independent_copies_across_native_calls() {
    let source = r#"
proc bump(x: &mut i64, by: i64) { x.* += by }
proc fill(items: &mut Vec<i64>, n: i64) {
    let original = items.len() as i64
    for i in 0..n { items.push(i + original) }
    bump(&mut items[0], n)
}
fn duplicate(items: &Vec<i64>) -> Vec<i64> {
    return std::mem::clone(items)
}
proc change(source: Vec<i64>) -> i64 {
    let mut items = source
    items[0] = 90
    items.push(91)
    return items[0] + (items.len() as i64)
}
proc entry(n: i64) -> i64 {
    let mut items = Vec::from(-2..=0)
    let copy = duplicate(&items)
    fill(&mut items, n)
    std::io::print(items.len(), ":", items[0], ":", copy[0], ":", change(copy))
    return (items.len() as i64) + items[0]
}
"#;
    let cases = ints(&[0, 1, 7]);
    check(source, &cases);
    let deferred = compile_for_execution(source, CompileOptions::default()).unwrap();
    check_module(lower_entry(&deferred), &cases);
}

#[test]
fn empty_and_non_integer_vectors_use_dense_elements() {
    check(
        r#"
proc entry(n: i64) -> i64 {
    let mut flags = Vec<bool>[]
    let mut samples = Vec[0.5, 1.0]
    let empty = Vec::from(4..n)
    flags.push(n > 0)
    flags.push(false)
    let first = &mut samples[0]
    first.* = first.* + 1.5
    samples.push(3.0)
    std::io::print(flags[0], flags[1], samples[0], samples[2], empty.len())
    return (flags.len() as i64) + (samples.len() as i64)
}
"#,
        &ints(&[0, 1, 4]),
    );
}

#[test]
fn native_vector_bounds_and_range_failures_match_the_interpreter() {
    for statement in [
        "std::io::print(items[n])",
        "items[n] = 7",
        "let element = &mut items[n]\nelement.* = 3",
    ] {
        check(
            &format!(
                r#"
proc entry(n: i64) {{
    let mut items = Vec[4, 5]
    items.push(6)
    std::io::print("before")
    {statement}
}}
"#
            ),
            &ints(&[-1, 2, 3, i64::MAX]),
        );
    }
    check(
        r#"
proc entry(n: i64) {
    std::io::print("before")
    let items = Vec::from(-2..=n)
    std::io::print(items.len())
}
"#,
        &ints(&[-3, -2, 3, 1 << 22, i64::MAX]),
    );
    check(
        r#"
proc entry(n: u64) -> i64 {
    let items = Vec[7]
    return items[n]
}
"#,
        &[vec![Scalar::U64(0)], vec![Scalar::U64(u64::MAX)]],
    );
    check(
        r#"
proc entry(n: i64) {
    let items = Vec[7]
    std::io::print(items[n as u64])
}
"#,
        &ints(&[-1, 0]),
    );
    check(
        r#"
proc entry(n: u64) -> i64 {
    let items = Vec[7]
    std::io::print(items.len())
    return n as i64
}
"#,
        &[vec![Scalar::U64(0)], vec![Scalar::U64(u64::MAX)]],
    );
}

#[test]
fn vector_growth_preserves_an_element_reborrow_in_a_native_callee() {
    check(
        r#"
proc append(items: &mut Vec<i64>) {
    let element = &mut items[0]
    items.push(2)
    element.* = 4
}
proc entry() -> i64 {
    let mut items = Vec[1]
    append(&mut items)
    return 10 * items[0] + items[1]
}
"#,
        &[vec![]],
    );
}

#[test]
fn the_verifier_rejects_a_direct_push_with_a_live_exclusive_element_loan() {
    use cyallo::bytecode::verified::{InvalidBytecode, LowerError};

    let source = r#"
proc append() {
    let mut items = Vec[1]
    let element = &mut items[0]
    items.push(2)
    element.* = 4
}
append()
"#;
    for program in [
        compile(source).unwrap(),
        compile_for_execution(source, CompileOptions::default()).unwrap(),
    ] {
        let error = VerifiedModule::lower(&program, 0).err().unwrap();
        assert!(matches!(error, LowerError::Verification(error)
                if matches!(error.kind, InvalidBytecode::ExclusiveOwnerAccess(_))));
    }
}
