use cyallo::{
    Session, Status,
    modules::{SourceModule, compile_modules},
};
use std::sync::Arc;
fn module(path: &str, source: &str) -> SourceModule {
    SourceModule::new(path, source)
}
#[test]
fn filesystem_dialects_stay_separate() {
    struct Sources(std::path::PathBuf);
    impl Drop for Sources {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        Sources(std::env::temp_dir().join(format!("caie-dialects-{}-{nonce}", std::process::id())));
    std::fs::create_dir_all(directory.0.join("lib")).unwrap();
    let entry = directory.0.join("main.ya");
    std::fs::write(
        &entry,
        "mod math;\nuse crate::math::ANSWER\nstd::io::print(ANSWER)\n",
    )
    .unwrap();
    std::fs::write(directory.0.join("math.ya"), "pub const ANSWER = 42\n").unwrap();
    // A compatibility file must not even be read during modern module discovery.
    std::fs::write(directory.0.join("lib/exam.psc"), [0xff]).unwrap();
    let sources = cyallo::modules::load_path(&entry).unwrap();
    assert_eq!(sources.len(), 2);
    let program = cyallo::modules::compile_path(&entry, Default::default(), false).unwrap();
    let mut session = Session::new(Arc::new(program)).unwrap();
    session.run(100);
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "42\n");

    let legacy = directory.0.join("legacy.ya");
    std::fs::write(&legacy, "DECLARE n : INTEGER\nn <- 1\nOUTPUT n\n").unwrap();
    for command in ["check", "run", "bytecode"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_cyallo"))
            .arg(command)
            .arg(&legacy)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{command} accepted CAIE in .ya");
        assert!(String::from_utf8_lossy(&result.stderr).contains("E002"));
    }
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_cyallo"))
        .arg("run")
        .arg(&entry)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"42\n");
}
#[test]
fn reachable_modules_constants_and_import_aliases() {
    let sources = [
        module("unused.ya", "std::io::print(\"must not run\")\n"),
        module("b.ya", "pub const BASE = 4\n"),
        module("a.ya", "use crate::b::BASE\npub const ANSWER = BASE + 3\n"),
        module(
            "main.ya",
            "mod a;\nmod b;\nuse crate::a::ANSWER as result\nstd::io::print(result)\n",
        ),
    ];
    let p = Arc::new(compile_modules(3, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "7\n");
}
#[test]
fn imported_constants_evaluate_pure_routines() {
    let sources = [
        module(
            "math.ya",
            "fn square(n: i64) -> i64 { return n * n }\npub const ANSWER = square(12)\n",
        ),
        module(
            "main.ya",
            "mod math;\nuse crate::math::ANSWER\nstd::io::print(ANSWER)\n",
        ),
    ];
    let p = Arc::new(compile_modules(1, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.context.output, "144\n");
    assert_eq!(s.function_call_count(1), 0);
}
#[test]
fn invalid_imports_visibility_initializers_and_cycles() {
    for (library, main) in [
        ("const secret = 1\n", "mod lib;\nuse crate::lib::secret"),
        (
            "trait Thing {}\npub const THING = 1\n",
            "mod lib;\nuse crate::lib::Thing",
        ),
        (
            "pub const x = 1\n",
            "mod lib;\nuse crate::lib::x\nuse crate::lib::x",
        ),
        (
            "pub const x = y\nconst y = x\n",
            "mod lib;\nuse crate::lib::x",
        ),
        (
            "proc install() -> i64 { std::io::print(\"side effect\"); return 1 }\npub const x = install()\n",
            "mod lib;\nuse crate::lib::x",
        ),
    ] {
        assert!(
            compile_modules(
                1,
                &[module("lib.ya", library), module("main.ya", main)],
                Default::default(),
                false
            )
            .is_err(),
            "accepted {library} {main}"
        );
    }
}
#[test]
fn extension_alias_and_real_record_member_precedence() {
    let sources = [
        module(
            "ext.ya",
            r#"
pub property std::service::Context.answer: i64 { proc get() { return 42 } }
"#,
        ),
        module(
            "main.ya",
            r#"mod ext;

use crate::ext::answer as value
record R { value: i64 }
let ctx = std::service::Context::new()
let r = R { value: 7 }
std::io::print(ctx.value, r.value)
"#,
        ),
    ];
    let p = Arc::new(compile_modules(1, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "427\n");
}
#[test]
fn nested_imported_body_spans_are_shifted_once() {
    let sources = [
        module(
            "lib.ya",
            "pub proc fail() -> i64 {\n if true { return missing }\n return 0\n}\n",
        ),
        module(
            "main.ya",
            "mod lib;\nuse crate::lib::fail\nstd::io::print(fail())\n",
        ),
    ];
    let error = compile_modules(1, &sources, Default::default(), false).unwrap_err();
    let archive = cyallo::modules::source_archive(&sources);
    assert_eq!(
        &archive[error.span.start as usize..error.span.end as usize],
        "missing"
    );
}
#[test]
fn service_modules_run_from_cli() {
    for engine in ["interpreter", "jit", "cranelift", "cranelift_speed"] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_cyallo"))
            .args([
                "run",
                "examples/services/main.ya",
                "--engine",
                engine,
                "--gc-stress",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            "first: ready\nfirst: cleanup\nsecond: ready\nFALSE\n"
        );
    }
}

#[test]
fn internal_declarations_are_visible_within_one_compilation_module() {
    let sources = [
        module("lib.ya", "pub(crate) const X = 3\n"),
        module(
            "main.ya",
            "mod lib;\nuse crate::lib::X\nstd::io::print(X)\n",
        ),
    ];
    let p = Arc::new(compile_modules(1, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(100);
    assert_eq!(s.context.output, "3\n");
}
#[test]
fn runtime_diagnostics_name_the_imported_source() {
    let sources = [
        module("lib.ya", "pub proc fail() -> i64 { return 1 div 0 }\n"),
        module(
            "main.ya",
            "mod lib;\nuse crate::lib::fail\nstd::io::print(fail())\n",
        ),
    ];
    let p = Arc::new(compile_modules(1, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p.clone()).unwrap();
    s.run(1000);
    let Status::Failed(error) = s.status() else {
        panic!("expected division failure")
    };
    let report =
        cyallo::diagnostics::render_program("main.ya", &p, error, false, Default::default());
    assert!(report.contains("lib.ya"));
    assert!(report.contains("1 div 0"));
}

#[test]
fn imports_do_not_shadow_real_members_or_field_writes() {
    let sources = [
        module(
            "ext.ya",
            r#"
pub property std::service::Context.available: i64 { proc get() { return 99 } }
pub property std::service::Context.n: i64 { proc get() { return 42 } }
"#,
        ),
        module(
            "main.ya",
            r#"mod ext;

use crate::ext::available
use crate::ext::n
trait T { proc f(self: &mut Self) }
const KEY = std::service::ServiceKey::new<dyn T>("key")
record R { n: i64 }
let mut r = R { n: 1 }
r.n += 1
let ctx = std::service::Context::new()
let exists = ctx.available(KEY)
std::io::print(r.n, exists)
"#,
        ),
    ];
    let p = Arc::new(compile_modules(1, &sources, Default::default(), false).unwrap());
    let mut s = Session::new(p).unwrap();
    s.run(1000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, "2FALSE\n");
}
