//! Tuples and enums of scalars in native code: construction, field and payload
//! reads, variant tests, composite parameters and results across native calls,
//! and handing a paused frame holding composites back to the interpreter.
//! The verified interpreter is the reference.

use cyallo::{
    backend::verified::{
        CodeStore, CraneliftProfile, EntryPin, NativeTarget, NativeTier, Operation, PrepareError,
    },
    bytecode::verified::{ExecutionLimits, ExecutionStatus, FunctionId, Scalar, VerifiedModule},
    compile,
};

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

/// Publish every function, callees first, and return the entry.
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
    let entry = store.entry(module.entry()).expect("the entry is native");
    (store, entry)
}

fn check(source: &str, cases: &[&[Scalar]]) {
    if NativeTarget::host().is_none() {
        return;
    }
    let module = module(source);
    for tier in TIERS {
        let (_store, pin) = publish_all(&module, tier);
        assert!(
            pin.plan().sites().iter().any(|site| matches!(
                site.operation,
                Operation::Words(_)
                    | Operation::TagIs { .. }
                    | Operation::Variant { .. }
                    | Operation::Borrow { .. }
            )) || cases.is_empty(),
            "{tier:?}: no composite operation in the entry"
        );
        for &args in cases {
            let mut reference = module.start(args, LIMITS).unwrap();
            let wanted = reference.resume(LIMITS.fuel);
            let mut native = pin.start(args, LIMITS).unwrap();
            assert_eq!(
                native.resume(LIMITS.fuel).unwrap(),
                wanted,
                "{tier:?} {args:?}"
            );
            assert_eq!(
                native.instructions_executed(),
                reference.instructions_executed(),
                "{tier:?} {args:?}"
            );
            // Hand the frame back to the interpreter after every budget.
            for budget in 1..reference.instructions_executed() {
                let mut native = pin.start(args, LIMITS).unwrap();
                let status = native.resume(budget).unwrap();
                if !matches!(status, ExecutionStatus::Paused(_)) {
                    continue;
                }
                let mut resumed = native.recover().unwrap();
                assert_eq!(
                    resumed.resume(LIMITS.fuel),
                    wanted,
                    "{tier:?} {args:?} recovered after {budget}"
                );
            }
        }
    }
}

fn ints(values: &[i64]) -> Vec<Vec<Scalar>> {
    values.iter().map(|&n| vec![Scalar::I64(n)]).collect()
}

fn cases(values: &[Vec<Scalar>]) -> Vec<&[Scalar]> {
    values.iter().map(Vec::as_slice).collect()
}

#[test]
fn tuples_are_built_read_and_reordered_in_the_frame() {
    let source = r#"
fn entry(n: i64) -> i64 {
    let p = (n, n + 1)
    let q = (p.1, p.0)
    let nested = (q, n * 2)
    return nested.0.0 * 100 + nested.0.1 * 10 + nested.1
}
"#;
    let values = ints(&[0, 1, 7, -3]);
    check(source, &cases(&values));
}

#[test]
fn mixed_scalar_tuples_keep_each_fields_type() {
    let source = r#"
fn entry(n: i64) -> f64 {
    let p = (n > 2, n as f64 / 4.0)
    if p.0 { return p.1 + 1.5 }
    return p.1
}
"#;
    let values = ints(&[1, 2, 3, 40]);
    check(source, &cases(&values));
}

#[test]
fn tuples_cross_native_calls_as_parameters_and_results() {
    let source = r#"
fn swap(p: (i64, i64)) -> (i64, i64) { return (p.1, p.0) }
fn entry(n: i64) -> i64 {
    let t = swap(swap(swap((n, n + 5))))
    return t.0 * 1000 + t.1
}
"#;
    let values = ints(&[0, 4, -9]);
    check(source, &cases(&values));
}

#[test]
fn enums_are_wrapped_tested_and_unwrapped() {
    let source = r#"
enum Step { Stop, Go: i64, Pair: (i64, i64) }
fn entry(n: i64) -> i64 {
    let mut s = Step::Stop
    if n >= 10 { s = Step::Pair((n, n + 1)) } else if n >= 0 { s = Step::Go(n) }
    return match s {
        Step::Stop => -1,
        Step::Go(v) => v * 2,
        Step::Pair((a, b)) => a * b,
    }
}
"#;
    let values = ints(&[-5, 0, 9, 10, 25]);
    check(source, &cases(&values));
}

#[test]
fn option_results_cross_native_calls() {
    let source = r#"
fn pick(n: i64) -> Option<i64> {
    if n > 0 { return Option::Some(n) }
    return Option::None
}
fn entry(n: i64) -> i64 {
    return match pick(n) { Option::Some(v) => v, Option::None => -1 }
}
"#;
    let values = ints(&[-2, 0, 1, 50]);
    check(source, &cases(&values));
}

#[test]
#[ignore = "slow (~26s); run with --ignored"]
fn an_enum_rebuilt_in_a_loop_reuses_its_frame_words() {
    let source = r#"
enum Step { Stop, Go: i64 }
fn entry(n: i64) -> i64 {
    let mut total = 0
    let mut i = 0
    while i < n {
        let mut s = Step::Stop
        if i % 3 != 0 { s = Step::Go(i) }
        total += match s { Step::Stop => 1, Step::Go(v) => v }
        i += 1
    }
    return total
}
"#;
    let values = ints(&[0, 1, 10, 100]);
    check(source, &cases(&values));
}

#[test]
fn local_loans_read_and_write_the_owner_in_the_frame() {
    let source = r#"
fn entry(n: i64) -> i64 {
    let mut x = n
    let r = &mut x
    r.* = r.* + 5
    let s = &x
    return s.* * 2 + x
}
"#;
    let values = ints(&[0, 3, -8]);
    check(source, &cases(&values));
}

#[test]
fn loans_cross_native_calls_and_nest_through_parameters() {
    let source = r#"
proc bump(c: &mut i64, by: i64) { c.* += by }
proc twice(c: &mut i64, by: i64) { bump(c, by); bump(c, by + 1) }
fn peek(c: &i64) -> i64 { return c.* * 2 }
proc entry(n: i64) -> i64 {
    let mut total = 0
    let mut i = 0
    while i < n {
        twice(&mut total, i)
        i += 1
    }
    return peek(&total) + total
}
"#;
    let values = ints(&[0, 1, 5, 12]);
    check(source, &cases(&values));
}

#[test]
fn a_composite_can_be_read_and_replaced_through_a_loan() {
    let source = r#"
proc swap(p: &mut (i64, i64)) {
    p.* = (p.*.1, p.*.0)
}
proc entry(n: i64) -> i64 {
    let mut pair = (n, n + 1)
    swap(&mut pair)
    swap(&mut pair)
    swap(&mut pair)
    return pair.0 * 10 + pair.1
}
"#;
    let values = ints(&[0, 3, 41]);
    check(source, &cases(&values));
}

#[test]
fn field_loans_update_records_and_nested_tuples_in_place() {
    let source = r#"
record Counter { hits: i64, misses: i64 }
proc hit(c: &mut Counter) { c.hits += 1 }
proc miss(m: &mut i64, by: i64) { m.* += by }
proc entry(n: i64) -> i64 {
    let mut c = Counter { hits: 0, misses: 0 }
    let mut deep = (1, (2, 3))
    let mut i = 0
    while i < n {
        hit(&mut c)
        if i % 2 == 0 { miss(&mut c.misses, 2) }
        deep.1.0 += i
        miss(&mut deep.1.1, 1)
        i += 1
    }
    return c.hits * 1000 + c.misses * 100 + deep.1.0 * 10 + deep.1.1
}
"#;
    let values = ints(&[0, 1, 4, 9]);
    check(source, &cases(&values));
}

#[test]
fn payload_loans_mutate_the_held_variant() {
    let source = r#"
enum Slot { Empty, Full: (i64, i64) }
proc entry(n: i64) -> i64 {
    let mut slot = Slot::Full((n, 1))
    match &mut slot {
        Slot::Full((a, b)) => { a.* += 5; b.* = a.* * 2 },
        Slot::Empty => {},
    }
    return match slot { Slot::Full((a, b)) => a + b, Slot::Empty => -1 }
}
"#;
    let values = ints(&[0, 3, -9]);
    check(source, &cases(&values));
}
