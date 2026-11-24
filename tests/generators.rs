use cyallo::{compile, vm::{Context, Session, Status}};
use std::sync::Arc;

#[test]
fn pure_factory_and_owned_delegation_forward_input_and_return() {
    output(
        include_str!("../examples/concurrency/generators/delegation.yas"),
        "yielded: 7\ncomplete: 42\n",
    );
}
#[test]
fn construction_is_lazy_and_closed_generators_do_not_run() {
    output(
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

fn output(source: &str, expected: &str) {
    let program = Arc::new(compile(source).unwrap_or_else(|error| panic!("{error}\n{source}")));
    for stress in [false, true] {
        let mut session = Session::with_context(
            program.clone(),
            Context {
                gc: cyallo::runtime::gc::GcConfig { stress, ..Default::default() },
                ..Default::default()
            },
        )
        .unwrap();
        for _ in 0..100_000 {
            if session.run(1000) != &Status::Paused {
                break;
            }
        }
        assert_eq!(session.status(), &Status::Finished, "stress={stress}\n{source}");
        assert_eq!(session.context.output, expected, "stress={stress}\n{source}");
    }
}

