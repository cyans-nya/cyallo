use cyallo::{
    CompileOptions, Dialect, Status,
    backend::{CraneliftBackend, Executable, NativeBackend},
    repl::{Repl, Submission},
};
fn entry(repl: &mut Repl, source: &str, jit: bool) {
    assert_eq!(repl.push_line(source).unwrap(), Submission::Ready);
    if jit {
        CraneliftBackend
            .compile(repl.session().program().clone())
            .unwrap()
            .run(repl.session_mut(), 100_000)
            .unwrap();
    } else {
        repl.run(100_000);
    }
    assert_eq!(repl.session().status(), &Status::Finished);
}
#[test]
fn state_is_persistent_without_replaying_output() {
    for jit in [false, true] {
        let mut repl = Repl::simplified().unwrap();
        entry(&mut repl, "Count <- 0\nOUTPUT \"once\"", jit);
        entry(&mut repl, "Count <- Count + 1", jit);
        entry(&mut repl, "Count", jit);
        assert_eq!(repl.session().context.output, "once\n1\n");
        entry(&mut repl, "Arr[10] <- 123", jit);
        entry(&mut repl, "Arr[10] + Count", jit);
        assert_eq!(repl.session().context.output, "once\n1\n124\n");
    }
}
#[test]
fn definitions_and_multiline_blocks() {
    let mut repl = Repl::simplified().unwrap();
    assert_eq!(
        repl.push_line("FUNCTION Twice(n : INTEGER) RETURNS INTEGER")
            .unwrap(),
        Submission::Incomplete
    );
    assert_eq!(
        repl.push_line("RETURN n * 2").unwrap(),
        Submission::Incomplete
    );
    assert_eq!(repl.push_line("ENDFUNCTION").unwrap(), Submission::Ready);
    repl.run(100);
    entry(&mut repl, "Twice(21)", true);
    assert_eq!(repl.session().context.output, "42\n");
    assert_eq!(
        repl.push_line("IF TRUE THEN").unwrap(),
        Submission::Incomplete
    );
    assert_eq!(repl.push_line("OUTPUT 7").unwrap(), Submission::Incomplete);
    assert_eq!(repl.push_line("ENDIF").unwrap(), Submission::Ready);
    repl.run(1000);
    assert_eq!(repl.session().context.output, "42\n7\n");
}
#[test]
fn errors_do_not_poison_subsequent_entries() {
    let mut repl = Repl::simplified().unwrap();
    entry(&mut repl, "Count <- 1", false);
    assert!(repl.push_line("Count <- \"wrong\"").is_err());
    assert!(!repl.is_pending());
    entry(&mut repl, "Count + 1", false);
    entry(&mut repl, "Arr[10] <- 123", false);
    assert_eq!(repl.push_line("Arr[0]").unwrap(), Submission::Ready);
    repl.run(1000);
    assert!(matches!(repl.session().status(), Status::Failed(_)));
    entry(&mut repl, "Arr[0] <- 7", false);
    entry(&mut repl, "Arr[0]", false);
    assert_eq!(repl.session().context.output, "2\n7\n");
}
#[test]
fn global_slots_can_move_when_declarations_are_added() {
    let mut repl = Repl::simplified().unwrap();
    entry(&mut repl, "Count <- 7", false);
    entry(&mut repl, "DECLARE Other : INTEGER\nOther <- 42", false);
    entry(&mut repl, "Count + Other", true);
    assert_eq!(repl.session().context.output, "49\n");
    assert!(repl.push_line("DECLARE Count : REAL").is_err());
    entry(&mut repl, "Count", false);
    assert_eq!(repl.session().context.output, "49\n7\n");
}
#[test]
fn strict_repl_requires_declarations() {
    let mut repl = Repl::new(CompileOptions {
        dialect: Dialect::Strict,
        ..Default::default()
    })
    .unwrap();
    assert!(repl.push_line("x <- 1").is_err());
    entry(&mut repl, "DECLARE x : INTEGER\nx <- 1", false);
    entry(&mut repl, "x", false);
    assert_eq!(repl.session().context.output, "1\n");
}
#[test]
fn input_and_random_state_are_not_replayed() {
    let mut repl = Repl::simplified().unwrap();
    entry(&mut repl, "n <- 0", false);
    assert_eq!(repl.push_line("INPUT n").unwrap(), Submission::Ready);
    assert_eq!(repl.run(100), &Status::WaitingForInput);
    repl.session_mut().context.input.push_back("42".into());
    repl.run(100);
    entry(&mut repl, "n", true);
    entry(&mut repl, "RAND(10)", false);
    let seed = repl.session().context.random_seed;
    entry(&mut repl, "DECLARE unused : INTEGER", true);
    assert_eq!(seed, repl.session().context.random_seed);
    assert!(repl.session().context.output.starts_with("42\n"));
}
#[test]
fn pending_entries_can_be_cancelled_and_state_reset() {
    let mut repl = Repl::simplified().unwrap();
    entry(&mut repl, "x <- 1", false);
    assert_eq!(
        repl.push_line("IF TRUE THEN").unwrap(),
        Submission::Incomplete
    );
    repl.discard_pending();
    entry(&mut repl, "x", false);
    repl.reset().unwrap();
    assert!(repl.session().globals().is_empty());
    assert!(repl.session().context.output.is_empty());
    assert!(repl.push_line("x").is_err());
}
#[test]
fn interrupted_entry_allows_recovery() {
    let mut repl = Repl::simplified().unwrap();
    assert_eq!(
        repl.push_line("WHILE TRUE\nENDWHILE").unwrap(),
        Submission::Ready
    );
    repl.run(100);
    repl.interrupt();
    entry(&mut repl, "42", true);
    assert_eq!(repl.session().context.output, "42\n");
}
