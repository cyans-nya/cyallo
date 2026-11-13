use cyallo::{
    Session,
    backend::{CraneliftConfig, Executable, NativeBackend},
    compile,
    inspection::{Inspection, InspectionOptions},
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "caie-inspection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cyallo"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn emission_leaves_cold_bodies_deferred_and_explicit_check_emits_all() {
    let scratch = Scratch::new();
    let source = scratch.0.join("source.ya");
    let dump = scratch.0.join("dump");
    std::fs::write(
        &source,
        "fn unused() -> i64 { return missing }\nstd::io::print(42)\n",
    )
    .unwrap();
    let args = [
        "run",
        source.to_str().unwrap(),
        "--engine",
        "interpreter",
        "--emit",
        "ir,bytecode",
        "--emit-dir",
        dump.to_str().unwrap(),
    ];
    let output = cli(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"42\n");
    assert!(
        std::fs::read_to_string(dump.join("p1-f0.ir"))
            .unwrap()
            .contains("registers")
    );
    assert!(dump.join("p1-f0.bytecode").exists());
    assert!(!dump.join("p1-f1.ir").exists());
    assert!(
        std::fs::read_to_string(dump.join("p1-manifest.txt"))
            .unwrap()
            .contains("unused: deferred")
    );
    std::fs::write(
        &source,
        "fn unused() -> i64 { return 3 }\nstd::io::print(42)\n",
    )
    .unwrap();
    let output = cli(&[
        "check",
        source.to_str().unwrap(),
        "--emit",
        "ir,bytecode",
        "--emit-dir",
        dump.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dump.join("p1-f1.ir").exists());
}

#[test]
fn clif_emits_before_and_after_optimization_without_disabling_inlining() {
    let scratch = Scratch::new();
    let source = scratch.0.join("source.ya");
    let dump = scratch.0.join("dump");
    std::fs::write(
        &source,
        "fn inc(n: i64) -> i64 { return n + 1 }\nstd::io::print(inc(41))\n",
    )
    .unwrap();
    let output = cli(&[
        "run",
        source.to_str().unwrap(),
        "--engine",
        "cranelift_speed",
        "--jit-report",
        "--emit",
        "ir,bytecode,clif",
        "--emit-dir",
        dump.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"42\n");
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(report.contains("frameless calls: 1"));
    assert!(report.contains("[inline]"));
    assert!(report.contains("[opt]"));
    let files = std::fs::read_dir(&dump)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    for suffix in [".before.clif", ".after.clif"] {
        let paths = files
            .iter()
            .filter(|p| p.to_string_lossy().ends_with(suffix))
            .collect::<Vec<_>>();
        assert!(!paths.is_empty());
        for path in paths {
            assert!(std::fs::read_to_string(path).unwrap().contains("function"));
        }
    }
}

#[test]
fn clif_finalization_drains_pending_jobs_without_checking_unrelated_bodies() {
    let scratch = Scratch::new();
    let source = scratch.0.join("source.ya");
    let dump = scratch.0.join("dump");
    // Enough loop work to cross the default Cranelift-fast threshold.
    std::fs::write(&source,"fn work(n: i64) -> i64 { let mut sum = 0; for i in 1..=n { sum += i }; return sum }\nfn unused() -> i64 { return missing }\nstd::io::print(work(100000))\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cyallo"))
        .args([
            "run",
            source.to_str().unwrap(),
            "--emit",
            "clif",
            "--emit-dir",
            dump.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"5000050000\n");
    let files = std::fs::read_dir(&dump)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let before = files
        .iter()
        .filter(|p| p.ends_with(".before.clif"))
        .collect::<Vec<_>>();
    assert!(!before.is_empty());
    for name in before {
        assert!(files.contains(&name.replace(".before.clif", ".after.clif")));
    }
    assert!(
        std::fs::read_to_string(dump.join("p1-manifest.txt"))
            .unwrap()
            .contains("unused: deferred")
    );
}

#[test]
fn unfulfilled_attributes_require_the_explicit_warning_flag() {
    let scratch = Scratch::new();
    let source = scratch.0.join("source.ya");
    std::fs::write(
        &source,
        "#[opt(eager)]\nfn tiny(n: i64) -> i64 { return n + 1 }\nstd::io::print(42)\n",
    )
    .unwrap();
    for warn in [false, true] {
        let mut args = vec!["run", source.to_str().unwrap(), "--jit-notices"];
        if warn {
            args.push("--warn-unfulfilled-attributes");
        }
        let output = cli(&args);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("[unfulfilled]"),
            warn
        );
    }
    let bad_dump = source.join("not-a-directory");
    let output = cli(&[
        "check",
        source.to_str().unwrap(),
        "--emit",
        "ir",
        "--emit-dir",
        bad_dump.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to emit"));
}

#[test]
fn overflow_falls_back_and_stale_artifacts_report_invalidation() {
    let observer = Arc::new(Inspection::new(InspectionOptions {
        notices: true,
        ..Default::default()
    }));
    let p = Arc::new(
        compile(
            "fn inc(n: i64) -> i64 { return n + 1 }\nstd::io::print(inc(9223372036854775807))\n",
        )
        .unwrap()
        .with_inspection(observer.clone()),
    );
    let a = CraneliftConfig::speed().compile(p.clone()).unwrap();
    let mut s = Session::new(p).unwrap();
    a.run(&mut s, 1000).unwrap();
    assert!(
        observer
            .notices()
            .iter()
            .any(|n| n.category == "fallback" && n.message.contains("arithmetic")),
        "{}",
        observer.render()
    );
    let p = Arc::new(compile("std::io::print(42)\n").unwrap());
    let mut s = Session::new(p).unwrap();
    assert!(a.run(&mut s, 1000).is_err());
    assert!(
        observer
            .notices()
            .iter()
            .any(|n| n.category == "invalidation")
    );
    assert!(s.context.output.is_empty());
}

#[test]
fn parallel_notices_are_bounded_and_coalesced_and_windows_are_independent() {
    let observer = Inspection::new(InspectionOptions {
        notices: true,
        ..Default::default()
    });
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let observer = &observer;
            scope.spawn(move || {
                for _ in 0..100 {
                    observer.notice("inline", "f", "same");
                }
            });
        }
    });
    assert_eq!(observer.notices()[0].count, 800);
    for i in 0..600 {
        observer.notice("opt", "f", i.to_string());
    }
    assert_eq!(observer.notices().len(), 256);
    assert!(observer.render().contains("omitted"));
    assert!(observer.window().notices().is_empty());
}

#[test]
fn repl_dumps_use_distinct_snapshot_names() {
    use std::{io::Write, process::Stdio};
    let scratch = Scratch::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cyallo"))
        .args([
            "repl",
            "--engine",
            "cranelift",
            "--emit",
            "ir,bytecode",
            "--emit-dir",
            scratch.0.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"#[inline]\nfn inc(n: i64) -> i64 { return n + 1 }\nstd::io::print(inc(41))\nstd::io::print(inc(42))\n",
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"42\n43\n");
    for snapshot in 1..=3 {
        assert!(scratch.0.join(format!("p{snapshot}-manifest.txt")).exists());
    }
}
