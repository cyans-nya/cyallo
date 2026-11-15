use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::runtime::{GeneratorError, RuntimeError};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use std::{sync::Arc, time::Duration};

fn parity(source: &str, expected: &str) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                compile_for_execution(source, CompileOptions::default())
            } else {
                compile(source)
            }
            .unwrap(),
        );
        let winch = TieredBackend::default().compile(program.clone()).unwrap();
        for function in 0..program.functions().len() {
            winch.precompile(function, Tier::Winch).unwrap();
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
        for (index, engine) in engines.into_iter().enumerate() {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..100_000 {
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
            assert_eq!(
                session.status(),
                &Status::Finished,
                "deferred={deferred}, engine={index}, output={:?}",
                session.context.output
            );
            assert_eq!(session.context.output, expected);
        }
    }
}

#[test]
fn lazy_capturing_factory_yields_and_completes() {
    parity(
        include_str!("../examples/concurrency/generators/sequence.ya"),
        "10\n11\n12\ncomplete: 3\n",
    );
}
#[test]
fn typed_result_resume_input_is_an_ordinary_value() {
    parity(
        include_str!("../examples/concurrency/generators/resume-result.ya"),
        "request\ncomplete: 42\nrequest\nerror: unavailable\ncomplete: -1\n",
    );
}
#[test]
fn pure_factory_and_owned_delegation_forward_input_and_return() {
    parity(
        include_str!("../examples/concurrency/generators/delegation.ya"),
        "yielded: 7\ncomplete: 42\n",
    );
}
#[test]
fn construction_is_lazy_and_closed_generators_do_not_run() {
    parity(
        "let create = gen proc() -> Generator<i64, (), ()> { std::io::print(\"started\"); yield 1 }\nlet mut first = create()\nfirst.close()\nlet mut second = create()\nlet item = second.start()\nstd::io::print(item.yielded())\nsecond.close()\n",
        "started\n1\n",
    );
}
#[test]
fn generator_syntax_and_types_reject_invalid_programs() {
    for source in [
        "yield 1\n",
        "let bad = proc() { yield 1 }\n",
        "let bad = gen { yield 1 }\n",
        "let n = 1\nlet bad = gen fn() -> Generator<i64, (), ()> { yield n }\n",
        "let bad = gen proc() -> Generator<i64, (), ()> { yield \"wrong\" }\n",
        "let bad = gen proc() -> Generator<&i64, (), ()> { let n = 1; yield &n }\n",
        "let create = gen proc() -> Generator<i64, i64, ()> { yield 1 }\nlet mut g = create()\nlet item = g.resume(\"wrong\")\n",
        "let create = gen proc() -> Generator<i64, (), ()> { yield 1 }\nlet g = create()\nlet item = g.resume()\n",
    ] {
        assert!(compile(source).is_err(), "accepted {source}");
    }
}
#[test]
fn resume_after_close_or_completion_is_rejected() {
    for body in [
        "g.close(); g.resume()",
        "let first = g.start(); let last = g.resume(); g.resume()",
    ] {
        let source = format!(
            "let create = gen proc() -> Generator<i64, (), ()> {{ yield 1 }}\nlet mut g = create()\n{body}\n"
        );
        let program = Arc::new(compile(&source).unwrap());
        let mut session = Session::new(program).unwrap();
        session.run(10_000);
        assert!(
            matches!(session.status(), Status::Failed(failure) if failure.error == RuntimeError::Generator(GeneratorError::Finished)),
            "{:?}",
            session.status()
        );
    }
}

#[test]
fn saved_managed_values_survive_compaction_between_resumes() {
    parity(
        r#"
record Item { number: i64 }
let make = gen proc() -> Generator<gc Item, (), i64> {
    let saved = gc Item { number: 42 }
    yield saved
    return saved.number
}
let mut values = make()
let item = values.start()
let saved = item.yielded()
for i in 0..=30 { let garbage = gc Item { number: i } }
std::io::print(saved.number)
let finished = values.resume()
std::io::print(finished.completed())
"#,
        "42\n42\n",
    );
}

#[test]
fn close_releases_rc_parameters_and_owned_delegates() {
    parity(
        r#"
record Item { number: i64 }
let child = gen proc(owner: rc Item) -> Generator<i64, (), ()> { yield owner.number }
let outer = gen proc(owner: rc Item) -> Generator<i64, (), ()> { yield from child(owner) }
let owner = rc Item { number: 42 }
let weak = owner.downgrade()
let mut values = outer(owner)
let item = values.start()
std::io::print(item.yielded())
values.close()
let expired = weak.upgrade()
std::io::print(expired.is_none())
"#,
        "42\ntrue\n",
    );
}

#[test]
fn a_session_can_migrate_with_parked_generator_and_host_roots() {
    let program = Arc::new(
        compile(include_str!(
            "../examples/concurrency/generators/sequence.ya"
        ))
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
    let root = session.gc_scope().allocate_integer(99).unwrap();
    for _ in 0..100 {
        session = std::thread::spawn(move || {
            session.run(1);
            session
        })
        .join()
        .unwrap();
        if session.status() == &Status::Finished {
            break;
        }
    }
    session.run(10_000);
    assert_eq!(session.status(), &Status::Finished);
    assert_eq!(session.context.output, "10\n11\n12\ncomplete: 3\n");
    assert_eq!(
        session.gc_scope().no_gc().get(&root).unwrap(),
        &cyallo::Value::Integer(99)
    );
}

#[test]
fn start_and_resume_enforce_the_generator_lifecycle() {
    for (body, expected) in [
        ("g.resume(1)", GeneratorError::NotStarted),
        (
            "let first = g.start(); g.start()",
            GeneratorError::AlreadyStarted,
        ),
    ] {
        let source = format!(
            "let make = gen proc() -> Generator<i64, i64, ()> {{ yield 1 }}\nlet mut g = make()\n{body}\n"
        );
        let mut session = Session::new(Arc::new(compile(&source).unwrap())).unwrap();
        session.run(10_000);
        assert!(
            matches!(session.status(), Status::Failed(failure) if failure.error == RuntimeError::Generator(expected))
        );
    }
    let source = "let make = gen proc() -> Generator<i64, i64, ()> { yield 1 }; let mut g = make(); let first = g.start(); g.resume()";
    assert!(compile(source).is_err());
    parity(
        "let make = gen proc() -> Generator<i64, (), i64> { yield 1; return 2 }; let mut g = make(); std::io::print(g.next().yielded()); std::io::print(g.next().completed())",
        "1\n2\n",
    );
}
