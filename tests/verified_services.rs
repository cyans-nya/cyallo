//! Typed service contexts through the verified interpreter. Expected lines and
//! failures are written by hand from docs/services.md: a reference finds the
//! provider the context sees at each call, withdrawing a provider makes new uses
//! fail, isolation separates one key, dependents clean up before their provider,
//! and a disposed context accepts nothing.

use cyallo::{
    bytecode::verified::{
        ExecutionLimits, ExecutionStatus, OutputSink, RunError, ServiceFault, VerifiedModule,
    },
    modules::{SourceModule, compile_modules},
};
use std::sync::{Arc, Mutex};

const LOGGING: &str = r#"
use std::service::{Context, ServiceKey, ServiceRef}
pub trait Logger { proc info(self: &Self, message: string) }
pub const LOGGER = ServiceKey::new<dyn Logger>("test.logger")
pub property Context.logger: ServiceRef<dyn Logger> {
    proc get() { return self.service(LOGGER) }
}
pub record Named { name: string }
impl Logger for Named {
    proc info(self: &Self, message: string) { std::io::print(self.name, ": ", message) }
}
pub proc named(name: string) -> dyn Logger { return Named { name: name } as dyn Logger }
"#;

fn run(main: &str, stress: bool) -> (Vec<String>, ExecutionStatus) {
    let sources = [
        SourceModule::new("logging.ya", LOGGING),
        SourceModule::new(
            "main.ya",
            format!("mod logging;\nuse crate::logging::{{LOGGER, named, logger}}\n{main}"),
        ),
    ];
    let program = compile_modules(1, &sources, Default::default(), false)
        .unwrap_or_else(|error| panic!("{error:?}\n{main}"));
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 5_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_gc_stress(stress);
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    let status = activation.resume(usize::MAX);
    (lines.lock().unwrap().clone(), status)
}

fn lines(main: &str) -> Vec<String> {
    let (lines, status) = run(main, true);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines
}

fn fault(main: &str) -> ServiceFault {
    match run(main, false).1 {
        ExecutionStatus::Failed(RunError::Service { fault, .. }) => fault,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reference_sees_the_provider_the_context_has_at_each_call() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
let reference = ctx.logger
let first = ctx.provide(LOGGER, named("first"))
reference.info("one")
first.dispose()
std::io::print(ctx.available(LOGGER))
let second = ctx.provide(LOGGER, named("second"))
reference.info("two")
std::io::print(ctx.available(LOGGER))
"#;
    assert_eq!(lines(main), ["first: one", "false", "second: two", "true"]);
}

#[test]
fn using_a_withdrawn_provider_is_a_typed_failure() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
let registration = ctx.provide(LOGGER, named("gone"))
registration.dispose()
ctx.logger.info("late")
"#;
    assert_eq!(fault(main), ServiceFault::NotInjected);
}

#[test]
fn a_key_has_one_provider_per_realm() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
let first = ctx.provide(LOGGER, named("a"))
let second = ctx.provide(LOGGER, named("b"))
"#;
    assert_eq!(fault(main), ServiceFault::ProviderConflict);
}

#[test]
fn an_extended_context_inherits_and_an_isolated_one_does_not() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
let root = ctx.provide(LOGGER, named("root"))
let child = ctx.extend()
child.logger.info("inherited")
let apart = ctx.isolate(LOGGER)
std::io::print(apart.available(LOGGER))
let own = apart.provide(LOGGER, named("apart"))
apart.logger.info("own")
ctx.logger.info("unchanged")
"#;
    assert_eq!(
        lines(main),
        ["root: inherited", "false", "apart: own", "root: unchanged"]
    );
}

#[test]
fn dependents_clean_up_before_their_provider_and_disposal_is_idempotent() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
let registration = ctx.provide(LOGGER, named("p"))
ctx.inject(LOGGER, proc(scoped: Context) {
    scoped.logger.info("setup")
    scoped.on_dispose(proc() { std::io::print("cleanup") })
})
registration.dispose()
registration.dispose()
std::io::print("done")
"#;
    assert_eq!(lines(main), ["p: setup", "cleanup", "done"]);
}

#[test]
fn a_disposed_context_accepts_nothing() {
    let main = r#"
use std::service::Context
let ctx = Context::new()
ctx.dispose()
let child = ctx.extend()
"#;
    assert_eq!(fault(main), ServiceFault::InactiveContext);
}

#[test]
fn the_services_example_runs_under_gc_stress() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/services");
    let sources = ["logging.ya", "console.ya", "main.yas"]
        .map(|name| SourceModule::new(name, std::fs::read_to_string(dir.join(name)).unwrap()));
    let program = compile_modules(2, &sources, Default::default(), false).unwrap();
    let module = VerifiedModule::lower(&program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 5_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_gc_stress(true);
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    assert!(matches!(
        activation.resume(usize::MAX),
        ExecutionStatus::Returned(None)
    ));
    assert_eq!(
        *lines.lock().unwrap(),
        ["first: ready", "first: cleanup", "second: ready", "false"]
    );
}
