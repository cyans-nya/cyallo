use cyallo::{CompileOptions, Jobs, Session, Status, compile_for_execution, compile_with_options};
use std::sync::Arc;
fn source() -> String {
    let mut s = String::from(
        "record Box<T> { value: T }\nfn box_value<T>(value: T) -> Box<T> { return Box<T> { value: value } }\n",
    );
    for i in 0..80 {
        s.push_str(&format!(
            "fn routine{i}(value: i64) -> i64 {{ return value + {i} }}\n"
        ));
    }
    s.push_str("let mut total = 0\n");
    for i in 0..80 {
        s.push_str(&format!("total += routine{i}({i})\n"));
    }
    s.push_str("let boxed = box_value(total)\nstd::io::print(boxed.value)\n");
    s
}
#[test]
fn serial_and_parallel_compilation_publish_identical_units() {
    let source = source();
    let serial = compile_with_options(
        &source,
        CompileOptions {
            jobs: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let parallel = compile_with_options(
        &source,
        CompileOptions {
            jobs: 8,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        serial.bytecode().disassemble(),
        parallel.bytecode().disassemble()
    );
    assert_eq!(
        serial
            .functions()
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>(),
        parallel
            .functions()
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );
    for program in [serial, parallel] {
        let mut s = Session::new(Arc::new(program)).unwrap();
        s.run(100000);
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "6320\n");
    }
}
#[test]
fn concurrent_preparation_initializes_only_selected_deferred_functions() {
    let p = Arc::new(
        compile_for_execution(
            &source(),
            CompileOptions {
                jobs: 8,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    assert!(!p.functions()[1].is_analyzed());
    Jobs(4).map(&[0, 1, 2, 3], 1, |_, _| {
        p.prepare_units(&[1, 2, 3, 4, 1], Jobs(4)).unwrap()
    });
    assert!(p.functions()[1..5].iter().all(|f| f.is_analyzed()));
    assert!(!p.functions()[5].is_analyzed());
    let units = p.compilation_units().collect::<Vec<_>>();
    assert_eq!(units[1].id, 1);
    assert!(std::ptr::eq(
        units[1].checked_ir().unwrap(),
        &*p.functions()[1]
    ));
    assert!(!units[1].bytecode().unwrap().instructions.is_empty());
    assert!(p.prepare_units(&[usize::MAX], Jobs(4)).is_err());
    p.validate_all_with_jobs(Jobs(8)).unwrap();
    assert!(p.functions().iter().all(|f| f.is_analyzed()));
}
#[test]
fn diagnostics_are_deterministic_across_worker_counts() {
    let source = "fn first() -> i64 { return unknown }\nfn second() -> i64 { return false }\nfn third() -> i64 { return 0 }\nfn fourth() -> i64 { return 0 }\n";
    let a = compile_with_options(
        source,
        CompileOptions {
            jobs: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    for _ in 0..5 {
        let b = compile_with_options(
            source,
            CompileOptions {
                jobs: 8,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(a, b);
    }
}
#[test]
fn modules_parse_independently_with_stable_archive_spans() {
    use cyallo::modules::{SourceModule, compile_modules};
    let sources = [
        SourceModule::new("a.ya", "pub fn value() -> i64 { return 42 }\n"),
        SourceModule::new(
            "b.ya",
            "use crate::a::value\npub fn doubled() -> i64 { return value() * 2 }\n",
        ),
        SourceModule::new(
            "main.ya",
            "mod a;\nmod b;\nuse crate::b::doubled\nstd::io::print(doubled())\n",
        ),
    ];
    let a = compile_modules(
        2,
        &sources,
        CompileOptions {
            jobs: 1,
            ..Default::default()
        },
        false,
    )
    .unwrap();
    let b = compile_modules(
        2,
        &sources,
        CompileOptions {
            jobs: 8,
            ..Default::default()
        },
        false,
    )
    .unwrap();
    assert_eq!(a.bytecode().disassemble(), b.bytecode().disassemble());
    let mut s = Session::new(Arc::new(b)).unwrap();
    s.run(10000);
    assert_eq!(s.context.output, "84\n");
    let broken = [
        SourceModule::new("a.ya", "fn broken(\n"),
        SourceModule::new("b.ya", "fn other(\n"),
    ];
    assert_eq!(
        compile_modules(
            1,
            &broken,
            CompileOptions {
                jobs: 1,
                ..Default::default()
            },
            false
        )
        .unwrap_err(),
        compile_modules(
            1,
            &broken,
            CompileOptions {
                jobs: 8,
                ..Default::default()
            },
            false
        )
        .unwrap_err()
    );
}

#[test]
fn empty_blocks_and_empty_records_parse_as_independent_units() {
    let source = "record Empty {}\nlet e = Empty {}\nlet flag = true\nif flag {}\nwhile false {}\nlet last = 0\nfor i = 1 to last {}\nstd::io::print(42)\n";
    for jobs in [1, 4] {
        let p = compile_with_options(
            source,
            CompileOptions {
                jobs,
                ..Default::default()
            },
        )
        .unwrap();
        let mut s = Session::new(Arc::new(p)).unwrap();
        s.run(10000);
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "42\n");
    }
}

#[test]
fn serial_and_parallel_native_units_preserve_effect_order_and_counters() {
    use cyallo::backend::{CraneliftConfig, Executable, NativeBackend, OptimizationLevel};
    let mut source = String::new();
    for i in 0..8 {
        source.push_str(&format!(
            "#[inline(never)]\nproc p{i}(n: i64) -> i64 {{ std::io::print(n); return n + {i} }}\n"
        ));
    }
    source.push_str("let mut total = 0\n");
    for i in 0..8 {
        source.push_str(&format!("total += p{i}({i})\n"));
    }
    source.push_str("std::io::print(total)\n");
    for optimization in [OptimizationLevel::None, OptimizationLevel::Speed] {
        let mut serial = None;
        for jobs in [1, 4] {
            let p = Arc::new(
                compile_with_options(
                    &source,
                    CompileOptions {
                        jobs,
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
            let a = CraneliftConfig {
                optimization,
                ..Default::default()
            }
            .compile(p.clone())
            .unwrap();
            let mut s = Session::new(p.clone()).unwrap();
            while s.status() == &Status::Paused {
                a.run(&mut s, 7).unwrap();
            }
            assert_eq!(s.status(), &Status::Finished);
            let result = (
                s.context.output.clone(),
                s.instruction_count(),
                (0..p.functions().len())
                    .map(|fi| (s.function_instruction_count(fi), s.function_call_count(fi)))
                    .collect::<Vec<_>>(),
            );
            if let Some(expected) = &serial {
                assert_eq!(&result, expected);
            } else {
                serial = Some(result);
            }
        }
    }
}
