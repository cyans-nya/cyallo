use cyallo::{
    Context, Session, Status,
    modules::{Compiler, SourceModule},
};
use cyallo_ir::{analysis::ModuleBuild, module_graph::BindingKind};
use std::sync::Arc;
fn module(path: &str, source: &str) -> SourceModule {
    SourceModule::new(path, source)
}
fn drive(program: cyallo::ir::Program, expected: &str) {
    let p = Arc::new(program);
    let mut s = Session::new(p).unwrap();
    s.run(100000);
    assert_eq!(s.status(), &Status::Finished);
    assert_eq!(s.context.output, expected);
}
fn unit<'a>(p: &'a cyallo::ir::Program, path: &str) -> &'a cyallo_ir::analysis::CheckedModule {
    p.modules()
        .iter()
        .find(|m| m.path.as_ref() == path)
        .unwrap()
}
#[test]
fn source_bodies_and_export_identities_remain_owned_by_modules() {
    let sources = [
        module(
            "lib.ya",
            "const SECRET = 2\npub const OFFSET = 40\nfn private(x: i64) -> i64 { return x + SECRET }\npub fn answer() -> i64 { return private(OFFSET) }\n",
        ),
        module(
            "main.ya",
            "mod lib;\nuse crate::lib::answer as imported\nstd::io::print(imported())\n",
        ),
    ];
    let p = cyallo::modules::compile_modules(1, &sources, Default::default(), false).unwrap();
    let library = unit(&p, "crate::lib");
    let entry = unit(&p, "crate");
    assert!(library.initializer.is_some());
    assert!(entry.initializer.is_none());
    let export = library
        .bindings
        .iter()
        .find(|b| b.name.as_ref() == "answer")
        .unwrap();
    let import = entry
        .bindings
        .iter()
        .find(|b| b.name.as_ref() == "imported")
        .unwrap();
    assert_eq!(export.declaration, import.declaration);
    assert_eq!(export.defining_module, library.id);
    assert_eq!(import.defining_module, library.id);
    assert_eq!(import.kind, BindingKind::Value);
    assert!(
        library
            .routines
            .iter()
            .any(|(_, f)| f.name.as_ref() == "private")
    );
    assert!(
        !entry
            .routines
            .iter()
            .any(|(_, f)| f.name.as_ref() == "private")
    );
    assert!(
        p.functions()[0]
            .code
            .iter()
            .any(|i| matches!(i.op,cyallo::ir::Op::Call(_,id,_) if Some(id)==library.initializer))
    );
    assert!(
        !entry
            .routines
            .iter()
            .find(|(id, _)| *id == 0)
            .unwrap()
            .1
            .code
            .iter()
            .any(|i| matches!(i.op,cyallo::ir::Op::Call(_,id,_) if Some(id)==library.initializer))
    );
    drive(p, "42\n");
}
#[test]
fn unchanged_modules_reuse_checked_ir_and_relocate_calls_and_sources() {
    let compiler = Compiler::default();
    let library = "pub fn answer(x: i64) -> i64 { return x * 2 }\n";
    let source = |implementation: &str| {
        vec![
            module("math.ya", implementation),
            module(
                "main.ya",
                "mod math;\nuse crate::math::answer\nfn report() -> i64 { return answer(21) }\nstd::io::print(report())\n",
            ),
        ]
    };
    let first = compiler
        .compile(1, &source(library), Default::default(), false)
        .unwrap();
    assert_eq!(unit(&first, "crate").build, ModuleBuild::Compiled);
    drive(first, "42\n");
    let changed = "// This implementation has a different body and file size.\npub fn answer(x: i64) -> i64 { return x + x + 1 }\n";
    let second = compiler
        .compile(1, &source(changed), Default::default(), false)
        .unwrap();
    assert_eq!(unit(&second, "crate").build, ModuleBuild::Reused);
    assert_eq!(unit(&second, "crate::math").build, ModuleBuild::Compiled);
    drive(second, "43\n");
    let third = compiler
        .compile(1, &source(changed), Default::default(), true)
        .unwrap();
    assert_eq!(unit(&third, "crate").build, ModuleBuild::Reused);
    assert_eq!(unit(&third, "crate::math").build, ModuleBuild::Reused);
    drive(third, "43\n");
}
#[test]
fn interface_changes_recheck_importers() {
    let compiler = Compiler::default();
    let main = "mod math;\nuse crate::math::answer\nstd::io::print(answer(21))\n";
    compiler
        .compile(
            1,
            &[
                module("math.ya", "pub fn answer(x: i64) -> i64 { return x * 2 }\n"),
                module("main.ya", main),
            ],
            Default::default(),
            false,
        )
        .unwrap();
    let error = compiler
        .compile(
            1,
            &[
                module("math.ya", "pub fn answer(x: bool) -> i64 { return 0 }\n"),
                module("main.ya", main),
            ],
            Default::default(),
            false,
        )
        .unwrap_err();
    assert!(error.message.contains("bool"));
}
#[test]
fn cross_module_generics_and_private_template_helpers_keep_their_owner() {
    let source = [
        module(
            "math.ya",
            "fn private(x: i64) -> i64 { return x + 1 }\npub fn keep<T>(value: T) -> fn() -> T { return fn() -> T { return value } }\npub fn answer<T>(value: T) -> (T, i64) { return (value, private(41)) }\n",
        ),
        module(
            "main.ya",
            "mod math;\nuse crate::math::{keep, answer}\nlet f = keep(7)\nlet result = answer(\"value\")\nstd::io::print(f(), \":\", result.0, \":\", result.1)\n",
        ),
    ];
    for deferred in [false, true] {
        let p = cyallo::modules::compile_modules(1, &source, Default::default(), deferred).unwrap();
        let lib = unit(&p, "crate::math");
        assert!(
            lib.routines
                .iter()
                .any(|(_, f)| f.name.as_ref() == "answer")
        );
        assert!(
            !unit(&p, "crate")
                .routines
                .iter()
                .any(|(_, f)| f.name.as_ref() == "private")
        );
        drive(p, "7:value:42\n");
    }
}
#[test]
fn initialization_dag_and_independent_session_storage() {
    let sources = [
        module(
            "a.ya",
            "use crate::b::BASE\npub const ANSWER = twice(BASE)\nfn twice(x: i64) -> i64 { return x * 2 }\n",
        ),
        module("b.ya", "pub const BASE = 21\n"),
        module("main.ya", "mod a;\nmod b;\nstd::io::print(a::ANSWER)\n"),
    ];
    let compiler = Compiler::default();
    let p = Arc::new(
        compiler
            .compile(2, &sources, Default::default(), false)
            .unwrap(),
    );
    for _ in 0..2 {
        let mut session = Session::with_context(
            p.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig::stress(),
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..10000 {
            session.run(1);
            if session.status() != &Status::Paused {
                break;
            }
        }
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, "42\n");
    }
}
#[test]
fn cached_deferred_diagnostics_relocate_to_the_defining_file() {
    let compiler = Compiler::default();
    let lib = "pub fn fail() -> i64 { return true }\n";
    let main = "mod lib;\n";
    let first = compiler
        .compile(
            1,
            &[module("lib.ya", lib), module("main.ya", main)],
            Default::default(),
            true,
        )
        .unwrap();
    assert!(first.validate_all().is_err());
    let current_main = "mod lib;\n// Extra entry source moves the embedded core archive.\n";
    let second = compiler
        .compile(
            1,
            &[module("lib.ya", lib), module("main.ya", current_main)],
            Default::default(),
            true,
        )
        .unwrap();
    assert_eq!(unit(&second, "crate::lib").build, ModuleBuild::Reused);
    let error = second.validate_all().unwrap_err();
    let (name, source, local) =
        cyallo::modules::diagnostic_context(second.source(), second.source_modules(), &error)
            .unwrap();
    assert_eq!(name, "lib.ya");
    assert_eq!(
        &source[local.span.start as usize..local.span.end as usize],
        "true"
    );
}

#[test]
fn reused_modules_relocate_routine_slots_after_closure_generation() {
    let compiler = Compiler::default();
    let sources = |a: &str| {
        vec![
            module("a.ya", a),
            module(
                "b.ya",
                "pub fn call() -> i64 { return crate::a::answer() }\n",
            ),
            module(
                "main.ya",
                "mod a;\nmod b;\nlet f = b::call\nstd::io::print(f())\n",
            ),
        ]
    };
    let first = compiler
        .compile(
            2,
            &sources("pub fn answer() -> i64 { return 42 }\n"),
            Default::default(),
            false,
        )
        .unwrap();
    let old_slot = unit(&first, "crate::b").routines[0].0;
    drive(first, "42\n");
    let changed = "pub fn answer() -> i64 { let f = fn() -> i64 { return 43 }; return f() }\n";
    let second = compiler
        .compile(2, &sources(changed), Default::default(), true)
        .unwrap();
    assert_eq!(unit(&second, "crate::b").build, ModuleBuild::Reused);
    assert_ne!(unit(&second, "crate::b").routines[0].0, old_slot);
    drive(second, "43\n");
}

#[test]
fn cached_service_keys_relocate_with_source_files() {
    let compiler = Compiler::default();
    let source = |a: &str| {
        vec![
            module("a.ya", a),
            module(
                "b.ya",
                "pub trait T { proc emit(self: &mut Self) }\nrecord R { n: i64 }\nimpl T for R { proc emit(self: &mut Self) { std::io::print(self.n) } }\npub const KEY = std::service::ServiceKey::new<dyn T>(\"key\")\npub fn key() -> std::service::ServiceKey<dyn T> { return KEY }\npub proc install(ctx: &std::service::Context) -> std::service::Registration { return ctx.provide(KEY, R { n: 42 } as dyn T) }\n",
            ),
            module(
                "main.ya",
                "mod a;\nmod b;\nlet ctx = std::service::Context::new()\nlet registration = b::install(&ctx)\nctx.service(b::key()).emit()\n",
            ),
        ]
    };
    let first = compiler
        .compile(2, &source(""), Default::default(), false)
        .unwrap();
    drive(first, "42\n");
    let second = compiler
        .compile(
            2,
            &source("// Move subsequent files without changing their contents.\n"),
            Default::default(),
            false,
        )
        .unwrap();
    assert_eq!(unit(&second, "crate::b").build, ModuleBuild::Reused);
    drive(second, "42\n");
}

#[test]
fn checked_module_example_runs_in_all_engines_with_budget_handoffs() {
    use cyallo::backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
    };
    use std::time::Duration;
    let sources = [
        module(
            "model.ya",
            include_str!("../examples/modules/static/model.ya"),
        ),
        module(
            "math.ya",
            include_str!("../examples/modules/static/math.ya"),
        ),
        module(
            "main.ya",
            include_str!("../examples/modules/static/main.ya"),
        ),
    ];
    for deferred in [false, true] {
        let program = Arc::new(
            cyallo::modules::compile_modules(2, &sources, Default::default(), deferred).unwrap(),
        );
        let winch = TieredBackend::default().compile(program.clone()).unwrap();
        for id in 0..program.functions().len() {
            winch.precompile(id, Tier::Winch).unwrap();
        }
        assert!(winch.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(winch)),
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
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..10000 {
                match &engine {
                    Some(engine) if step % 3 != 0 => engine.run(&mut session, 1).unwrap(),
                    _ => {
                        session.run(1);
                    }
                }
                if session.status() != &Status::Paused {
                    break;
                }
            }
            assert_eq!(session.status(), &Status::Finished);
            assert_eq!(session.context.output, "42:retained\n");
        }
    }
}

#[test]
fn constant_dependencies_include_imported_function_bodies_and_cycles() {
    let library = "pub fn read() -> i64 { return crate::BASE }\n";
    let main = "mod lib;\npub const ANSWER = lib::read() + 1\npub const BASE = 41\nstd::io::print(ANSWER)\n";
    let p = cyallo::modules::compile_modules(
        1,
        &[module("lib.ya", library), module("main.ya", main)],
        Default::default(),
        false,
    )
    .unwrap();
    drive(p, "42\n");
    let cyclic = "mod lib;\npub const BASE = lib::read()\n";
    let error = cyallo::modules::compile_modules(
        1,
        &[module("lib.ya", library), module("main.ya", cyclic)],
        Default::default(),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, "E140");
    assert!(error.message.contains("cyclic constant"));
}
