use cyallo::{
    CompileOptions, Dialect, Session, Status, compile_with_options,
    debugger::{Debugger, Mode},
};
use std::sync::Arc;
fn debugger(source: &str) -> Debugger {
    let program = compile_with_options(
        source,
        CompileOptions {
            dialect: Dialect::Compatibility,
            ..Default::default()
        },
    )
    .unwrap();
    Debugger::new(Session::new(Arc::new(program)).unwrap())
}
fn command(d: &mut Debugger, mode: Mode) {
    d.command(mode);
    d.tick(10_000);
}
#[test]
fn steps_into_over_and_out() {
    let mut d = debugger(
        "DECLARE x : INTEGER\nPROCEDURE AssignValue()\n x <- 42\nENDPROCEDURE\nCALL AssignValue()\nOUTPUT x\n",
    );
    assert_eq!(d.line(), Some(1));
    command(&mut d, Mode::Into);
    assert_eq!(d.line(), Some(5));
    command(&mut d, Mode::Into);
    assert_eq!(d.line(), Some(3));
    assert_eq!(d.session.depth(), 2);
    command(&mut d, Mode::Out(2));
    assert_eq!(d.line(), Some(6));
    assert_eq!(d.session.depth(), 1);
    assert!(d.session.variables().contains(&("x".into(), "42".into())));
    d.restart().unwrap();
    command(&mut d, Mode::Into);
    command(&mut d, Mode::Over(1));
    assert_eq!(d.line(), Some(6));
    command(&mut d, Mode::Continue);
    assert_eq!(d.session.context.output, "42\n");
    assert_eq!(d.session.status(), &Status::Finished);
}
#[test]
fn breakpoints_and_restart() {
    let mut d = debugger("DECLARE i : INTEGER\nFOR i <- 1 TO 3\n OUTPUT i\nNEXT i\n");
    d.breakpoints.insert(3);
    command(&mut d, Mode::Continue);
    assert_eq!(d.line(), Some(3));
    assert!(d.session.context.output.is_empty());
    command(&mut d, Mode::Continue);
    assert_eq!(d.session.context.output, "1\n");
    assert_eq!(d.line(), Some(3));
    d.restart().unwrap();
    assert_eq!(d.line(), Some(1));
    assert!(d.session.context.output.is_empty());
    assert!(d.breakpoints.contains(&3));
}
#[test]
fn input_suspends_and_resumes() {
    let mut d = debugger("DECLARE n : INTEGER\nINPUT n\nOUTPUT n\n");
    command(&mut d, Mode::Continue);
    assert_eq!(d.session.status(), &Status::WaitingForInput);
    d.session.context.input.push_back("7".into());
    command(&mut d, Mode::Continue);
    assert_eq!(d.session.context.output, "7\n");
    assert_eq!(d.session.status(), &Status::Finished);
}
#[test]
fn infinite_loops_can_be_paused() {
    let mut d = debugger("WHILE TRUE\nENDWHILE\n");
    d.command(Mode::Continue);
    d.tick(100);
    assert_eq!(d.mode, Mode::Continue);
    d.command(Mode::Paused);
    let span = d.session.current_span();
    d.tick(100);
    assert_eq!(d.session.current_span(), span);
}
#[test]
fn restart_restores_random_seed() {
    let mut d = debugger("OUTPUT RAND(10)\n");
    command(&mut d, Mode::Continue);
    let first = d.session.context.output.clone();
    d.restart().unwrap();
    command(&mut d, Mode::Continue);
    assert_eq!(first, d.session.context.output);
}
