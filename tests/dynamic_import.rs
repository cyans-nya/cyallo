use cyallo::{
    CompileOptions,
    vm::{Session, Status},
};
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
    drive(&mut session, 1);
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
    drive(&mut session, 1);
    assert_eq!(session.context.output, "42:42\n");
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
    drive(&mut session, 1);
    assert_eq!(session.context.output, "true\ntrue\ntrue\ntrue\n");
    assert!(cyallo::compile("type Plugin = object { answer: fn() -> i64 }\nfn bad() { let value = std::module::load(\"plugin.ya\") }").is_err());
    assert!(cyallo::compile("type Plugin = object { answer: fn() -> i64 }\nconst value = std::module::load(\"plugin.ya\")").is_err());
    assert!(cyallo::compile("std::module::load<i64>(\"plugin.ya\")").is_err());
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
    drive(&mut session, 1);
    assert_eq!(session.context.output, "42\ntrue\n");
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
    drive(&mut session, 1);
    assert_eq!(session.context.output, "true\n");
}

fn drive(session: &mut Session, budget: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        session.run(budget);
        match session.status() {
            Status::Paused => {}
            Status::Waiting => std::thread::sleep(Duration::from_millis(2)),
            _ => break,
        }
        assert!(Instant::now() < deadline, "timed out: {:?}", session.status());
    }
    assert_eq!(session.status(), &Status::Finished, "{}", session.context.output);
}
