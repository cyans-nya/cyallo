use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cyallo-import-{}-{:?}-{}",
            std::process::id(),
            std::thread::current().id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn drive(session: &mut Session, engine: Option<&dyn Executable>, budget: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(engine) = engine {
            engine.run(session, budget).unwrap();
        } else {
            session.run(budget);
        }
        match session.status() {
            Status::Paused => {}
            Status::Waiting => session.wait_for_wakeup(Duration::from_millis(2)),
            _ => break,
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}",
            session.status()
        );
    }
    assert_eq!(
        session.status(),
        &Status::Finished,
        "{}",
        session.context.output
    );
}
fn parity(main: &str, plugin: &str, expected: &str) {
    let files = Files::new();
    files.write("main.ya", main);
    for deferred in [false, true] {
        let _ = std::fs::remove_file(files.path("plugin.ya"));
        // The selected file does not exist when this program is compiled.
        let program = Arc::new(
            cyallo::modules::compile_path(
                &files.path("main.ya"),
                CompileOptions::default(),
                deferred,
            )
            .unwrap(),
        );
        files.write("plugin.ya", plugin);
        let tiered = TieredBackend::default().compile(program.clone()).unwrap();
        for function in 0..program.functions().len() {
            tiered.precompile(function, Tier::Winch).unwrap();
        }
        assert!(tiered.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(tiered)),
            Some(Box::new(CraneliftBackend.compile(program.clone()).unwrap())),
            Some(Box::new(
                CraneliftSpeedBackend.compile(program.clone()).unwrap(),
            )),
        ];
        #[cfg(feature = "wasm-adapter")]
        let engines = {
            let mut engines = engines;
            engines.push(Some(Box::new(
                cyallo::backend::WasmBackend
                    .compile(program.clone())
                    .unwrap(),
            )));
            engines
        };
        for (index, engine) in engines.into_iter().enumerate() {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    concurrency: cyallo_scheduler::Config {
                        processors: 1,
                        blocking_threads: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            session.enable_reporting(Instant::now());
            drive(
                &mut session,
                engine.as_deref(),
                if index % 2 == 0 { 1 } else { 1000 },
            );
            assert_eq!(
                session.context.output, expected,
                "deferred={deferred}, engine={index}"
            );
            session.restart().unwrap();
            drive(&mut session, engine.as_deref(), 7);
            assert_eq!(
                session.context.output, expected,
                "restart deferred={deferred}, engine={index}"
            );
        }
    }
}
#[test]
fn loads_new_files_at_runtime_and_caches_namespace_contracts() {
    parity(
        r#"
use std::module::load
type Plugin = object { twice: fn(i64) -> i64, OFFSET: i64 }
std::io::print("before")
let first = load("./plugin.ya").unwrap()
let second = load("./plugin.ya").unwrap()
std::io::print(first.twice(21), ":", second.OFFSET)
"#,
        r#"
pub const OFFSET = 2
pub fn twice(x: i64) -> i64 { return x * OFFSET }
"#,
        "before\n42:2\n",
    );
}
#[test]
fn imported_procedures_share_the_callers_local_heap_and_can_suspend() {
    parity(
        r#"
use std::module::load
type Plugin = object { update: proc(gc Cell<i64>) -> i64 }
let plugin = load("plugin.ya").unwrap()
let data = gc Cell::new(10)
std::io::print(plugin.update(data.clone()), ":", data.*.*)
"#,
        r#"
pub proc update(value: gc Cell<i64>) -> i64 {
    let clock = std::time::clock()
    clock.sleep(1)
    value.*.* = value.*.* + 32
    return value.*.*
}
"#,
        "42:42\n",
    );
}
#[test]
fn imports_report_typed_failures_as_ordinary_results() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
use std::module::load
type Plugin = object { twice: fn(i64) -> i64 }
std::io::print(load("missing.ya").unwrap_err().is_not_found())
std::io::print(load("../outside.ya").unwrap_err().is_denied())
std::io::print(load("broken.ya").unwrap_err().is_check_error())
"#,
    );
    files.write("broken.ya", "pub fn twice(x: i64) -> i64 { return false }");
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "true\ntrue\ntrue\n");
}
#[test]
fn in_memory_compilation_has_no_ambient_file_authority() {
    let program=Arc::new(cyallo::compile("type Plugin = object { value: i64 }\nstd::io::print(std::module::load(\"plugin.ya\").unwrap_err().is_denied())").unwrap());
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "true\n");
}

#[test]
fn indexed_snapshot_survives_successive_module_publications() {
    use cyallo::module_loader::{ModuleLoader, ReadGrant};

    let files = Files::new();
    files.write("first.ya", "pub fn answer() -> i64 { return 20 }\n");
    files.write("second.ya", "pub fn answer() -> i64 { return 22 }\n");
    let source: Arc<str> = Arc::from(concat!(
        "use std::module::load\n",
        "type Plugin = object { answer: fn() -> i64 }\n",
        "let first = load(\"first.ya\").unwrap()\n",
        "let second = load(\"second.ya\").unwrap()\n",
        "std::io::print(first.answer() + second.answer())\n",
    ));
    let snapshot = cyallo_parser::parse_modern_snapshot(source, false, cyallo::Jobs(1)).unwrap();
    let program =
        cyallo::compile_snapshot_for_execution(Arc::new(snapshot), CompileOptions::default())
            .unwrap()
            .with_module_loader(ModuleLoader::new(ReadGrant::directory(&files.0).unwrap()));
    let mut session = Session::new(Arc::new(program)).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42\n");
}

#[test]
fn loads_additional_modules_and_different_contracts_without_replaying_root_code() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
use std::module::load
type First = object { answer: fn() -> i64 }
type Other = object { id: fn(i64) -> i64 }
std::io::print("once")
let a = load("plugins/first.ya").unwrap()
std::io::print(a.answer())
let b = load("plugins/other.ya").unwrap()
std::io::print(b.id(9), ":", a.answer())
let c = load("plugins/first.ya").unwrap()
std::io::print(c.id(7), ":", a.answer())
"#,
    );
    files.write(
        "plugins/first.ya",
        r#"
pub mod child;
pub use self::child::answer
pub fn id<T>(x: T) -> T { return x }
"#,
    );
    files.write(
        "plugins/child.ya",
        "pub const N = 42\npub fn answer() -> i64 { return N }",
    );
    files.write("plugins/other.ya", "pub fn id<T>(x: T) -> T { return x }");
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::with_context(
        program,
        Context {
            gc: cyallo::runtime::gc::GcConfig::stress(),
            ..Default::default()
        },
    )
    .unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "once\n42\n9:42\n7:42\n");
}

#[test]
fn loaded_code_keeps_the_importers_permission_ceiling() {
    use cyallo::import_policy::{ImportPolicy, ImportScope, ModuleAccess, ScopeMatch};
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
mod restricted;
std::io::print(restricted::attempt().is_check_error())
"#,
    );
    files.write(
        "restricted.ya",
        r#"
type Plugin = object { work: proc() }
pub proc attempt() -> std::module::LoadError {
    return std::module::load("plugin.ya").unwrap_err()
}
"#,
    );
    files.write("plugin.ya", "pub proc work() { std::io::print(42) }");
    let sources = cyallo::modules::load_path(&files.path("main.ya")).unwrap();
    let policy = ImportPolicy {
        scopes: vec![ImportScope {
            referrer: "crate::restricted".into(),
            matching: ScopeMatch::Exact,
            access: ModuleAccess::Except(vec!["std::io".into()]),
        }],
        ..Default::default()
    };
    let program = cyallo::modules::compile_modules_with_policy(
        sources.len() - 1,
        &sources,
        CompileOptions::default(),
        false,
        &policy,
    )
    .unwrap()
    .with_module_loader(cyallo::module_loader::ModuleLoader::new(
        cyallo::module_loader::ReadGrant::directory(&files.0).unwrap(),
    ));
    let mut session = Session::new(Arc::new(program)).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "true\n");
}

#[test]
fn nested_runtime_imports_resolve_relative_to_the_imported_file() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
type Plugin = object { answer: proc() -> i64 }
let plugin = std::module::load("plugins/outer.ya").unwrap()
std::io::print(plugin.answer(), ":", plugin.answer())
"#,
    );
    files.write(
        "plugins/outer.ya",
        r#"
type Inner = object { answer: fn() -> i64 }
pub proc answer() -> i64 { return std::module::load("inner.ya").unwrap().answer() }
"#,
    );
    files.write("plugins/inner.ya", "pub fn answer() -> i64 { return 42 }");
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42:42\n");
}

#[test]
fn cached_imports_keep_the_loaded_snapshot_after_the_source_is_removed() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
type Plugin = object { answer: fn() -> i64 }
let first = std::module::load("plugin.ya").unwrap()
std::io::print(first.answer())
let second = std::module::load("plugin.ya").unwrap()
std::io::print(second.answer())
"#,
    );
    files.write("plugin.ya", "pub fn answer() -> i64 { return 42 }");
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    while session.context.output.is_empty() {
        session.run(1);
        if session.status() == &Status::Waiting {
            session.wait_for_wakeup(Duration::from_millis(2));
        }
        assert!(matches!(session.status(), Status::Paused | Status::Waiting));
    }
    std::fs::remove_file(files.path("plugin.ya")).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42\n42\n");
}

#[test]
fn invalid_exports_and_effects_are_rejected_without_running_module_code() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
type Plugin = object { answer: fn() -> i64 }
std::io::print(std::object::view<Plugin>(std::module::load("private.ya").unwrap()).is_err())
std::io::print(std::object::view<Plugin>(std::module::load("wrong.ya").unwrap()).is_err())
std::io::print(std::module::load("effect.ya").unwrap_err().is_check_error())
std::io::print(std::module::load("syntax.ya").unwrap_err().is_check_error())
"#,
    );
    files.write("private.ya", "fn answer() -> i64 { return 42 }");
    files.write("wrong.ya", "pub fn answer() -> string { return \"wrong\" }");
    files.write(
        "effect.ya",
        "std::io::print(\"must not run\")\npub fn answer() -> i64 { return 42 }",
    );
    files.write("syntax.ya", "pub fn answer( {");
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "true\ntrue\ntrue\ntrue\n");
    assert!(cyallo::compile("type Plugin = object { answer: fn() -> i64 }\nfn bad() { let value = std::module::load(\"plugin.ya\") }").is_err());
    assert!(cyallo::compile("type Plugin = object { answer: fn() -> i64 }\nconst value = std::module::load(\"plugin.ya\")").is_err());
    assert!(cyallo::compile("std::module::load<i64>(\"plugin.ya\")").is_err());
}

#[test]
fn independent_coroutines_import_with_one_processor() {
    parity(
        r#"
type Plugin = object { answer: proc() -> i64 }
proc work() -> i64 {
    let plugin = std::module::load("plugin.ya").unwrap()
    return plugin.answer()
}
let a = std::task::spawn(work)
let b = std::task::spawn(work)
std::io::print(a.join().completed() + b.join().completed())
"#,
        r#"
pub const VALUE = 21
pub proc answer() -> i64 { let clock = std::time::clock(); clock.sleep(1); return VALUE }
"#,
        "42\n",
    );
}

#[test]
fn import_declarations_can_move_and_shadow_without_changing_compiler_paths() {
    use cyallo::{
        core_package::{CorePackage, SourceText},
        import_policy::ImportPolicy,
        module_loader::{ModuleLoader, ReadGrant},
    };
    let files = Files::new();
    files.write("plugin.ya", "pub fn answer() -> i64 { return 42 }");
    let mut sources: Vec<(String, String)> = CorePackage::embedded()
        .sources()
        .iter()
        .map(|source| (source.path.into(), source.source.into()))
        .collect();
    for (path, source) in &mut sources {
        if path == "std.ya" {
            *source = source.replace("pub mod module;", "pub mod plugins;");
        }
        if path == "std/module.ya" {
            *path = "std/plugins.ya".into();
            *source = source
                .replace("LoadError", "LoadFailure")
                .replace("proc load(", "proc read_file(")
                .replace("fn is_denied(", "fn was_denied(");
        }
    }
    let core = CorePackage::from_trusted_sources(
        "std.ya",
        sources
            .iter()
            .map(|(path, source)| SourceText { path, source }),
    )
    .unwrap()
    .with_bindings(
        cyallo_ir::bindings::NativeBindings::embedded().map(|path, key| {
            let path = match path.strip_prefix("std::module::") {
                Some(rest) => format!(
                    "std::plugins::{}",
                    rest.replace("LoadError", "LoadFailure")
                        .replace("load", "read_file")
                        .replace("is_denied", "was_denied")
                ),
                None => path.to_string(),
            };
            Some((path, key.to_string()))
        }),
    );
    let program = cyallo::compile_with_core_package(
        r#"
use std::plugins::read_file as read_plugin
type Plugin = object { answer: fn() -> i64 }
fn load(value: i64) -> i64 { return value + 1 }
std::io::print(read_plugin("plugin.ya").unwrap().answer(), ":", load(6))
std::io::print(read_plugin("../outside.ya").unwrap_err().was_denied())
"#,
        CompileOptions::default(),
        &ImportPolicy::default(),
        &core,
    )
    .unwrap()
    .with_module_loader(ModuleLoader::new(ReadGrant::directory(&files.0).unwrap()));
    let mut session = Session::new(Arc::new(program)).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42:7\ntrue\n");
}

#[test]
fn runtime_files_have_their_own_crate_and_cannot_escape_through_super() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
fn private_host() -> i64 { return 99 }
type Plugin = object { answer: fn() -> i64 }
std::io::print(std::module::load("plugin.ya").unwrap().answer())
std::io::print(std::module::load("escape.ya").unwrap_err().is_check_error())
std::io::print(std::module::load("escape2.ya").unwrap_err().is_check_error())
"#,
    );
    files.write(
        "plugin.ya",
        r#"
pub mod child;
pub use crate::child::answer
const VALUE = 42
"#,
    );
    files.write("child.ya", "pub fn answer() -> i64 { return super::VALUE }");
    files.write(
        "escape.ya",
        "pub fn answer() -> i64 { return super::private_host() }",
    );
    files.write(
        "escape2.ya",
        "pub fn answer() -> i64 { return self::super::private_host() }",
    );
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42\ntrue\ntrue\n");
}

#[test]
fn suspended_generators_keep_their_frames_across_code_publication() {
    parity(
        r#"
type Plugin = object { twice: fn(i64) -> i64 }
let create = gen proc() -> Generator<i64, (), ()> {
    let data = gc Cell::new(7)
    yield data.*.*
    let plugin = std::module::load("plugin.ya").unwrap()
    yield plugin.twice(data.*.*)
}
let mut g = create()
std::io::print(g.next().yielded())
let plugin = std::module::load("plugin.ya").unwrap()
std::io::print(g.next().yielded(), ":", plugin.twice(21))
std::io::print(g.next().is_complete())
"#,
        "pub fn twice(x: i64) -> i64 { return x * 2 }",
        "7\n14:42\ntrue\n",
    );
}

#[test]
fn native_reexports_and_distinct_generic_callbacks_satisfy_import_contracts() {
    parity(
        r#"
type Plugin = object { magnitude: fn(i64) -> i64, id: fn(i64) -> i64, text: fn(string) -> string, visit: proc(i64) -> () }
let p = std::object::view<Plugin>(std::module::load("plugin.ya").unwrap()).unwrap()
p.visit(0)
std::io::print(p.magnitude(-42), ":", p.id(7), ":", p.text("yes"))
"#,
        r#"
pub use std::num::abs as magnitude
pub fn id<T>(x: T) -> T { return x }
pub use self::id as text
pub proc visit<T>(value: T) {}
"#,
        "42:7:yes\n",
    );
}

#[test]
fn cached_code_does_not_widen_a_different_importers_permissions() {
    use cyallo::{
        import_policy::{ImportPolicy, ImportScope, ModuleAccess, ScopeMatch},
        module_loader::{ModuleLoader, ReadGrant},
    };
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
mod restricted;
type Plugin = object { work: proc() }
let p = std::module::load("plugin.ya").unwrap()
p.work()
std::io::print(restricted::attempt())
"#,
    );
    files.write(
        "restricted.ya",
        r#"
type Plugin = object { work: proc() }
pub proc attempt() -> bool { return std::module::load("plugin.ya").is_err() }
"#,
    );
    files.write("plugin.ya", "pub mod child;\npub use crate::child::work");
    files.write("child.ya", "pub proc work() { std::io::print(42) }");
    let sources = cyallo::modules::load_path(&files.path("main.ya")).unwrap();
    let policy = ImportPolicy {
        scopes: vec![ImportScope {
            referrer: "crate::restricted".into(),
            matching: ScopeMatch::Exact,
            access: ModuleAccess::Except(vec!["std::io".into()]),
        }],
        ..Default::default()
    };
    let program = cyallo::modules::compile_modules_with_policy(
        sources.len() - 1,
        &sources,
        CompileOptions::default(),
        false,
        &policy,
    )
    .unwrap()
    .with_module_loader(ModuleLoader::new(ReadGrant::directory(&files.0).unwrap()));
    let mut session = Session::new(Arc::new(program)).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "42\ntrue\n");
}

#[test]
fn unrelated_native_artifacts_are_still_rejected() {
    let artifact = CraneliftBackend
        .compile(Arc::new(cyallo::compile("std::io::print(1)").unwrap()))
        .unwrap();
    let mut session =
        Session::new(Arc::new(cyallo::compile("std::io::print(1)").unwrap())).unwrap();
    assert_eq!(
        artifact.run(&mut session, 1),
        Err(cyallo::runtime::EngineError::ForeignProgram)
    );
}

#[cfg(unix)]
#[test]
fn runtime_imports_reject_leaf_symlinks() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
type Plugin = object { answer: fn() -> i64 }
std::io::print(std::module::load("link.ya").unwrap_err().is_denied())
"#,
    );
    files.write("plugin.ya", "pub fn answer() -> i64 { return 42 }");
    std::os::unix::fs::symlink(files.path("plugin.ya"), files.path("link.ya")).unwrap();
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    drive(&mut session, None, 1);
    assert_eq!(session.context.output, "true\n");
}

#[test]
fn exported_aggregate_constants_are_copied_from_initialized_module_storage() {
    parity(
        r#"
type Plugin = object { values: [3]i64 }
let first = std::object::view<Plugin>(std::module::load("plugin.ya").unwrap()).unwrap()
let mut values = first.values
values[0] = 99
let second = std::module::load("plugin.ya").unwrap()
std::io::print(values[0], ":", second.values[0], ":", second.values[2])
"#,
        "pub const values = [1, 2, 3]",
        "99:1:3\n",
    );
}

#[test]
fn publication_can_enable_tracing_and_retains_imported_error_locations() {
    let files = Files::new();
    files.write(
        "main.ya",
        r#"
type Plugin = object { answer: proc() -> i64 }
let p = std::module::load("plugin.ya").unwrap()
std::io::print(p.answer())
"#,
    );
    files.write(
        "plugin.ya",
        r#"
pub proc answer() -> i64 {
    let data = gc Cell::new(42)
    return data.*.*
}
"#,
    );
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    assert!(!session.context.gc.enabled);
    drive(&mut session, None, 1);
    assert!(session.context.gc.enabled);
    assert_eq!(session.context.output, "42\n");
    // A separately loaded routine traps at its own source, rather than the caller archive.
    files.write(
        "plugin.ya",
        "pub proc answer() -> i64 { let x = 0; return 1 / x }",
    );
    let program = Arc::new(
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap(),
    );
    let mut session = Session::new(program).unwrap();
    while matches!(session.status(), Status::Paused | Status::Waiting) {
        session.run(1);
        if session.status() == &Status::Waiting {
            session.wait_for_wakeup(Duration::from_millis(2));
        }
    }
    let Status::Failed(failure) = session.status() else {
        panic!("expected division failure")
    };
    let (path, source, diagnostic) = cyallo::modules::diagnostic_context(
        session.program().source(),
        session.program().source_modules(),
        &failure.diagnostic(),
    )
    .unwrap();
    assert!(path.ends_with("plugin.ya"));
    assert_eq!(
        &source[diagnostic.span.start as usize..diagnostic.span.end as usize],
        "1 / x"
    );
}

#[test]
fn publication_preserves_existing_services_and_associated_function_identities() {
    parity(
        r#"
use std::service::{Context, ServiceKey}
trait Readable { fn value(self: &Self) -> i64 }
record Data { n: i64 }
impl Data { pub fn first() -> Data { return Data { n: 7 } }
pub fn second() -> Data { return Data { n: 9 } } }
impl Readable for Data { fn value(self: &Self) -> i64 { return self.n } }
const KEY = ServiceKey::new<dyn Readable>("host")
let context = Context::new()
let registration = context.provide(KEY, Data::first() as dyn Readable)
type Plugin = object { work: proc() -> i64 }
let p = std::module::load("plugin.ya").unwrap()
std::io::print(context.service(KEY).value(), ":", p.work(), ":", Data::second().n)
registration.dispose()
"#,
        r#"
use std::service::{Context, ServiceKey}
trait Readable { fn value(self: &Self) -> i64 }
record Data { n: i64 }
impl Data { pub fn make() -> Data { return Data { n: 42 } } }
impl Readable for Data { fn value(self: &Self) -> i64 { return self.n } }
const KEY = ServiceKey::new<dyn Readable>("plugin")
pub proc work() -> i64 {
    let context = Context::new()
    let registration = context.provide(KEY, Data::make() as dyn Readable)
    let result = context.service(KEY).value()
    registration.dispose()
    return result
}

"#,
        "7:42:9\n",
    );
}

#[test]
fn namespaces_expose_modules_types_and_dynamic_generic_calls_without_projection() {
    parity(
        r#"
type Plugin=object { id:fn(i64)->i64,.. }
let module=std::module::load("plugin.ya").unwrap()
let same=std::module::load("plugin.ya").unwrap()
let export_name="id"
let callback=module[export_name]
std::io::print(callback(7), ":", callback("text"), ":", module==same)
let checked=std::object::view<Plugin>(module).unwrap()
std::io::print(checked.id(9), ":", checked.child.N, ":", module.alias.N, ":", module.Data)
"#,
        r#"
pub fn id<T>(value:T)->T { return value }
pub mod child {pub const N=11}
pub use crate::child as alias
pub record Data {value:i64}
"#,
        "7:text:true\n9:11:11:type Data\n",
    );
}

#[test]
fn namespaces_are_readonly_and_failed_specializations_do_not_execute() {
    for (main, plugin, expected) in [
        (
            "let module=std::module::load(\"plugin.ya\").unwrap(); module.value=2",
            "pub const value=1",
            cyallo_shapes::DynamicError::Readonly,
        ),
        (
            "let module=std::module::load(\"plugin.ya\").unwrap(); delete module.value",
            "pub const value=1",
            cyallo_shapes::DynamicError::Readonly,
        ),
    ] {
        let files = Files::new();
        files.write("main.ya", main);
        files.write("plugin.ya", plugin);
        let program =
            cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
                .unwrap();
        let mut session = Session::new(Arc::new(program)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(session.status(), Status::Paused | Status::Waiting) {
            session.run(1);
            if session.status() == &Status::Waiting {
                session.wait_for_wakeup(Duration::from_millis(1));
            }
            assert!(Instant::now() < deadline);
        }
        let Status::Failed(failure) = session.status() else {
            panic!("{:?}", session.status())
        };
        assert_eq!(
            failure.error,
            cyallo::runtime::RuntimeError::Dynamic(expected)
        );
    }
    let files = Files::new();
    files.write(
        "main.ya",
        "let module=std::module::load(\"plugin.ya\").unwrap(); module.bad(1)",
    );
    files.write(
        "plugin.ya",
        "pub proc bad<T>(value:T)->T {std::io::print(\"must not run\"); return false}",
    );
    let program =
        cyallo::modules::compile_path(&files.path("main.ya"), CompileOptions::default(), false)
            .unwrap();
    let mut session = Session::new(Arc::new(program)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(session.status(), Status::Paused | Status::Waiting) {
        session.run(1);
        if session.status() == &Status::Waiting {
            session.wait_for_wakeup(Duration::from_millis(1));
        }
        assert!(Instant::now() < deadline);
    }
    assert!(
        matches!(session.status(),Status::Failed(cyallo::runtime::RuntimeFailure{error:cyallo::runtime::RuntimeError::Dynamic(cyallo_shapes::DynamicError::Call(error)),..}) if matches!(error.as_ref(),cyallo_modules::LoadError::Check{..})),
        "{:?}",
        session.status()
    );
    assert!(session.context.output.is_empty());
}
