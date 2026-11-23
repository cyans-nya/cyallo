use cyallo::{
    compile, compile_for_execution,
    modules::{Compiler, SourceModule},
    vm::{Session, Status},
};
use std::sync::Arc;

#[test]
fn scalar_routine_crosses_both_branches_through_the_verified_interpreter() {
    let source = include_str!("../examples/routine-ir.yas");
    for program in [
        compile(source).unwrap(),
        compile_for_execution(source, Default::default()).unwrap(),
    ] {
        assert_eq!(run(program), "24\n13\n");
    }
}

#[test]
fn indexed_bodies_participate_in_module_cache_invalidation_and_constant_evaluation() {
    let compiler = Compiler::default();
    for (increment, expected) in [(1, "23\n"), (1, "23\n"), (2, "25\n")] {
        let library = format!("pub fn bump(n: i64) -> i64 {{ return n + {increment} }}\n");
        let sources = [
            SourceModule {
                path: "lib.ya".into(),
                source: library,
            },
            SourceModule {
                path: "main.ya".into(),
                source: "mod lib;\nconst answer = lib::bump(10)\nstd::io::print(answer + lib::bump(11))\n"
                    .into(),
            },
        ];
        let program = compiler
            .compile(1, &sources, Default::default(), true)
            .unwrap();
        assert_eq!(run(program), expected);
    }
}

#[test]
fn direct_calls_cross_modules_through_the_verified_interpreter() {
    let compiler = Compiler::default();
    for (increment, expected) in [(1, "22\n"), (2, "24\n")] {
        let sources = [
            SourceModule {
                path: "lib.ya".into(),
                source: format!(
                    "fn bump(n: i64) -> i64 {{ return n + {increment} }}\npub fn twice(n: i64) -> i64 {{ return bump(bump(n)) + 20 }}\n"
                ),
            },
            SourceModule {
                path: "main.ya".into(),
                source: "mod lib;\nuse crate::lib::twice\nstd::io::print(twice(0))\n".into(),
            },
        ];
        let program = compiler
            .compile(1, &sources, Default::default(), true)
            .unwrap();
        assert_eq!(run(program), expected);
    }
}

#[test]
fn call_sites_in_indexed_bodies_keep_the_resolvers_diagnostics_and_timing() {
    // The eager entry point reports each failure although the routine is never
    // called.
    for (library, main) in [
        // A private callee in another module.
        (
            "fn hidden(n: i64) -> i64 { return n }\n",
            "mod lib;\nfn f(n: i64) -> i64 { return crate::lib::hidden(n) }\nstd::io::print(1)\n",
        ),
        // An unresolved callee is reported when the routine is checked.
        (
            "pub fn shown(n: i64) -> i64 { return n }\n",
            "mod lib;\nfn f(n: i64) -> i64 { return crate::lib::missing(n) }\nstd::io::print(1)\n",
        ),
    ] {
        let sources = [
            SourceModule {
                path: "lib.ya".into(),
                source: library.into(),
            },
            SourceModule {
                path: "main.ya".into(),
                source: main.into(),
            },
        ];
        let eager = cyallo::modules::compile_modules(1, &sources, Default::default(), false);
        assert!(eager.is_err(), "{main}");
        // A body check, unlike a declaration error, waits for first use in the
        // deferred entry point. This matches the resolver before the indexed path.
        let deferred = cyallo::modules::compile_modules(1, &sources, Default::default(), true);
        assert!(deferred.is_ok(), "{main}");
    }
}

fn run(program: cyallo::ir::Program) -> String {
    let mut session = Session::new(Arc::new(program)).unwrap();
    for _ in 0..128 {
        match session.run(1) {
            Status::Finished => break,
            Status::Paused => {}
            other => panic!("unexpected interpreter status {other:?}"),
        }
    }
    assert!(matches!(session.status(), Status::Finished));
    session.context.output
}
