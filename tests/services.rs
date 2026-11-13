use cyallo::{Session, Status, compile};
use std::sync::Arc;
const INTERFACE: &str = r#"
trait Logger { proc info(self: &mut Self, message: string) }
record Console { prefix: string }
impl Logger for Console { proc info(self: &mut Self, message: string) { std::io::print(self.prefix, message) } }
const LOGGER = std::service::ServiceKey::new<dyn Logger>("logger")
"#;
#[test]
fn typed_services_smoke() {
    let source = format!(
        "{INTERFACE}\nlet ctx = std::service::Context::new()\nlet provider = Console {{ prefix: \"app: \" }} as dyn Logger\nlet registration = ctx.provide(LOGGER, provider)\nctx.service(LOGGER).info(\"ready\")\nregistration.dispose()\nstd::io::print(ctx.available(LOGGER))\n"
    );
    let p = Arc::new(compile(&source).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(10000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "app: ready\nFALSE\n");
    assert_eq!(s.service_stats().released, 1);
}
#[test]
fn imported_extension_getters() {
    let logging = cyallo::modules::SourceModule::new(
        "logging.ya",
        r#"
pub trait Logger { proc info(self: &mut Self, message: string) }
pub const LOGGER = std::service::ServiceKey::new<dyn Logger>("logger")
pub property std::service::Context.logger: std::service::ServiceRef<dyn Logger> { proc get() { return self.service(LOGGER) } }
"#,
    );
    let console = cyallo::modules::SourceModule::new(
        "console.ya",
        r#"
use crate::logging::Logger
use crate::logging::LOGGER
record Console { prefix: string }
impl Logger for Console { proc info(self: &mut Self, message: string) { std::io::print(self.prefix,message) } }
pub proc install(ctx: &std::service::Context) -> std::service::Registration { return ctx.provide(LOGGER,Console { prefix: "app: " } as dyn Logger) }
"#,
    );
    let main = cyallo::modules::SourceModule::new(
        "main.ya",
        r#"mod console;
mod logging;

use crate::logging::logger
use crate::console::install
let ctx = std::service::Context::new()
let registration = install(&ctx)
ctx.logger.info("ready")
registration.dispose()
"#,
    );
    let p = Arc::new(
        cyallo::modules::compile_modules(2, &[logging, console, main], Default::default(), false)
            .unwrap(),
    );
    let mut s = Session::new(p).unwrap();
    s.run(10000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "app: ready\n");
}

fn engines(p: &Arc<cyallo::ir::Program>) -> Vec<Box<dyn cyallo::backend::Executable>> {
    use cyallo::backend::{
        CraneliftBackend, CraneliftSpeedBackend, NativeBackend, Tier, TieredBackend,
    };
    let baseline = TieredBackend::default().compile(p.clone()).unwrap();
    for i in 0..p.functions().len() {
        baseline.precompile(i, Tier::Winch).unwrap();
    }
    assert!(baseline.wait_for_compilation(std::time::Duration::from_secs(30)));
    vec![
        Box::new(baseline),
        Box::new(CraneliftBackend.compile(p.clone()).unwrap()),
        Box::new(CraneliftSpeedBackend.compile(p.clone()).unwrap()),
    ]
}
fn parity(source: &str, expected: &str) {
    for deferred in [false, true] {
        let p = Arc::new(
            if deferred {
                cyallo::compile_for_execution(source, Default::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let mut reference = Session::new(p.clone()).unwrap();
        reference.run(100000);
        assert_eq!(reference.status(), &Status::Finished);
        assert_eq!(reference.context.output, expected);
        for gc in [
            cyallo::runtime::gc::GcConfig::default(),
            cyallo::runtime::gc::GcConfig::stress(),
        ] {
            for engine in engines(&p) {
                let mut s = Session::with_context(
                    p.clone(),
                    cyallo::Context {
                        gc,
                        ..Default::default()
                    },
                )
                .unwrap();
                for step in 0..100000 {
                    if step % 3 == 0 {
                        s.run(1);
                    } else {
                        engine.run(&mut s, 1).unwrap();
                    }
                    if s.status() != &Status::Paused {
                        break;
                    }
                }
                assert_eq!(s.status(), reference.status());
                assert_eq!(s.context.output, expected);
                assert_eq!(s.instruction_count(), reference.instruction_count());
            }
        }
    }
}
#[test]
fn replacement_and_native_pic() {
    let source = format!(
        r#"{INTERFACE}
record Other {{ prefix: string }}
impl Logger for Other {{ proc info(self: &mut Self, message: string) {{ std::io::print(self.prefix,message) }} }}
let ctx = std::service::Context::new()
let live = ctx.service(LOGGER)
proc log(logger: &std::service::ServiceRef<dyn Logger>, message: string) {{ logger.info(message) }}
let mut registration = ctx.provide(LOGGER,Console {{ prefix: "A" }} as dyn Logger)
for i in 0..=3 {{ log(&live, "1") }}
registration.dispose()
registration = ctx.provide(LOGGER,Console {{ prefix: "B" }} as dyn Logger)
log(&live, "2")
registration.dispose()
registration = ctx.provide(LOGGER,Other {{ prefix: "C" }} as dyn Logger)
log(&live, "3")
registration.dispose()
std::io::print(ctx.available(LOGGER))
"#
    );
    parity(&source, "A1\nA1\nA1\nA1\nB2\nC3\nFALSE\n");
}
#[test]
fn injection_isolation_and_cleanup_before_release() {
    let source = format!(
        r#"{INTERFACE}
let root = std::service::Context::new()
let registration = root.provide(LOGGER,Console {{ prefix: "root:" }} as dyn Logger)
root.inject(LOGGER,proc(scoped: std::service::Context) {{
    scoped.service(LOGGER).info("setup")
    let child = scoped.extend()
    child.service(LOGGER).info("child")
    scoped.on_dispose(proc() {{ scoped.service(LOGGER).info("cleanup") }})
}})
let isolated = root.isolate(LOGGER)
std::io::print(isolated.available(LOGGER))
let own = isolated.provide(LOGGER,Console {{ prefix: "inner:" }} as dyn Logger)
isolated.service(LOGGER).info("ready")
registration.dispose()
isolated.service(LOGGER).info("still")
own.dispose()
std::io::print(root.available(LOGGER))
"#
    );
    parity(
        &source,
        "root:setup\nroot:child\nFALSE\ninner:ready\nroot:cleanup\ninner:still\nFALSE\n",
    );
}
#[test]
fn revoked_refs_cannot_bypass_admission() {
    let source = format!(
        r#"{INTERFACE}
let root = std::service::Context::new()
let registration = root.provide(LOGGER,Console {{ prefix: "" }} as dyn Logger)
let live = root.service(LOGGER)
registration.dispose()
live.info("must not run")
"#
    );
    let p = Arc::new(compile(&source).unwrap());
    for engine in engines(&p) {
        let mut s = Session::new(p.clone()).unwrap();
        engine.run(&mut s, 10000).unwrap();
        assert!(matches!(s.status(), Status::Failed(_)));
        assert_eq!(s.context.output, "");
    }
}

#[test]
fn stable_slots_and_native_targets_are_observable() {
    let source = format!(
        r#"{INTERFACE}
let ctx = std::service::Context::new()
let mut registration = ctx.provide(LOGGER,Console {{ prefix: "" }} as dyn Logger)
let live = ctx.service(LOGGER)
proc emit(logger: &std::service::ServiceRef<dyn Logger>, message: string) {{ logger.info(message) }}
for i in 1..=20 {{ emit(&live, "x") }}
registration.dispose()
registration = ctx.provide(LOGGER,Console {{ prefix: "" }} as dyn Logger)
for i in 1..=20 {{ emit(&live, "y") }}
registration.dispose()
"#
    );
    let p = Arc::new(compile(&source).unwrap());
    for engine in engines(&p) {
        let mut s = Session::new(p.clone()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.service_stats().target_misses, 1);
        assert!(s.service_stats().native_target_hits > 30);
        assert_eq!(s.service_stats().slot_hits, 40);
        assert_eq!(s.service_stats().released, 2);
        assert_eq!(s.service_stats().resolutions, 5);
    }
}
#[test]
fn pure_erased_methods_keep_their_effect_contract() {
    parity(
        "trait Readable { fn value(self) -> i64 }\nrecord R { n: i64 }\nimpl Readable for R { fn value(self) -> i64 { return self.n } }\nproc inspect_value(x: dyn Readable) -> i64 { return x.value() }\nlet x = R { n: 7 } as dyn Readable\nstd::io::print(inspect_value(x))\n",
        "7\n",
    );
}
#[test]
fn provider_cleanup_follows_dependent_cleanup() {
    let source = format!(
        r#"{INTERFACE}
let root = std::service::Context::new()
let plugin = root.extend()
let registration = plugin.provide(LOGGER,Console {{ prefix: "" }} as dyn Logger)
let cleanup_plugin = std::mem::clone(&plugin)
plugin.on_dispose(proc() {{ cleanup_plugin.service(LOGGER).info("provider") }})
root.inject(LOGGER,proc(scoped: std::service::Context) {{ scoped.on_dispose(proc() {{ scoped.service(LOGGER).info("dependent") }}) }})
plugin.dispose()
"#
    );
    parity(&source, "dependent\nprovider\n");
}

#[test]
fn repl_relinks_live_bindings_and_cleanup_functions() {
    let mut repl = cyallo::repl::Repl::new(Default::default()).unwrap();
    for entry in [
        INTERFACE,
        "let root = std::service::Context::new()\nlet registration = root.provide(LOGGER,Console { prefix: \"repl:\" } as dyn Logger)\nroot.inject(LOGGER,proc(scoped: std::service::Context) { scoped.on_dispose(proc() { scoped.service(LOGGER).info(\"cleanup\") }) })",
        "proc unrelated() {}\nconst SECOND = std::service::ServiceKey::new<dyn Logger>(\"second\")",
        "root.service(LOGGER).info(\"ready\")\nregistration.dispose()",
    ] {
        assert_eq!(
            repl.push_line(entry).unwrap(),
            cyallo::repl::Submission::Ready
        );
        repl.run(100000);
        assert_eq!(repl.session().status(), &Status::Finished);
    }
    assert_eq!(repl.session().context.output, "repl:ready\nrepl:cleanup\n");
}
#[test]
fn polymorphism_is_bounded_and_unrelated_keys_do_not_change_targets() {
    let mut source = String::from(
        "trait T { proc emit(self: &mut Self) }\nconst KEY = std::service::ServiceKey::new<dyn T>(\"key\")\nconst OTHER = std::service::ServiceKey::new<dyn T>(\"other\")\nlet ctx = std::service::Context::new()\nproc invoke(c: &std::service::Context) { c.service(KEY).emit() }\n",
    );
    for i in 0..6 {
        source.push_str(&format!("record R{i} {{ n: i64 }}\nimpl T for R{i} {{ proc emit(self: &mut Self) {{ std::io::print(self.n) }} }}\n"));
    }
    source.push_str("let mut r = ctx.provide(KEY,R0 { n: 0 } as dyn T)\ninvoke(&ctx)\nlet other_registration = ctx.provide(OTHER,R0 { n: 9 } as dyn T)\ninvoke(&ctx)\nother_registration.dispose()\nr.dispose()\n");
    for i in 1..6 {
        source.push_str(&format!("r = ctx.provide(KEY,R{i} {{ n: {i} }} as dyn T)\ninvoke(&ctx)\ninvoke(&ctx)\nr.dispose()\n"));
    }
    let p = Arc::new(compile(&source).unwrap());
    for engine in engines(&p) {
        let mut s = Session::new(p.clone()).unwrap();
        engine.run(&mut s, 100000).unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "0\n0\n1\n1\n2\n2\n3\n3\n4\n4\n5\n5\n");
        assert_eq!(s.service_stats().max_target_cases, 4);
        assert_eq!(s.service_stats().target_misses, 8);
    }
}
#[test]
fn bad_trait_and_service_contracts_are_rejected() {
    for source in [
        "trait T { proc f(self: &mut Self) }\nrecord R { x: i64 }\nimpl T for R { fn f(self) -> i64 { return 1 } }\n",
        "trait T { proc f(self: &mut Self) }\nrecord R { x: i64 }\nlet v = R { x: 1 } as dyn T\n",
        "let key = std::service::ServiceKey::new<i64>(\"bad\")\n",
        "proc f(x: dyn Missing) {}\n",
        "trait T { proc f(self: &mut Self) }\nconst KEY = std::service::ServiceKey::new<dyn T>(\"key\")\nlet ctx = std::service::Context::new()\nctx.provide(KEY,1)\n",
        "fn forbidden(ctx: std::service::Context) -> bool { return ctx.available(std::service::ServiceKey::new<dyn Missing>(\"missing\")) }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}

#[test]
fn reentrant_withdrawal_keeps_the_active_method_alive() {
    parity(
        r#"
trait T { proc run(self: &mut Self) }
record R { n: i64 }
impl T for R { proc run(self: &mut Self) { registration.dispose(); let pressure = [1,2,3]; std::io::print(self.n + pressure[0]) } }
const KEY = std::service::ServiceKey::new<dyn T>("key")
let ctx = std::service::Context::new()
let registration = ctx.provide(KEY,R { n: 6 } as dyn T)
ctx.service(KEY).run()
std::io::print(ctx.available(KEY))
"#,
        "7\nFALSE\n",
    );
}
#[test]
fn cleanup_can_reprovide_without_rebinding_its_old_admission() {
    let source = format!(
        r#"{INTERFACE}
let root = std::service::Context::new()
let registration = root.provide(LOGGER,Console {{ prefix: "old:" }} as dyn Logger)
let cleanup_root = std::mem::clone(&root)
root.inject(LOGGER,proc(scoped: std::service::Context) {{
    scoped.on_dispose(proc() {{
        cleanup_root.provide(LOGGER,Console {{ prefix: "new:" }} as dyn Logger)
        scoped.service(LOGGER).info("cleanup")
    }})
}})
registration.dispose()
root.service(LOGGER).info("ready")
"#
    );
    parity(&source, "old:cleanup\nnew:ready\n");
}
