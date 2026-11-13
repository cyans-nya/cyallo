//! Syntax nesting is bounded with a diagnostic, and parsing stays linear.
use cyallo::{Session, Status, compile};
use std::{sync::Arc, time::Instant};

/// Compiler worker threads use the default 2 MiB stack.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn deep_nesting_is_a_diagnostic_not_a_stack_overflow() {
    for depth in [1_000, 100_000] {
        let source = format!("let a = {}1{}\n", "(".repeat(depth), ")".repeat(depth));
        let error = on_small_stack(move || compile(&source).unwrap_err());
        assert_eq!(error.code, "E002");
        assert!(error.message.contains("nesting"), "{error}");
    }
    let blocks = format!(
        "{}std::io::print(1){}\n",
        "if true { ".repeat(10_000),
        " }".repeat(10_000)
    );
    let error = on_small_stack(move || compile(&blocks).unwrap_err());
    assert!(error.message.contains("nesting"), "{error}");
}

#[test]
fn nesting_within_the_limit_compiles_and_runs_on_a_worker_stack() {
    let output = on_small_stack(|| {
        let source = format!("std::io::print({}1{})\n", "(".repeat(200), ")".repeat(200));
        let program = Arc::new(compile(&source).unwrap());
        let mut session = Session::new(program).unwrap();
        session.run(10_000);
        assert_eq!(session.status(), &Status::Finished);
        session.context.output.clone()
    });
    assert_eq!(output, "1\n");
}

#[test]
fn nested_closure_statements_parse_in_linear_time() {
    let mut source = String::from("f(0)");
    for _ in 0..60 {
        source = format!("f(proc() {{\n{source}\n}})");
    }
    let started = Instant::now();
    cyallo_parser_probe(&source);
    assert!(
        started.elapsed().as_secs() < 2,
        "took {:?}",
        started.elapsed()
    );
}

fn cyallo_parser_probe(source: &str) {
    // Name resolution fails (`f` is undefined); parsing must still finish.
    let _ = compile(source);
}
