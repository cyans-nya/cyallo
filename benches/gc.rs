//! Observation-free execution samples; parsing/code generation are outside timing.
use cyallo::{
    Context, Session, Status,
    backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TierPolicy,
        TieredBackend,
    },
    compile,
    runtime::gc::{GcConfig, GcStats},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    for (name, source) in [
        (
            "scalar",
            "let mut total = 0\nfor i = 0 to 100000 { total += i }\nstd::io::print(total)\n",
        ),
        (
            "short-lived",
            "fn work(n: i64) -> i64 { let a = [n,n+1]; let b = a; return b[0]+b[1] }\nlet mut total = 0\nfor i = 0 to 5000 { total += work(i) }\nstd::io::print(total)\n",
        ),
        (
            "high-survival-copy",
            "proc copy(data: &mut []i64, n: i64) { let other = std::mem::clone(&data[1]); data[0] = other+n }\nlet mut data = std::collections::Vec::from(0..512)\nfor i = 0 to 100 { copy(&mut data,i) }\nstd::io::print(data[0])\n",
        ),
    ] {
        let program = Arc::new(compile(source).unwrap());
        if std::env::args().any(|arg| arg == "--check") {
            println!("{name}: OK");
            continue;
        }
        let winch = TieredBackend {
            policy: TierPolicy {
                hot_instructions: u64::MAX,
                warm_instructions: u64::MAX,
                ..Default::default()
            },
        }
        .compile(program.clone())
        .unwrap();
        for fi in 0..program.functions().len() {
            winch.precompile(fi, Tier::Winch).unwrap();
        }
        assert!(winch.wait_for_compilation(Duration::from_secs(30)));
        let fast = CraneliftBackend.compile(program.clone()).unwrap();
        let speed = CraneliftSpeedBackend.compile(program.clone()).unwrap();
        for (engine, artifact) in [
            ("interpreter", None),
            ("winch", Some(&winch as &dyn Executable)),
            ("cranelift", Some(&fast as &dyn Executable)),
            ("cranelift_speed", Some(&speed as &dyn Executable)),
        ] {
            let mut expected = None;
            for enabled in [false, true] {
                let mut samples = Vec::new();
                let mut stats = GcStats::default();
                for _ in 0..9 {
                    let start = Instant::now();
                    let mut session = Session::with_context(
                        program.clone(),
                        Context {
                            gc: GcConfig {
                                enabled,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    if let Some(artifact) = artifact {
                        artifact.run(&mut session, 100000000).unwrap();
                    } else {
                        session.run(100000000);
                    }
                    assert_eq!(session.status(), &Status::Finished);
                    samples.push(start.elapsed());
                    if let Some(output) = &expected {
                        assert_eq!(&session.context.output, output);
                    } else {
                        expected = Some(session.context.output.clone());
                    }
                    stats = session.gc_stats().clone();
                    std::hint::black_box(&session);
                }
                samples.sort();
                println!(
                    "{name:18} {engine:16} gc={enabled:5} median={:9.3} ms p95={:9.3} ms collections={} reclaimed={} peak={} final={}",
                    samples[4].as_secs_f64() * 1000.,
                    samples[8].as_secs_f64() * 1000.,
                    stats.collections,
                    stats.reclaimed_cells,
                    stats.peak_cells,
                    stats.occupied_cells
                );
            }
        }
    }
}
