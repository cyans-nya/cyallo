//! Repeatable timing samples for the tier-up policy. Ignored by default; run
//! with `scripts/tier_samples.py run <label>` (see docs/native-tiers-next-steps.md,
//! section 8). Prints a table and, when `TIER_SAMPLES_OUT` is set, writes JSON.

use cyallo::{
    backend::verified::{CodeStore, CraneliftProfile, NativeTier, PrepareError},
    bytecode::verified::{ExecutionLimits, FunctionId, Scalar, VerifiedModule},
    compile,
};
use std::time::Instant;

const TIERS: [(&str, NativeTier); 3] = [
    ("winch", NativeTier::Winch),
    (
        "cranelift_ll",
        NativeTier::Cranelift(CraneliftProfile::LowLatency),
    ),
    (
        "cranelift_opt",
        NativeTier::Cranelift(CraneliftProfile::Optimized),
    ),
];
const LIMITS: ExecutionLimits = ExecutionLimits {
    fuel: usize::MAX,
    frames: 16,
};

fn module(source: &str) -> VerifiedModule {
    let program = compile(source).unwrap();
    let index = program
        .functions()
        .iter()
        .position(|routine| routine.name.as_ref() == "entry")
        .unwrap();
    VerifiedModule::lower(&program, index).unwrap()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[values.len() / 2]
}

/// Publish every function in dependency order; return the store and entry pin.
fn publish_all(
    module: &VerifiedModule,
    tier: NativeTier,
) -> (CodeStore, cyallo::backend::verified::EntryPin) {
    let mut store = CodeStore::new(module.clone()).unwrap();
    let mut pending: Vec<FunctionId> = (0..)
        .map(FunctionId)
        .take_while(|&id| module.function(id).is_some())
        .collect();
    let mut stalled = 0;
    while let Some(id) = pending.pop() {
        match store.prepare(id) {
            Ok(plan) => match tier.emit(plan).map(|image| store.publish(image)) {
                Ok(Ok(_)) => stalled = 0,
                _ => {
                    pending.insert(0, id);
                    stalled += 1;
                }
            },
            Err(PrepareError::UnsupportedOperation { .. } | PrepareError::NonScalar(_)) => {}
            Err(error) => panic!("{error}"),
        }
        assert!(
            stalled <= pending.len() + 1,
            "functions cannot be published"
        );
    }
    let entry = store.entry(module.entry()).expect("entry is native");
    (store, entry)
}

struct Run {
    program: &'static str,
    instructions: usize,
    ns_per_instruction: [f64; 4],
}

fn run_cost(program: &'static str, source: &str, n: i64) -> Run {
    let module = module(source);
    let arg = [Scalar::I64(n)];
    let mut result = None;
    let mut times = Vec::new();
    let mut instructions = 0;
    for _ in 0..9 {
        let mut activation = module.start(&arg, LIMITS).unwrap();
        let started = Instant::now();
        let status = activation.resume(usize::MAX / 2);
        times.push(started.elapsed().as_nanos() as f64);
        instructions = activation.instructions_executed();
        result = Some(status);
    }
    let mut ns = [median(times) / instructions as f64, 0.0, 0.0, 0.0];
    for (slot, (_, tier)) in TIERS.iter().enumerate() {
        let (_store, pin) = publish_all(&module, *tier);
        let mut times = Vec::new();
        for _ in 0..9 {
            let mut native = pin.start(&arg, LIMITS).unwrap();
            let started = Instant::now();
            let status = native.resume(usize::MAX / 2).unwrap();
            times.push(started.elapsed().as_nanos() as f64);
            assert_eq!(Some(status), result, "{program} {tier:?}");
            assert_eq!(native.instructions_executed(), instructions);
        }
        ns[slot + 1] = median(times) / instructions as f64;
    }
    Run {
        program,
        instructions,
        ns_per_instruction: ns,
    }
}

fn straight_line(statements: usize) -> String {
    let mut source = String::from("fn entry(n: i64) -> i64 {\n let mut total = 0\n");
    for index in 0..statements {
        source.push_str(&format!(" total += n * {}\n", index + 1));
    }
    source.push_str(" return total\n}\n");
    source
}

fn emit_cost(statements: usize) -> (usize, [f64; 3]) {
    let module = module(&straight_line(statements));
    let mut store = CodeStore::new(module.clone()).unwrap();
    let plan = store.prepare(module.entry()).unwrap();
    let sites = plan.sites().len();
    let mut micros = [0.0; 3];
    for (slot, (_, tier)) in TIERS.iter().enumerate() {
        let _ = tier.emit(plan.clone()).unwrap();
        let samples = (0..15)
            .map(|_| {
                let started = Instant::now();
                let _ = tier.emit(plan.clone()).unwrap();
                started.elapsed().as_nanos() as f64 / 1000.0
            })
            .collect();
        micros[slot] = median(samples);
    }
    (sites, micros)
}

#[test]
#[ignore = "timing samples; run through scripts/tier_samples.py"]
fn sample() {
    let sum = "fn entry(n: i64) -> i64 {\n let mut total = 0\n for i in 1..=n { total += i }\n return total\n}\n";
    let calls = "fn add(a: i64, b: i64) -> i64 { return a + b }\nfn entry(n: i64) -> i64 {\n let mut total = 0\n for i in 1..=n { total = add(total, i) }\n return total\n}\n";
    let enums = "enum Step { Stop, Go: i64 }\nfn entry(n: i64) -> i64 {\n let mut total = 0\n let mut i = 0\n while i < n {\n  let mut s = Step::Stop\n  if i % 3 != 0 { s = Step::Go(i) }\n  total += match s { Step::Stop => 1, Step::Go(v) => v }\n  i += 1\n }\n return total\n}\n";
    let runs = [
        run_cost("scalar_loop", sum, 131_072),
        run_cost("call_loop", calls, 65_536),
        run_cost("enum_loop", enums, 32_768),
    ];
    let emits: Vec<_> = [1, 16, 64]
        .iter()
        .map(|&statements| emit_cost(statements))
        .collect();

    println!("program        instr      interp   winch    cl_ll    cl_opt  (ns/instr)");
    for run in &runs {
        let [a, b, c, d] = run.ns_per_instruction;
        println!(
            "{:<13} {:>9} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
            run.program, run.instructions, a, b, c, d
        );
    }
    println!("sites   winch_us   cl_ll_us   cl_opt_us   N_promote(cl_ll, gap of scalar loop)");
    let gap = runs[0].ns_per_instruction[1] - runs[0].ns_per_instruction[2];
    for (sites, micros) in &emits {
        let promote = micros[1] * 1000.0 / gap;
        println!(
            "{:>5} {:>10.1} {:>10.1} {:>11.1}   {:>12.0}",
            sites, micros[0], micros[1], micros[2], promote
        );
    }

    if let Ok(path) = std::env::var("TIER_SAMPLES_OUT") {
        let runs_json: Vec<String> = runs
            .iter()
            .map(|run| {
                format!(
                    "{{\"program\":\"{}\",\"instructions\":{},\"ns_per_instruction\":{{\"interpreter\":{:.4},\"winch\":{:.4},\"cranelift_ll\":{:.4},\"cranelift_opt\":{:.4}}}}}",
                    run.program, run.instructions, run.ns_per_instruction[0], run.ns_per_instruction[1], run.ns_per_instruction[2], run.ns_per_instruction[3]
                )
            })
            .collect();
        let emits_json: Vec<String> = emits
            .iter()
            .map(|(sites, micros)| {
                format!(
                    "{{\"sites\":{},\"emit_us\":{{\"winch\":{:.2},\"cranelift_ll\":{:.2},\"cranelift_opt\":{:.2}}}}}",
                    sites, micros[0], micros[1], micros[2]
                )
            })
            .collect();
        std::fs::write(
            path,
            format!(
                "{{\"runs\":[{}],\"emits\":[{}]}}\n",
                runs_json.join(","),
                emits_json.join(",")
            ),
        )
        .unwrap();
    }
}
