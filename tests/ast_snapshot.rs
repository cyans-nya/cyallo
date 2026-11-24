use cyallo::{
    CompileOptions, Jobs, Session, Status, compile_snapshot, compile_snapshot_for_execution,
};
use cyallo_parser::{ParsedFile, parse_modern_snapshot};
use std::sync::Arc;

fn snapshot(source: &str) -> Arc<ParsedFile> {
    let text = Arc::<str>::from(source);
    let parsed = parse_modern_snapshot(Arc::clone(&text), false, Jobs(4)).unwrap();
    drop(text);
    Arc::new(parsed)
}

#[test]
fn snapshot_compiles_modules_and_uses_deferred_body_checks() {
    let parsed = snapshot(concat!(
        "mod calculation {\n",
        " #[opt(always_lazy)]\n pub fn answer(x: i64) -> i64 { return x + 1 }\n",
        "}\n",
        "use std::io::print\nprint(calculation::answer(41))\n",
    ));
    let options = CompileOptions {
        jobs: 1,
        ..Default::default()
    };
    let eager = compile_snapshot(Arc::clone(&parsed), options).unwrap();
    let deferred = compile_snapshot_for_execution(parsed, options).unwrap();
    let answer = deferred
        .functions()
        .iter()
        .find(|routine| routine.name.as_ref() == "answer")
        .unwrap();
    assert!(!answer.is_analyzed());
    for program in [eager, deferred] {
        let mut session = Session::new(Arc::new(program)).unwrap();
        session.run(1_000);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "42\n");
    }
}

#[test]
fn snapshot_keeps_the_same_diagnostic_span_as_source_compilation() {
    let source = "mod nested { pub fn fail() -> i64 { return unknown_name } }\n";
    let expected = cyallo::compile(source).unwrap_err();
    let error = compile_snapshot(snapshot(source), CompileOptions::default()).unwrap_err();
    assert_eq!(error, expected);
    assert_eq!(error.code, "E106");
    assert_eq!(error.span.source_text(source), "unknown_name");
}

#[test]
fn compatibility_compilation_preserves_case_folded_names() {
    let source = "DECLARE Count : INTEGER\ncount <- 41\nOUTPUT Count + 1\n";
    let options = CompileOptions {
        dialect: cyallo::Dialect::Caie27,
        ..Default::default()
    };
    for program in [
        cyallo::compile_with_options(source, options).unwrap(),
        cyallo::compile_for_execution(source, options).unwrap(),
    ] {
        let mut session = Session::new(Arc::new(program)).unwrap();
        session.run(100);
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "42\n");
    }
}
