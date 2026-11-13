use std::{
    io::Write,
    process::{Command, Stdio},
};
fn cli(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cyallo"));
    command.current_dir(env!("CARGO_MANIFEST_DIR")).args(args);
    command
}
#[test]
fn moving_gc_cli_and_repl_reset_report_collections() {
    for engine in ["interpreter", "jit", "cranelift", "cranelift_speed"] {
        let result = cli(&[
            "run",
            "examples/simplified.psc",
            "--dialect",
            "simplified",
            "--engine",
            engine,
            "--gc-stress",
            "--jit-report",
        ])
        .output()
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"3:11:3:123\n");
        assert!(String::from_utf8_lossy(&result.stderr).contains("GC totals:"));
    }
    let result = piped_repl(
        "interpreter",
        "let a = [1,2]\n:reset\nlet b = [3,4]\nstd::io::print(b[1])\n",
        &["--gc-stress", "--jit-report"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"4\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("GC totals:"));
}
#[test]
fn extensions_select_dialects_and_simplified_is_explicit() {
    let strict = cli(&["check", "examples/loops.psc"]).output().unwrap();
    assert!(strict.status.success());
    assert_eq!(
        strict.stdout,
        "OK: CAIE 9618 compatibility (2027–2029)\n".as_bytes()
    );
    let modern = cli(&["check", "examples/tour.ya"]).output().unwrap();
    assert!(modern.status.success());
    assert_eq!(modern.stdout, b"OK: modern\n");
    let invalid = cli(&["check", "examples/simplified.psc"]).output().unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("E114"));
    let invalid = cli(&["check", "examples/loops.psc", "--dialect", "modern"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("E002"));
    let simplified = cli(&[
        "check",
        "examples/simplified.psc",
        "--dialect",
        "simplified",
    ])
    .output()
    .unwrap();
    assert!(simplified.status.success());
    assert!(String::from_utf8_lossy(&simplified.stdout).contains("simplified"));
    let invalid = cli(&[
        "check",
        "examples/simplified.psc",
        "--dialect",
        "compatibility",
    ])
    .output()
    .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("E114"));
}
#[test]
fn engines_run_simplified_source() {
    for engine in ["interpreter", "cranelift", "cranelift_speed"] {
        let result = cli(&[
            "run",
            "examples/simplified.psc",
            "--dialect",
            "simplified",
            "--engine",
            engine,
        ])
        .output()
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"3:11:3:123\n");
    }
}
#[test]
fn input_and_eof_are_reported() {
    for engine in ["interpreter", "cranelift", "cranelift_speed"] {
        let mut child = cli(&["run", "examples/input.psc", "--engine", engine])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"42\n").unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"Number?\n42\n");
        let eof = cli(&["run", "examples/input.psc", "--engine", engine])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!eof.status.success());
        let error = String::from_utf8_lossy(&eof.stderr);
        assert!(error.contains("R002"));
        assert!(error.contains("INPUT reached end of input"));
        assert!(!error.contains("src/main.rs"));
    }
}
fn piped_repl(engine: &str, source: &str, extra: &[&str]) -> std::process::Output {
    let mut command = cli(&["repl", "--engine", engine]);
    command.args(extra);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn repl_keeps_state_and_accepts_multiline_input() {
    for engine in ["interpreter", "cranelift", "cranelift_speed"] {
        let result = piped_repl(
            engine,
            "let mut Count = 0\nCount = Count + 1\nCount\nlet Arr = std::collections::Vec[123]\nArr[0]\nif true {\nstd::io::print(7)\n}\n:quit\n",
            &[],
        );
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"1\n123\n7\n");
        assert!(
            result.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
#[test]
fn repl_recovers_from_errors_and_computation_limits() {
    for engine in ["interpreter", "cranelift", "cranelift_speed"] {
        let result = piped_repl(
            engine,
            "let mut x = 1\nx = \"wrong\"\nx\nwhile true {\n}\nx + 1\n:reset\nx\n:quit\n",
            &["--max-instructions", "100"],
        );
        assert!(result.status.success());
        assert_eq!(result.stdout, b"1\n2\n");
        let errors = String::from_utf8_lossy(&result.stderr);
        assert!(errors.contains("E109"), "{errors}");
        assert!(errors.contains("instruction limit"), "{errors}");
        assert!(errors.contains("E106"), "{errors}");
    }
}
#[test]
fn repl_input_consumes_the_next_line() {
    let result = piped_repl(
        "interpreter",
        "let mut n = 0\nstd::runtime::caie::input(&mut n)\n42\nn\n:quit\n",
        &[],
    );
    assert!(result.status.success());
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"42\n");
}
#[test]
fn pretty_color_modes_preserve_plain_source() {
    let source = std::fs::read("examples/simplified.psc").unwrap();
    for mode in ["auto", "never"] {
        let result = cli(&[
            "pretty",
            "examples/simplified.psc",
            "--dialect",
            "compatibility",
            "--color",
            mode,
        ])
        .output()
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, source);
    }
    let result = cli(&["pretty", "examples/simplified.psc", "--color", "always"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stdout.contains(&0x1b));
}
#[test]
fn diagnostic_color_modes_work_when_redirected() {
    for mode in ["auto", "never", "always"] {
        let result = cli(&[
            "check",
            "examples/simplified.psc",
            "--dialect",
            "compatibility",
            "--color",
            mode,
        ])
        .output()
        .unwrap();
        assert!(!result.status.success());
        assert_eq!(result.stderr.contains(&0x1b), mode == "always");
    }
    let result = cli(&[
        "check",
        "examples/simplified.psc",
        "--dialect",
        "compatibility",
    ])
    .env("NO_COLOR", "1")
    .output()
    .unwrap();
    assert!(!result.stderr.contains(&0x1b));
}
#[test]
fn modern_syntax_and_bytecode_are_inspectable() {
    let modern = cli(&["run", "examples/modern.ya"]).output().unwrap();
    assert!(
        modern.status.success(),
        "{}",
        String::from_utf8_lossy(&modern.stderr)
    );
    assert_eq!(modern.stdout, b"ADA\n288212784234102776:10:3\n");
    let compat = cli(&["check", "examples/modern.ya", "--dialect", "compatibility"])
        .output()
        .unwrap();
    assert!(!compat.status.success());
    let bytecode = cli(&["bytecode", "examples/modern.ya"]).output().unwrap();
    assert!(bytecode.status.success());
    assert!(String::from_utf8_lossy(&bytecode.stdout).contains("Branch"));
    let wasm = cli(&["bytecode", "examples/modern.ya", "--wasm", "1"])
        .output()
        .unwrap();
    assert!(wasm.status.success());
    assert!(String::from_utf8_lossy(&wasm.stdout).starts_with("(module"));
}
#[test]
fn reports_show_metrics_without_changing_program_output() {
    for engine in ["jit", "interpreter", "cranelift", "cranelift_speed"] {
        let result = cli(&[
            "run",
            "examples/loops.psc",
            "--engine",
            engine,
            "--jit-trace",
        ])
        .output()
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"5050\n");
        let report = String::from_utf8_lossy(&result.stderr);
        for label in [
            "Execution report:",
            "VM execution:",
            "bytecode instructions:",
            "Timeline",
            "Executed by tier",
            "OSR transitions:",
        ] {
            assert!(report.contains(label), "missing {label}: {report}");
        }
        if engine == "jit" {
            assert!(report.contains("tier policy: interp -> winch"));
        }
    }
    let result = piped_repl(
        "cranelift_speed",
        "let n = 42\nn\n:quit\n",
        &["--jit-report"],
    );
    assert!(result.status.success());
    assert_eq!(result.stdout, b"42\n");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr)
            .matches("Execution report:")
            .count(),
        2
    );
}
#[test]
fn jit_report_preserves_inlining_and_has_compact_output() {
    let result = cli(&[
        "run",
        "examples/modern.ya",
        "--engine",
        "cranelift_speed",
        "--jit-report",
    ])
    .output()
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"ADA\n288212784234102776:10:3\n");
    let report = String::from_utf8_lossy(&result.stderr);
    assert!(
        report.contains("frameless calls: 65534"),
        "summary must preserve inlining"
    );
    assert!(
        report.lines().count() < 200,
        "summary must not print per-invocation events"
    );
}
#[test]
fn summary_repl_counts_only_the_current_entry() {
    let result = piped_repl(
        "cranelift_speed",
        "fn inc(n: i64) -> i64 { return n + 1 }\ninc(1)\ninc(2)\n:quit\n",
        &["--jit-report"],
    );
    assert!(result.status.success());
    assert_eq!(result.stdout, b"2\n3\n");
    let text = String::from_utf8_lossy(&result.stderr);
    let reports: Vec<_> = text.split("Execution report:").skip(1).collect();
    assert_eq!(reports.len(), 3);
    for report in &reports[1..] {
        assert!(report.contains("report mode: summary"));
        assert!(report.contains("inc: calls=1 instructions="));
        assert!(!report.contains("inc: calls=2"));
    }
    let work = |report: &str| {
        report
            .lines()
            .find(|line| line.contains("bytecode instructions:"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(work(reports[1]), work(reports[2]));
}
