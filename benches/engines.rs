use cyallo::{
    CompileOptions, Dialect, Session, Status,
    backend::{
        CompilationScope, CraneliftBackend, CraneliftConfig, CraneliftSpeedBackend, Executable,
        NativeBackend, OptimizationLevel, ResumeStrategy, Tier, TierPolicy, TieredArtifact,
        TieredBackend,
    },
    compile_with_options,
};
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
const BUDGET: usize = 100_000_000;
struct Case {
    name: String,
    source: String,
    dialect: Dialect,
    samples: usize,
    sessions: usize,
}
impl Case {
    fn new(
        name: impl Into<String>,
        source: impl Into<String>,
        samples: usize,
        sessions: usize,
    ) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            dialect: Dialect::Modern,
            samples,
            sessions,
        }
    }
}
fn measure(
    program: &Arc<cyallo::ir::Program>,
    sessions: usize,
    artifact: Option<&dyn Executable>,
) -> Duration {
    let start = Instant::now();
    for _ in 0..sessions {
        let mut session = Session::new(program.clone()).unwrap();
        if let Some(artifact) = artifact {
            artifact.run(&mut session, BUDGET).unwrap();
        } else {
            session.run(BUDGET);
        }
        assert_eq!(session.status(), &Status::Finished);
        std::hint::black_box(session.context.output);
    }
    start.elapsed()
}
fn summary(durations: &mut [Duration]) -> String {
    durations.sort();
    format!(
        "median {:?}; p95 {:?}",
        durations[durations.len() / 2],
        durations[durations.len() * 19 / 20]
    )
}
fn warm(program: &Arc<cyallo::ir::Program>, tier: Tier) -> (TieredArtifact, Duration) {
    let start = Instant::now();
    // Freeze the requested tier: a hot workload must not silently promote a
    // baseline/fast measurement to speed during the repeated samples.
    let artifact = TieredBackend {
        policy: TierPolicy {
            small_limit: usize::MAX - 1,
            heavy_limit: usize::MAX,
            hot_instructions: u64::MAX,
            warm_instructions: u64::MAX,
            ..TierPolicy::default()
        },
    }
    .compile(program.clone())
    .unwrap();
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    for function in 0..program.functions().len() {
        artifact.precompile(function, tier).unwrap();
    }
    assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
    (artifact, start.elapsed())
}
fn sized_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (label, n, sessions) in [("small", 100, 50), ("medium", 1000, 10), ("large", 4000, 3)] {
        cases.push(Case::new(
            format!("quicksort-{label}"),
            include_str!("../examples/quicksort.ya")
                .replace("let n = 1000", &format!("let n = {n}")),
            7,
            sessions,
        ));
    }
    for (label, n, sessions) in [("small", 100, 50), ("medium", 800, 10), ("large", 3200, 3)] {
        cases.push(Case::new(
            format!("mergesort-{label}"),
            include_str!("../examples/mergesort.ya")
                .replace("let n = 800", &format!("let n = {n}")),
            7,
            sessions,
        ));
    }
    for (label, n, sessions) in [("small", 18, 20), ("medium", 22, 5), ("large", 26, 2)] {
        cases.push(Case::new(
            format!("fib-{label}"),
            include_str!("../examples/fib.ya").replace(
                "std::io::print(fib(22))",
                &format!("std::io::print(fib({n}))"),
            ),
            7,
            sessions,
        ));
    }
    for (label, items, capacity, sessions) in [
        ("small", 10, 50, 50),
        ("medium", 40, 200, 10),
        ("large", 100, 500, 3),
    ] {
        cases.push(Case::new(
            format!("knapsack-{label}"),
            include_str!("../examples/knapsack.ya")
                .replace("let items = 40", &format!("let items = {items}"))
                .replace("let capacity = 200", &format!("let capacity = {capacity}")),
            7,
            sessions,
        ));
    }
    for (label, n, sessions) in [("small", 100, 30), ("medium", 300, 5), ("large", 600, 2)] {
        cases.push(Case::new(
            format!("dfs-{label}"),
            include_str!("../examples/dfs.ya").replace("let n = 300", &format!("let n = {n}")),
            7,
            sessions,
        ));
    }
    for (label, reps, sessions) in [("small", 20, 20), ("medium", 50, 5), ("large", 150, 2)] {
        cases.push(Case::new(
            format!("parser-{label}"),
            include_str!("../examples/parser.ya")
                .replace("let reps = 200", &format!("let reps = {reps}")),
            7,
            sessions,
        ));
    }
    cases
}
fn admission(name: &str, source: &str, dialect: Dialect) {
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..41 {
        for offset in 0..2 {
            let index = (sample + offset) % 2;
            let start = Instant::now();
            let program = Arc::new(
                cyallo::compile_for_execution(
                    source,
                    CompileOptions {
                        dialect,
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
            let artifact = TieredBackend {
                policy: TierPolicy {
                    eager: cyallo::backend::EagerPolicy {
                        enabled: index == 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            }
            .compile(program.clone())
            .unwrap();
            let mut session = Session::new(program).unwrap();
            artifact.run(&mut session, BUDGET).unwrap();
            assert_eq!(session.status(), &Status::Finished);
            times[index].push(start.elapsed());
            // Drain outside the timed region so one sample's background work
            // cannot overlap another sample. Both modes retain the same service.
            assert!(artifact.wait_for_compilation(Duration::from_secs(30)));
            std::hint::black_box(session.context.output);
        }
    }
    for (index, times) in times.iter_mut().enumerate() {
        println!(
            "{name} eager={} {}; includes frontend, backend, session and execution; retained compiler service",
            index == 1,
            summary(times)
        );
    }
}
fn main() {
    let mut cases: Vec<Case> = [
        (
            "loops",
            include_str!("../examples/loops.psc"),
            Dialect::Compatibility,
        ),
        (
            "hot-loop",
            "let mut total = 0\nfor i = 1 to 10000 { total += i }\nstd::io::print(total)\n",
            Dialect::Modern,
        ),
        (
            "calls",
            include_str!("../examples/functions.psc"),
            Dialect::Compatibility,
        ),
        (
            "arrays",
            include_str!("../examples/arrays.psc"),
            Dialect::Compatibility,
        ),
        (
            "strings",
            include_str!("../examples/strings.psc"),
            Dialect::Compatibility,
        ),
    ]
    .into_iter()
    .map(|(name, source, dialect)| Case {
        dialect,
        ..Case::new(name, source, 21, 100)
    })
    .collect();
    cases.push(Case::new(
        "modern",
        include_str!("../examples/modern.ya"),
        15,
        5,
    ));
    cases.push(Case::new("tiny-leaves", "fn square(n: i64) -> i64 { return n * n }\nfn cube(n: i64) -> i64 { return square(n) * n }\nlet mut total = 0\nfor i = 0 to 32767 { total += cube(i) }\nstd::io::print(total)\n", 15, 5));
    cases.push(Case::new("procedure-calls", "proc bump(n: &mut i64) { *n += 1 }\nlet mut total = 0\nfor i = 0 to 32767 { bump(&mut total) }\nstd::io::print(total)\n", 15, 5));
    for count in [100, 500] {
        let source = (0..count)
            .map(|i| format!("fn cold{i}(n: i64) -> i64 {{ return n + {i} }}\n"))
            .collect::<String>()
            + "std::io::print(cold0(42))\n";
        cases.push(Case::new(format!("codegen-size-{count}"), source, 3, 1));
    }
    cases.extend(sized_cases());
    let args: Vec<_> = std::env::args().skip(1).collect();
    let matched = args.iter().any(|arg| arg == "--matched");
    let filter = args
        .iter()
        .find(|arg| !arg.starts_with('-') && !arg.contains("engines"));
    for Case {
        name,
        source,
        dialect,
        samples,
        sessions,
    } in cases
    {
        if filter.is_some_and(|filter| !name.contains(filter.as_str())) {
            continue;
        }
        if args.iter().any(|arg| arg == "--admission") {
            admission(&name, &source, dialect);
            continue;
        }
        let program = Arc::new(
            compile_with_options(
                &source,
                CompileOptions {
                    dialect,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        if args.iter().any(|arg| arg == "--check") {
            println!("{name}: OK");
            continue;
        }

        let start = Instant::now();
        let direct = CraneliftBackend.compile(program.clone()).unwrap();
        println!(
            "{name} direct-fast compile {:?}; code {} bytes; {:?}",
            start.elapsed(),
            direct.code_size(),
            direct.configuration()
        );
        let start = Instant::now();
        let direct_speed = CraneliftSpeedBackend.compile(program.clone()).unwrap();
        println!(
            "{name} direct-speed compile {:?}; code {} bytes; {:?}",
            start.elapsed(),
            direct_speed.code_size(),
            direct_speed.configuration()
        );
        let mut frozen = Vec::new();
        for tier in [Tier::Winch, Tier::CraneliftFast, Tier::CraneliftSpeed] {
            let (artifact, compile) = warm(&program, tier);
            println!("{name} {tier:?} prewarm {compile:?}");
            frozen.push((tier, artifact));
        }
        let mut comparisons = Vec::new();
        for optimization in [OptimizationLevel::None, OptimizationLevel::Speed] {
            let config = CraneliftConfig {
                optimization,
                inlining: false,
                ..Default::default()
            };
            let start = Instant::now();
            let artifact = config.compile(program.clone()).unwrap();
            let label = format!("no-inline-{optimization:?}");
            println!(
                "{name} {label} compile {:?}; code {} bytes; {config:?}",
                start.elapsed(),
                artifact.code_size()
            );
            comparisons.push((label, artifact));
            let config = CraneliftConfig {
                optimization,
                procedure_inlining: true,
                ..Default::default()
            };
            let start = Instant::now();
            let artifact = config.compile(program.clone()).unwrap();
            let label = format!("procedure-bodies-{optimization:?}");
            println!(
                "{name} {label} compile {:?}; code {} bytes; {config:?}",
                start.elapsed(),
                artifact.code_size()
            );
            comparisons.push((label, artifact));
        }
        let configurations = [
            (OptimizationLevel::None, false, false),
            (OptimizationLevel::None, true, false),
            (OptimizationLevel::None, true, true),
            (OptimizationLevel::Speed, false, false),
            (OptimizationLevel::Speed, true, false),
            (OptimizationLevel::Speed, true, true),
        ];
        let mut codegen = vec![vec![]; configurations.len()];
        for sample in 0..7 {
            for offset in 0..configurations.len() {
                let index = (sample + offset) % configurations.len();
                let (optimization, inlining, procedure_inlining) = configurations[index];
                let config = CraneliftConfig {
                    optimization,
                    inlining,
                    procedure_inlining,
                    ..Default::default()
                };
                let start = Instant::now();
                let artifact = config.compile(program.clone()).unwrap();
                codegen[index].push(start.elapsed());
                std::hint::black_box(artifact.code_size());
            }
        }
        for ((optimization, inlining, procedures), times) in configurations.iter().zip(&mut codegen)
        {
            println!(
                "{name} matched-codegen {optimization:?} inline={inlining} procedures={procedures} {}",
                summary(times)
            );
        }
        if matched {
            for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
                for resume in [ResumeStrategy::AllPcs, ResumeStrategy::LoopHeaders] {
                    for optimization in [OptimizationLevel::None, OptimizationLevel::Speed] {
                        let config = CraneliftConfig {
                            scope,
                            resume,
                            optimization,
                            ..Default::default()
                        };
                        let start = Instant::now();
                        let artifact = config.compile(program.clone()).unwrap();
                        let label = format!("matched-{scope:?}-{resume:?}-{optimization:?}");
                        println!(
                            "{name} {label} compile {:?}; code {} bytes; {config:?}",
                            start.elapsed(),
                            artifact.code_size()
                        );
                        comparisons.push((label, artifact));
                    }
                }
            }
        }
        #[cfg(feature = "wasm-adapter")]
        let wasm = {
            let start = Instant::now();
            let wasm = cyallo::backend::WasmBackend
                .compile(program.clone())
                .unwrap();
            println!("{name} legacy-wasm compile {:?}", start.elapsed());
            wasm
        };
        let mut artifacts: Vec<(String, Option<&dyn Executable>)> = vec![
            ("interpreter".into(), None),
            ("direct-fast".into(), Some(&direct)),
            ("direct-speed".into(), Some(&direct_speed)),
        ];
        for (tier, artifact) in &frozen {
            artifacts.push((format!("{tier:?}"), Some(artifact)));
        }
        for (label, artifact) in &comparisons {
            artifacts.push((label.clone(), Some(artifact)));
        }
        #[cfg(feature = "wasm-adapter")]
        artifacts.push(("legacy-wasm".into(), Some(&wasm)));
        for (label, artifact) in &artifacts {
            let mut probe = Session::new(program.clone()).unwrap();
            if let Some(artifact) = artifact {
                artifact.run(&mut probe, BUDGET).unwrap();
            } else {
                probe.run(BUDGET);
            }
            assert_eq!(probe.status(), &Status::Finished);
            println!(
                "{name} {label} logical work {}; frameless calls {}; embedded procedure calls {}",
                probe.instruction_count(),
                probe.inlined_call_count(),
                probe.body_inlined_call_count()
            );
        }
        let mut durations = vec![Vec::with_capacity(samples); artifacts.len()];
        for sample in 0..samples {
            for offset in 0..artifacts.len() {
                let index = (sample + offset) % artifacts.len();
                durations[index].push(measure(&program, sessions, artifacts[index].1));
            }
        }
        for ((label, _), durations) in artifacts.iter().zip(&mut durations) {
            println!("{name} {label} {} / {sessions}", summary(durations));
        }
        for (tier, artifact) in &frozen {
            assert!(
                artifact.tiers().iter().all(|info| info.tier == Some(*tier)),
                "measured tier changed"
            );
        }
        let mut fresh = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            let artifact = TieredBackend::default().compile(program.clone()).unwrap();
            let mut session = Session::new(program.clone()).unwrap();
            artifact.run(&mut session, BUDGET).unwrap();
            assert_eq!(session.status(), &Status::Finished);
            fresh.push(start.elapsed());
        }
        println!(
            "{name} fresh-artifact-jit {} / 1; includes backend/session setup; excludes frontend; compiler service retained",
            summary(&mut fresh)
        );
    }
    if args
        .iter()
        .any(|arg| arg == "--check" || arg == "--admission")
        || filter.is_some_and(|filter| !"frontend".contains(filter.as_str()))
    {
        return;
    }
    let source=(0..500).map(|i|format!("fn cold{i}(n: i64) -> i64 {{ let mut total = 0; for i = 1 to n {{ total += i }}; return total }}\n")).collect::<String>()+"std::io::print(1)\n";
    for deferred in [false, true] {
        let mut times = vec![];
        for _ in 0..21 {
            let start = Instant::now();
            let program = if deferred {
                cyallo::compile_for_execution(&source, Default::default())
            } else {
                cyallo::compile(&source)
            }
            .unwrap();
            times.push(start.elapsed());
            assert_eq!(
                program
                    .functions()
                    .iter()
                    .filter(|f| f.is_analyzed())
                    .count(),
                if deferred { 1 } else { 501 }
            );
            std::hint::black_box(program);
        }
        times.sort();
        println!(
            "frontend 500 cold routines deferred={deferred}: median {:?}; p95 {:?}",
            times[10], times[19]
        );
    }
}
