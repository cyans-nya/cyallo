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
fn bytecode_can_verify_and_show_a_selected_routine() {
    let result = cli(&["bytecode", "examples/functions.psc", "--verified", "1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let disassembly = String::from_utf8(result.stdout).unwrap();
    assert!(disassembly.contains("verified bytecode v"));
    assert!(disassembly.contains("frame slots"));
    assert!(disassembly.contains("storage.dead"));
    assert!(disassembly.contains("call.direct f0("));
    assert!(disassembly.contains(".statement"));
    assert!(
        !disassembly
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("nop"))
    );

    let result = cli(&["bytecode", "examples/functions.psc", "--verified", "0"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("output.caie")
    );

    let unsupported = unsupported_program();
    let result = cli(&["bytecode", unsupported.to_str().unwrap(), "--verified", "0"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported"));
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
            "--jit-trace=summary",
        ])
        .output()
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"3:11:3:123\n");
        assert!(String::from_utf8_lossy(&result.stderr).contains("collections"));
    }
    let result = piped_repl(
        "interpreter",
        "let a = [1,2]\n:reset\nlet b = [3,4]\nstd::io::print(b[1])\n",
        &["--gc-stress", "--jit-trace=summary"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"4\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("collections"));
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
#[test]
fn verified_engine_reads_standard_input() {
    for (example, input, expected) in [
        ("examples/input.psc", "42\n", "Number?\n42\n"),
        (
            "examples/secret-number.ya",
            "50\nabc\n10\n42\n",
            "Guess a whole number from 1 to 100.\nTry lower.\nPlease enter a whole number.\nTry higher.\nCorrect!\n",
        ),
    ] {
        let mut child = cli(&["run", example, "--engine", "verified"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "{example}");
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            expected,
            "{example}"
        );
    }
    let eof = cli(&["run", "examples/input.psc", "--engine", "verified"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!eof.status.success());
    assert!(String::from_utf8_lossy(&eof.stderr).contains("INPUT reached end of input"));
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
            "VM execution",
            "bytecode instructions",
            "Timeline",
            "Tiers",
            "OSR transitions",
        ] {
            assert!(report.contains(label), "missing {label}: {report}");
        }
        if engine == "jit" {
            assert!(report.contains("interp -> winch"));
        }
    }
    let result = piped_repl(
        "cranelift_speed",
        "let n = 42\nn\n:quit\n",
        &["--jit-trace=summary"],
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
        "--jit-trace=summary",
    ])
    .output()
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"ADA\n288212784234102776:10:3\n");
    let report = String::from_utf8_lossy(&result.stderr);
    assert!(
        report
            .lines()
            .any(|line| line.trim_start().starts_with("frameless calls") && line.contains("65,534")),
        "summary must preserve inlining: {report}"
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
        &["--jit-trace=summary"],
    );
    assert!(result.status.success());
    assert_eq!(result.stdout, b"2\n3\n");
    let text = String::from_utf8_lossy(&result.stderr);
    let reports: Vec<_> = text.split("Execution report:").skip(1).collect();
    assert_eq!(reports.len(), 3);
    for report in &reports[1..] {
        assert!(report.contains("summary (counters only"));
        assert_eq!(routine_row(report, "inc").0, 1);
    }
    let work = |report: &str| {
        report
            .lines()
            .find(|line| line.contains("bytecode instructions"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(work(reports[1]), work(reports[2]));
}

#[test]
fn concurrency_language_examples_execute_through_the_cli() {
    for engine in ["interpreter", "jit", "cranelift", "cranelift_speed"] {
        for (path, expected) in [
            (
                "examples/concurrency/ownership/local-managed.ya",
                "42\n2\n7\ntrue\n",
            ),
            (
                "examples/concurrency/channels/buffered.ya",
                "true\n42\ntrue\n99\n",
            ),
            ("examples/concurrency/channels/rendezvous.ya", "42\n7\n"),
            ("examples/concurrency/channels/mpmc.ya", "21\n"),
            (
                "examples/concurrency/scheduling/clock.ya",
                "before\nafter\n",
            ),
            (
                "examples/concurrency/scheduling/cancellation.ya",
                "true\ntrue\n99\n",
            ),
        ] {
            let result = cli(&["run", path, "--engine", engine, "--gc-stress"])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{path} {engine}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(result.stdout, expected.as_bytes(), "{path} {engine}");
        }
    }
}

#[test]
fn runtime_import_examples_execute_and_report_published_functions() {
    for engine in ["interpreter", "jit", "cranelift", "cranelift_speed"] {
        for (path, expected) in [
            (
                "examples/modules/dynamic/main.ya",
                "runtime\n42\n42\ntrue\n",
            ),
            ("examples/concurrency/modules/main.ya", "42\n"),
        ] {
            let result = cli(&[
                "run",
                path,
                "--engine",
                engine,
                "--gc-stress",
                "--jit-trace",
            ])
            .output()
            .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(result.stdout, expected.as_bytes());
        }
    }
}

/// Value of the last column on the first line that starts with `label`.
fn kv_value(report: &str, label: &str) -> u64 {
    let line = report
        .lines()
        .find(|line| line.trim_start().starts_with(label))
        .unwrap_or_else(|| panic!("no {label} row: {report}"));
    line.split_whitespace()
        .last()
        .unwrap()
        .replace(',', "")
        .parse()
        .unwrap()
}

/// (calls, instructions) for a routine row in the Routines table.
fn routine_row(report: &str, name: &str) -> (u64, u64) {
    let fields: Vec<&str> = report
        .lines()
        .map(str::split_whitespace)
        .map(Iterator::collect::<Vec<_>>)
        .find(|fields| fields.first() == Some(&name))
        .unwrap_or_else(|| panic!("no routine row for {name}: {report}"));
    let number = |text: &str| text.replace(',', "").parse().unwrap();
    (number(fields[1]), number(fields[2]))
}

#[test]
fn trace_keeps_inlining_and_no_inline_turns_it_off() {
    let traced = cli(&[
        "run",
        "examples/modern.ya",
        "--engine",
        "cranelift_speed",
        "--jit-trace",
    ])
    .output()
    .unwrap();
    assert!(traced.status.success());
    assert_eq!(traced.stdout, b"ADA\n288212784234102776:10:3\n");
    let report = String::from_utf8_lossy(&traced.stderr);
    assert!(kv_value(&report, "frameless calls") > 0, "{report}");

    let untraced = cli(&[
        "run",
        "examples/modern.ya",
        "--engine",
        "cranelift_speed",
        "--jit-trace",
        "--jit-no-inline",
    ])
    .output()
    .unwrap();
    assert!(untraced.status.success());
    assert_eq!(untraced.stdout, b"ADA\n288212784234102776:10:3\n");
    let report = String::from_utf8_lossy(&untraced.stderr);
    assert_eq!(kv_value(&report, "frameless calls"), 0, "{report}");
}

#[test]
fn report_styles_follow_the_style_flag_and_color_policy() {
    let run = |extra: &[&str]| {
        let mut args = vec!["run", "examples/loops.psc", "--jit-trace=summary"];
        args.extend_from_slice(extra);
        cli(&args).output().unwrap()
    };
    let styled = |extra: &[&str]| String::from_utf8_lossy(&run(extra).stderr).into_owned();

    let color = styled(&["--color", "always"]);
    assert!(color.contains("\x1b["), "{color}");
    assert!(!styled(&["--color", "never"]).contains("\x1b["));
    assert!(!styled(&["--color", "always", "--jit-trace-style=plain"]).contains("\x1b["));
    let boxed = styled(&["--color", "never", "--jit-trace-style=boxed"]);
    assert!(boxed.contains("╭─") && boxed.contains("│ ") && !boxed.contains("\x1b["));
    assert!(styled(&["--color", "always", "--jit-trace-style=boxed"]).contains("\x1b["));
}

#[test]
fn json_report_is_one_object_and_stdout_is_unchanged() {
    let result = cli(&[
        "run",
        "examples/loops.psc",
        "--jit-trace=summary",
        "--jit-trace-format=json",
    ])
    .output()
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"5050\n");
    let text = String::from_utf8_lossy(&result.stderr);
    let json = text.trim();
    assert!(json.starts_with("{\"title\":\"Execution report:"), "{json}");
    assert!(json.ends_with('}'), "{json}");
    assert!(json.contains("\"kind\":\"table\""), "{json}");
    assert!(!json.contains("\x1b["));
}

#[test]
fn live_block_is_not_drawn_when_stderr_is_not_a_terminal() {
    let result = cli(&["run", "examples/loops.psc", "--jit-trace"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8_lossy(&result.stderr);
    assert!(text.contains("Execution report:"), "{text}");
    assert!(!text.contains("JIT live"), "{text}");
    assert!(!text.contains("\x1b["), "{text}");
}

#[test]
fn json_trace_carries_the_timeline_for_the_viewer() {
    let result = cli(&[
        "run",
        "examples/mergesort.ya",
        "--jit-trace",
        "--jit-trace-format=json",
    ])
    .output()
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"398403790:1921:999272\n");
    let text = String::from_utf8_lossy(&result.stderr);
    let json = text.trim();
    assert!(
        json.contains("\"trace\":{\"version\":1,\"mode\":\"trace\""),
        "{json}"
    );
    for kind in [
        "\"kind\":\"install\"",
        "\"kind\":\"compile_end\"",
        "\"kind\":\"osr\"",
    ] {
        assert!(json.contains(kind), "missing {kind}");
    }
    assert!(json.contains("\"duration_ms\":"));
    assert!(json.contains("\"engines\":{\"interpreter\":"));
    assert!(!json.contains("JIT live"));
}

#[test]
fn trace_viewer_needs_an_interactive_terminal() {
    let result = cli(&["trace", "examples/mergesort.ya"]).output().unwrap();
    assert!(!result.status.success());
    let text = String::from_utf8_lossy(&result.stderr);
    assert!(text.contains("interactive"), "{text}");
}

#[test]
fn jit_trace_out_writes_the_report_to_a_file_and_keeps_stderr_quiet() {
    let path = std::env::temp_dir().join(format!("cyallo-trace-out-{}.json", std::process::id()));
    let result = cli(&[
        "run",
        "examples/mergesort.ya",
        "--jit-trace",
        "--jit-trace-format=json",
        "--jit-trace-out",
        path.to_str().unwrap(),
    ])
    .output()
    .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"398403790:1921:999272\n");
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert!(text.contains("\"trace\":{\"version\":1"), "{text}");

    let rejected = cli(&["run", "examples/mergesort.ya", "--jit-trace-out", "x.json"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("needs --jit-trace"));
}

#[test]
fn enum_examples_execute_through_the_cli() {
    for engine in ["interpreter", "jit", "cranelift", "cranelift_speed"] {
        for (path, expected) in [
            ("examples/enums/result.ya", "4\ncannot divide by 0\n"),
            ("examples/enums/patterns.ya", "positive: 7\nchanged\n2\n"),
            ("examples/enums/ownership.ya", "first:1:1:true\n"),
            ("examples/enums/recursive.ya", "5\n"),
        ] {
            let result = cli(&["run", path, "--engine", engine, "--gc-stress"])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{path} ({engine}): {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&result.stdout), expected);
        }
    }
}

#[test]
fn verified_engine_matches_the_interpreter_on_supported_programs() {
    for example in [
        "examples/functions.psc",
        "examples/loops.psc",
        "examples/fib.ya",
        "examples/arrays.psc",
        "examples/strings.psc",
        "examples/composites.ya",
        "examples/hot-loop.ya",
        "examples/enums/recursive.ya",
        "examples/rc-ownership.ya",
        "examples/enums/ownership.ya",
        "examples/mergesort.ya",
        "examples/quicksort.ya",
        "examples/knapsack.ya",
        "examples/gc-cycles.ya",
        "examples/gc-sharing.ya",
        "examples/modern.ya",
        "examples/tour.ya",
        "examples/generics.ya",
        "examples/parser.ya",
        "examples/postfix-dereference.ya",
        "examples/enums/patterns.ya",
        "examples/enums/result.ya",
        "examples/gc-rc.ya",
        "examples/dfs.ya",
        "examples/traits.ya",
        "examples/lookups.ya",
        "examples/locations.ya",
        "examples/managed.ya",
        "examples/concurrency/generators/sequence.ya",
        "examples/concurrency/generators/resume-result.ya",
        "examples/concurrency/generators/delegation.ya",
        "examples/concurrency/channels/buffered.ya",
        "examples/concurrency/channels/rendezvous.ya",
        "examples/concurrency/channels/mpmc.ya",
        "examples/concurrency/scheduling/cancellation.ya",
        "examples/concurrency/scheduling/clock.ya",
        "examples/concurrency/enums.ya",
        "examples/concurrency/status-enums.ya",
        "examples/objects/open.ya",
        "examples/objects/native-ic.ya",
        "examples/modules/static/main.ya",
        "examples/modules/dynamic/main.ya",
        "examples/services/main.ya",
        "examples/lookup-services/main.ya",
        "examples/objects/views.ya",
        "examples/concurrency/modules/main.ya",
    ] {
        let run = |engine| cli(&["run", example, "--engine", engine]).output().unwrap();
        let verified = run("verified");
        assert!(verified.status.success(), "{example}");
        assert_eq!(verified.stdout, run("interpreter").stdout, "{example}");
    }
}

/// A program the verified engine cannot lower yet: it keeps a character, which
/// has no verified representation.
fn unsupported_program() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("cyallo-unsupported-{}.ya", std::process::id()));
    std::fs::write(&path, "let c = 'a'\nstd::io::print(c)\n").unwrap();
    path
}

#[test]
fn verified_engine_refuses_unsupported_programs_before_running() {
    let unsupported = unsupported_program();
    let result = cli(&["run", unsupported.to_str().unwrap(), "--engine", "verified"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported"));
}

#[test]
fn the_default_engine_is_the_verified_interpreter() {
    // Output and input match the older engines.
    for example in [
        "examples/fib.ya",
        "examples/composites.ya",
        "examples/rc-ownership.ya",
    ] {
        let default = cli(&["run", example]).output().unwrap();
        let older = cli(&["run", example, "--engine", "interpreter"])
            .output()
            .unwrap();
        assert!(default.status.success(), "{example}");
        assert_eq!(default.stdout, older.stdout, "{example}");
    }
    let mut child = cli(&["run", "examples/secret-number.ya"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"50\nabc\n42\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&result.stdout).ends_with("Correct!\n"));
    // A failure is a diagnostic with a code and the failing source line.
    let failing = std::env::temp_dir().join(format!("cyallo-default-{}.ya", std::process::id()));
    std::fs::write(&failing, "let zero = 0\nstd::io::print(1 / zero)\n").unwrap();
    let result = cli(&["run", failing.to_str().unwrap()]).output().unwrap();
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success());
    assert!(error.contains("R100"), "{error}");
    assert!(error.contains("1 / zero"), "{error}");
}

#[test]
fn programs_the_verified_engine_cannot_lower_fall_back_unless_it_is_asked_for() {
    let path = unsupported_program();
    let fallback = cli(&["run", path.to_str().unwrap()]).output().unwrap();
    assert!(fallback.status.success());
    assert_eq!(fallback.stdout, b"a\n");
    let asked = cli(&["run", path.to_str().unwrap(), "--engine", "verified"])
        .output()
        .unwrap();
    assert!(!asked.status.success());
    assert!(asked.stdout.is_empty());
}
