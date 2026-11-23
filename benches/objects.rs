//! Internal runtime benchmark using the same Cyallo example in every engine.
use cyallo::{
    Session, Status,
    backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
    },
    runtime::ObjectIcMode,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn main() {
    let program =
        Arc::new(cyallo::compile(include_str!("../examples/objects/native-ic.yas")).unwrap());
    let winch = TieredBackend::default().compile(program.clone()).unwrap();
    for function in 0..program.functions().len() {
        winch.precompile(function, Tier::Winch).unwrap();
    }
    assert!(winch.wait_for_compilation(Duration::from_secs(30)));
    let engines: Vec<(&str, Box<dyn Executable>)> = vec![
        ("tiered", Box::new(winch)),
        (
            "cranelift",
            Box::new(CraneliftBackend.compile(program.clone()).unwrap()),
        ),
        (
            "cranelift_speed",
            Box::new(CraneliftSpeedBackend.compile(program.clone()).unwrap()),
        ),
    ];
    for (name, engine) in engines {
        for mode in [ObjectIcMode::Disabled, ObjectIcMode::Enabled] {
            let mut times = vec![];
            let mut stats = Default::default();
            for _ in 0..5 {
                let mut session = Session::new(program.clone()).unwrap();
                session.set_object_ic_mode(mode);
                let start = Instant::now();
                while session.status() == &Status::Paused {
                    engine.run(&mut session, 1_000_000).unwrap();
                }
                assert_eq!(session.status(), &Status::Finished);
                assert_eq!(session.context.output, "19999900000\n");
                times.push(start.elapsed().as_secs_f64());
                stats = session.object_stats();
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{name} {mode:?}: median={:.6}s reads={} writes={} misses={}",
                times[2], stats.native_reads, stats.native_writes, stats.native_misses
            );
        }
    }
}
