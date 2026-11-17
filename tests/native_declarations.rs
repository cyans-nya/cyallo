use cyallo::{
    CompileOptions, Context, Session, Status,
    backend::{CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, TieredBackend},
    core_package::{CorePackage, SourceText},
    import_policy::{ImportPolicy, ImportScope, ModuleAccess, ScopeMatch},
    modules::{SourceModule, compile_modules_with_policy},
};
use cyallo_ir::bindings::NativeBindings;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn execute(program: cyallo::ir::Program, expected: &str) {
    let program = Arc::new(program);
    let engines: Vec<Option<Box<dyn Executable>>> = vec![
        None,
        Some(Box::new(
            TieredBackend::default().compile(program.clone()).unwrap(),
        )),
        Some(Box::new(CraneliftBackend.compile(program.clone()).unwrap())),
        Some(Box::new(
            CraneliftSpeedBackend.compile(program.clone()).unwrap(),
        )),
    ];
    for engine in engines {
        let mut session = Session::with_context(
            program.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig::stress(),
                concurrency: cyallo_scheduler::Config {
                    processors: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match &engine {
                Some(engine) => engine.run(&mut session, 3).unwrap(),
                None => {
                    session.run(3);
                }
            }
            match session.status() {
                Status::Paused => {}
                Status::Waiting => session.wait_for_wakeup(Duration::from_millis(5)),
                _ => break,
            }
            assert!(
                Instant::now() < deadline,
                "native declaration execution timed out"
            );
        }
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, expected);
    }
}

fn standard_sources() -> Vec<(String, String)> {
    CorePackage::embedded()
        .sources()
        .iter()
        .map(|source| (source.path.into(), source.source.into()))
        .collect()
}

fn package(sources: &[(String, String)]) -> CorePackage<'_> {
    package_with(sources, NativeBindings::embedded())
}
fn package_with(sources: &[(String, String)], bindings: NativeBindings) -> CorePackage<'_> {
    CorePackage::from_trusted_sources(
        "std.ya",
        sources
            .iter()
            .map(|(path, source)| SourceText { path, source }),
    )
    .unwrap()
    .with_bindings(bindings)
}
/// The embedded table with some entries' paths rewritten, as a provider does when it
/// moves or renames declarations in its source.
fn renamed(f: impl Fn(&str) -> String) -> NativeBindings {
    NativeBindings::embedded().map(|path, key| Some((f(path), key.to_string())))
}

#[test]
fn native_types_functions_members_and_modules_can_move_without_compiler_changes() {
    let mut sources = standard_sources();
    for (path, source) in &mut sources {
        if path == "std.ya" {
            *source = source.replace("pub mod task;", "pub mod workers;");
        }
        *source = source.replace("std::task::", "std::workers::");
        if path == "std/task.ya" {
            *path = "std/workers.ya".into();
            *source = source
                .replace("TaskOutcome", "Completion")
                .replace("Task", "Work")
                .replace("proc spawn<", "proc launch<")
                .replace("proc join(", "proc collect(");
        }
    }
    let program = cyallo::compile_with_core_package(
        r#"
        use std::workers::{launch, Work}
        proc wait(work: Work<i64>) -> i64 { return work.collect().completed() }
        let work = launch(proc() -> i64 { return 42 })
        std::io::print(wait(work))
    "#,
        CompileOptions::default(),
        &ImportPolicy::default(),
        &package_with(
            &sources,
            renamed(|path| match path.strip_prefix("std::task::") {
                Some(rest) => format!(
                    "std::workers::{}",
                    rest.replace("TaskOutcome", "Completion")
                        .replace("Task", "Work")
                        .replace("spawn", "launch")
                        .replace("join", "collect")
                ),
                None => path.to_string(),
            }),
        ),
    )
    .unwrap();
    execute(program, "42\n");
}

#[test]
fn source_contract_is_checked_and_source_body_needs_no_annotation() {
    let mut sources = standard_sources();
    let task = sources
        .iter_mut()
        .find(|(path, _)| path == "std/task.ya")
        .unwrap();
    task.1 = task.1.replace(
        "pub proc spawn<T: Transferable>(body: proc() -> T) -> Task<T>;",
        "pub proc spawn() -> i64 { return 73 }",
    );
    // Removing the provider's binding makes the declaration ordinary source.
    let unbound = NativeBindings::embedded()
        .map(|path, key| (path != "std::task::spawn").then(|| (path.to_string(), key.to_string())));
    let program = cyallo::compile_with_core_package(
        "std::io::print(std::task::spawn())",
        Default::default(),
        &ImportPolicy::default(),
        &package_with(&sources, unbound),
    )
    .unwrap();
    execute(program, "73\n");

    let mut sources = standard_sources();
    sources
        .iter_mut()
        .find(|(path, _)| path == "std/task.ya")
        .unwrap()
        .1 = sources
        .iter()
        .find(|(path, _)| path == "std/task.ya")
        .unwrap()
        .1
        .replace(
            "pub proc spawn<T: Transferable>(body: proc() -> T) -> Task<T>;",
            "pub proc spawn<T: Transferable>(body: proc() -> T) -> bool;",
        );
    let error = cyallo::compile_with_core_package(
        "let task = std::task::spawn(proc() -> i64 { return 1 })",
        Default::default(),
        &ImportPolicy::default(),
        &package(&sources),
    )
    .unwrap_err();
    assert_eq!(error.code, "E173");
}

#[test]
fn native_members_follow_lexical_visibility_inside_core_modules() {
    let mut sources = standard_sources();
    let task = sources
        .iter_mut()
        .find(|(path, _)| path == "std/task.ya")
        .unwrap();
    task.1 = task.1.replace("pub proc cancel(", "proc cancel(");
    task.1
        .push_str("\npub proc stop(work: Task<i64>) { work.cancel() }\n");
    let core = package(&sources);
    let source = "let work = std::task::spawn(proc() -> i64 { return 42 }); std::task::stop(work)";
    execute(
        cyallo::compile_with_core_package(
            source,
            Default::default(),
            &ImportPolicy::default(),
            &core,
        )
        .unwrap(),
        "",
    );
    let source = "let work = std::task::spawn(proc() -> i64 { return 42 }); work.cancel()";
    assert_eq!(
        cyallo::compile_with_core_package(
            source,
            Default::default(),
            &ImportPolicy::default(),
            &core
        )
        .unwrap_err()
        .code,
        "E116"
    );
}

#[test]
fn native_annotations_require_provider_provenance_and_opaque_storage() {
    for source in [
        "#[cyallo_internal(\"task_struct\")] record Task<T> {}",
        "#[cyallo_internal(\"task_spawn\")] proc spawn() {}",
        "let fake = std::task::Task<i64> {}",
    ] {
        assert_eq!(
            cyallo::compile(source).unwrap_err().code,
            "E173",
            "{source}"
        );
    }
    let sources = [
        SourceModule::new(
            "std/task.ya",
            "#[cyallo_internal(\"task_struct\")] pub record Task<T> {}",
        ),
        SourceModule::new("std.ya", "mod task;"),
        SourceModule::new("main.ya", "mod std;"),
    ];
    assert_eq!(
        compile_modules_with_policy(
            2,
            &sources,
            Default::default(),
            false,
            &ImportPolicy::default()
        )
        .unwrap_err()
        .code,
        "E173"
    );
}

#[test]
fn source_archive_preserves_core_paths_and_utf8_spans() {
    let mut sources = standard_sources();
    sources
        .iter_mut()
        .find(|(path, _)| path == "std/task.ya")
        .unwrap()
        .1
        .insert_str(0, "// 猫\n");
    let core = package_with(
        &sources,
        NativeBindings::embedded().map(|path, key| {
            let key = if path == "std::task::Task" { "unknown_representation" } else { key };
            Some((path.to_string(), key.to_string()))
        }),
    );
    let user = "// λ\nstd::io::print(1)";
    let error = cyallo::compile_with_core_package(
        user,
        Default::default(),
        &ImportPolicy::default(),
        &core,
    )
    .unwrap_err();
    let mut archive = user.to_owned();
    let layout = core.append_to(&mut archive).unwrap();
    let ranges: Vec<_> = layout
        .ranges
        .into_iter()
        .map(|range| cyallo::modules::SourceRange {
            path: Some(range.path),
            span: range.span,
        })
        .collect();
    let (path, text, local) =
        cyallo::modules::diagnostic_context(&archive, &ranges, &error).unwrap();
    assert_eq!(path, "std/task.ya");
    // A provider binding has no annotation text of its own, so the diagnostic points
    // at the bound declaration, still in the right file and at UTF-8-correct offsets.
    assert!(
        text[local.span.start as usize..local.span.end as usize].contains("record Task<T>")
    );
}

#[test]
fn exact_module_permissions_cover_qualified_calls_import_aliases_and_receiver_calls() {
    let policy = ImportPolicy {
        ceiling: ModuleAccess::Except(vec!["std::task".into()]),
        scopes: vec![],
    };
    for source in [
        "let task = std::task::spawn(proc() {})",
        "use std::task::spawn as launch; let task = launch(proc() {})",
        "proc wait(t: std::task::Task<i64>) { t.cancel() }",
    ] {
        assert_eq!(
            cyallo::compile_with_import_policy(source, Default::default(), &policy)
                .unwrap_err()
                .code,
            "E174",
            "{source}"
        );
    }
    execute(
        cyallo::compile_with_import_policy("std::io::print(42)", Default::default(), &policy)
            .unwrap(),
        "42\n",
    );
}

#[test]
fn scoped_import_permissions_follow_referrers_and_preserve_reexport_origins() {
    let policy = ImportPolicy {
        ceiling: ModuleAccess::All,
        scopes: vec![ImportScope {
            referrer: "crate::restricted".into(),
            matching: ScopeMatch::Subtree,
            access: ModuleAccess::Except(vec!["std::task".into()]),
        }],
    };
    for deferred in [false, true] {
        for body in [
            "use crate::bridge::launch; pub proc work() { let task = launch(proc() {}) }",
            "pub proc work(t: std::task::Task<i64>) { t.cancel() }",
        ] {
            let modules = [
                SourceModule::new("bridge.ya", "pub use std::task::spawn as launch;"),
                SourceModule::new("restricted.ya", body),
                SourceModule::new("main.ya", "mod bridge; mod restricted; std::io::print(42)"),
            ];
            let error =
                compile_modules_with_policy(2, &modules, Default::default(), deferred, &policy)
                    .unwrap_err();
            assert_eq!(error.code, "E174", "{error:?}");
        }
    }
}

#[test]
fn receiver_permissions_survive_inferred_types_and_deferred_bodies() {
    let policy = ImportPolicy {
        ceiling: ModuleAccess::All,
        scopes: vec![ImportScope {
            referrer: "crate::restricted".into(),
            matching: ScopeMatch::Exact,
            access: ModuleAccess::Except(vec!["std::task".into()]),
        }],
    };
    let modules = [
        SourceModule::new(
            "bridge.ya",
            "pub proc create() -> std::task::Task<i64> { return std::task::spawn(proc() -> i64 { return 42 }) }",
        ),
        SourceModule::new(
            "restricted.ya",
            "pub proc work() { let task = crate::bridge::create(); task.cancel() }",
        ),
        SourceModule::new(
            "main.ya",
            "mod bridge; mod restricted; crate::restricted::work()",
        ),
    ];
    assert_eq!(
        compile_modules_with_policy(2, &modules, Default::default(), false, &policy)
            .unwrap_err()
            .code,
        "E174"
    );
    match compile_modules_with_policy(2, &modules, Default::default(), true, &policy) {
        Err(error) => assert_eq!(error.code, "E174"),
        Ok(program) => assert_eq!(program.validate_all().unwrap_err().code, "E174"),
    }
}

#[test]
fn erased_resources_are_owned_and_checked_casts_transfer_them() {
    execute(
        cyallo::compile(
            r#"
        let pair = std::channel::bounded<i64>(0 as usize)
        let sender = pair.0.clone()
        let receiver = pair.1.clone()
        std::mem::drop(pair)
        let erased = std::any::erase(sender)
        let restored = std::any::checked<std::channel::Sender<i64>>(erased)
        std::mem::drop(restored)
        std::io::print(receiver.is_closed())
        let pair = std::channel::bounded<i64>(0 as usize)
        let sender = pair.0.clone()
        let receiver = pair.1.clone()
        std::mem::drop(pair)
        let erased = std::any::erase(sender)
        std::mem::drop(erased)
        std::io::print(receiver.is_closed())
    "#,
        )
        .unwrap(),
        "true\ntrue\n",
    );
    assert!(
        cyallo::compile(
            r#"
        let erased = std::any::erase(42)
        let first = std::any::checked<i64>(erased)
        let second = std::any::checked<i64>(erased)
    "#
        )
        .is_err()
    );
}

#[test]
fn standard_enum_variant_names_are_owned_by_source_declarations() {
    let mut sources = standard_sources();
    for (path, source) in &mut sources {
        if path == "std/option.ya" {
            *source = source
                .replace("Some: T", "Present: T")
                .replace("    None,", "    Absent,");
        }
        if path == "std/result.ya" {
            *source = source
                .replace("Ok: T", "Success: T")
                .replace("Err: E", "Failure: E");
        }
        // The prelude reexports variants by name, so renaming them is a source
        // edit there too.
        if path == "std/prelude.ya" {
            *source = source
                .replace("{Some, None}", "{Present, Absent}")
                .replace("{Ok, Err}", "{Success, Failure}");
        }
    }
    let table = renamed(|path| {
        path.replace("::Option::Some", "::Option::Present")
            .replace("::Option::None", "::Option::Absent")
            .replace("::Result::Ok", "::Result::Success")
            .replace("::Result::Err", "::Result::Failure")
    });
    let program=cyallo::compile_with_core_package(r#"
use std::option::Option::{Present, Absent}
use std::result::Result::{Success, Failure}
let present = Present(8)
let absent: Option<i64> = Absent
let result: Result<i64,string> = Failure("message")
std::io::print(match present { Present(n) => n, Absent => 0 }, match absent { Present(n) => n, Absent => 9 })
std::io::print(match result { Success(n) => "unexpected", Failure(text) => text })
std::io::print(match std::num::parse<i64>("42") { Success(n) => n, Failure(_) => 0 })
"#,CompileOptions::default(),&ImportPolicy::default(),&package_with(&sources, table)).unwrap();
    execute(program, "89\nmessage\n42\n");
}

#[test]
fn malformed_native_enum_payloads_and_roles_are_rejected() {
    for replacement in ["Some: bool", "#[since(\"1.0\")]\n    Some: bool"] {
        let mut sources = standard_sources();
        for (path, source) in &mut sources {
            if path == "std/option.ya" {
                *source = source.replace("Some: T", replacement);
            }
        }
        let error = cyallo::compile_with_core_package(
            "",
            CompileOptions::default(),
            &ImportPolicy::default(),
            &package(&sources),
        )
        .expect_err("invalid native enum accepted");
        assert_eq!(error.code, "E184");
    }
    let sources = standard_sources();
    let swapped = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::option::Option::None" { "result_err_variant" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert_eq!(
        cyallo::compile_with_core_package(
            "",
            CompileOptions::default(),
            &ImportPolicy::default(),
            &package_with(&sources, swapped)
        )
        .expect_err("wrong native variant role accepted")
        .code,
        "E184"
    );
}

#[test]
fn foreign_open_enums_require_a_wildcard_and_own_package_remains_exhaustive() {
    let mut sources = standard_sources();
    for (path, source) in &mut sources {
        if path == "std/option.ya" {
            source.push_str("\npub open enum Future { Ready: i64, #[since(\"1.1\")] Later }\n");
        }
    }
    let source = "use std::option::Future\nlet value = Future::Ready(3)\nlet result = match value { Future::Ready(n) => n, Future::Later => 0 }\n";
    let error = cyallo::compile_with_core_package(
        source,
        CompileOptions::default(),
        &ImportPolicy::default(),
        &package(&sources),
    )
    .expect_err("foreign open enum accepted without wildcard");
    assert_eq!(error.code, "E185");
    assert!(error.message.contains("wildcard"));
    let source = "use std::option::Future\nlet value = Future::Ready(3)\nstd::io::print(match value { Future::Ready(n) => n, _ => 0 })\n";
    execute(
        cyallo::compile_with_core_package(
            source,
            CompileOptions::default(),
            &ImportPolicy::default(),
            &package(&sources),
        )
        .unwrap(),
        "3\n",
    );
    execute(cyallo::compile("mod own { pub open enum Future { Ready: i64, Later } }\nlet value = own::Future::Ready(3)\nstd::io::print(match value { own::Future::Ready(n) => n, own::Future::Later => 0 })\n").unwrap(),"3\n");
}

#[test]
fn version_annotations_do_not_replace_private_semantic_annotations() {
    let mut sources = standard_sources();
    for (path, source) in &mut sources {
        if path == "std/option.ya" {
            // A version annotation on a bound declaration does not replace its binding.
            *source = source.replace("pub fn ", "#[since(\"1.1\")]\npub fn ");
        }
    }
    let program=cyallo::compile_with_core_package("#[since(\"1.0\")]\nfn source_value() -> i64 { return 3 }\nlet value = Option::Some(source_value())\nstd::io::print(value.is_some(), value.unwrap())\n",CompileOptions::default(),&ImportPolicy::default(),&package(&sources)).unwrap();
    execute(program, "true3\n");
}

#[test]
fn option_and_result_native_identities_require_actual_enum_declarations() {
    let mut sources = standard_sources();
    for (path, source) in &mut sources {
        if path == "std/option.ya" {
            let start = source.find("pub enum Option<T> {").unwrap();
            let end = start + source[start..].find("\n}").unwrap() + 2;
            source.replace_range(start..end, "pub record Option<T> {}");
        }
    }
    let error = cyallo::compile_with_core_package(
        "",
        CompileOptions::default(),
        &ImportPolicy::default(),
        &package(&sources),
    )
    .expect_err("an opaque record must not implement the enum ABI");
    assert!(error.message.contains("record"));
}
