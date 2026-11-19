use cyallo::vm::{Session, Status};
use cyallo::{CompileOptions, Dialect, compile, compile_with_options, ir::RoutineKind};
use std::sync::Arc;
#[test]
fn returning_procedures_and_private_mutation() {
    let source = "let mut counter = 0\nproc advance(n: &mut i64) -> i64 { n.* += 1; return n.* }\nfn work(n: i64) -> i64 { let mut copy = [n,2]; copy[0] += 3; let mut x = 0; for i in 1..=3 { x += i }; return copy[0] + x }\nstd::io::print(advance(&mut counter), work(1), counter)\nadvance(&mut counter)\nadvance(&mut counter)\nstd::io::print(counter)\n";
    let program = Arc::new(compile(source).unwrap());
    assert_eq!(program.functions()[1].kind, RoutineKind::Procedure);
    assert!(program.functions()[1].effects.writes_references);
    assert!(program.functions()[2].pure);
    assert!(program.functions()[2].effects.is_observably_pure());
    assert!(program.functions()[2].effects.may_allocate);
    assert!(program.functions()[2].effects.may_diverge);
    let mut session = Session::new(program).unwrap();
    session.run(10000);
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "1101\n3\n");
}
#[test]
fn rejects_effects_including_transitive_calls() {
    for source in [
        "let mut n = 1\nfn f() -> i64 { return n }\n",
        "let mut n = 1\nfn f() -> i64 { n = 2; return n }\n",
        "fn f() -> i64 { std::io::print(1); return 1 }\n",
        "fn f() -> f64 { return RAND(1) }\n",
        "proc effect() -> i64 { return 1 }\nfn f() -> i64 { return effect() }\n",
        "fn outer() -> i64 { return inner() }\nfn inner() -> i64 { std::io::print(1); return 1 }\n",
        "fn f(n: &mut i64) -> i64 { n.* += 1; return n.* }\n",
        "proc f() -> i64 { if true { return 1 } }\n",
        "proc f() -> i64 { return }\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
fn recursion_and_checked_failures_remain_observable() {
    let program =
        Arc::new(compile("fn forever(n: i64) -> i64 { return forever(n) }\nforever(0)\n").unwrap());
    assert!(program.functions()[1].effects.may_diverge);
    let mut session = Session::new(program).unwrap();
    session.run(100);
    assert_eq!(session.status(), &Status::Paused);
    let program = Arc::new(compile("const n = 5\nfn f() -> i64 { return n / 0 }\nf()\n").unwrap());
    // Const uses have already become immediate values.
    assert!(!program.functions()[1].effects.reads_immutable);
    assert!(program.functions()[1].effects.may_fail);
    let mut session = Session::new(program).unwrap();
    session.run(100);
    assert!(matches!(session.status(), Status::Failed(_)));
}
#[test]
fn compatibility_functions_keep_effects() {
    let source = "FUNCTION F() RETURNS INTEGER\n OUTPUT 1\n RETURN 2\nENDFUNCTION\nOUTPUT F()\n";
    assert!(compile(source).is_err());
    let program = compile_with_options(
        source,
        CompileOptions {
            dialect: Dialect::Compatibility,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!program.functions()[1].pure);
    assert!(program.functions()[1].effects.io);
}
