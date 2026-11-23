use cyallo::{
    modules::{Compiler, SourceModule},
    vm::{Context, Session, Status},
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
