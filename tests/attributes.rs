use cyallo::{
    Session, Status,
    backend::{CompilationScope, CraneliftConfig, Executable, NativeBackend, Tier, TieredBackend},
    compile, compile_for_execution,
};
use cyallo_ast::{CompileHint, HeatHint, InlineHint};
use std::sync::Arc;
#[test]
fn attributes_survive_parallel_parsing_and_specialization() {
    let source = "#[hint(hot)]\n#[opt(eager)]\n#[inline]\nfn identity<T>(n: T) -> T { return n }\nstd::io::print(identity(42))\n";
    for jobs in [1, 4] {
        let p = cyallo::compile_with_options(
            source,
            cyallo::CompileOptions {
                jobs,
                ..Default::default()
            },
        )
        .unwrap();
        let f = &p.functions()[1];
        assert_eq!(f.attributes.heat, HeatHint::Hot);
        assert_eq!(f.attributes.compilation, CompileHint::Eager);
        assert_eq!(f.attributes.inlining, InlineHint::Preferred);
        assert_eq!(f.span.start, 0);
    }
}
#[test]
fn malformed_attributes_and_conflicts_are_rejected() {
    for attrs in [
        "#[hint(warm)]",
        "#[opt(unknown)]",
        "#[inline(sometimes)]",
        "#[hint(hot)]\n#[hint(cold)]",
        "#[opt(always_lazy)]\n#[inline(always)]",
    ] {
        assert!(
            compile(&format!(
                "{attrs}\nfn f(n: i64) -> i64 {{ return n + 1 }}\n"
            ))
            .is_err(),
            "{attrs}"
        );
    }
    assert!(compile("#[inline]\nlet n = 1\n").is_err());
    assert!(compile("trait T { #[inline]\nfn f(self) -> i64 }\n").is_err());
}
#[test]
fn never_and_always_lazy_are_honored_by_all_layouts() {
    let source = "#[opt(always_lazy)]\nfn interpreted(n: i64) -> i64 { return n + 1 }\n#[inline(never)]\nfn outlined(n: i64) -> i64 { return n + 2 }\nlet n = interpreted(10)\nstd::io::print(outlined(n))\n";
    let p = Arc::new(compile(source).unwrap());
    for scope in [CompilationScope::Combined, CompilationScope::PerRoutine] {
        let a = CraneliftConfig {
            scope,
            ..Default::default()
        }
        .compile(p.clone())
        .unwrap();
        let mut s = Session::new(p.clone()).unwrap();
        s.enable_reporting(std::time::Instant::now());
        a.run(&mut s, 1000).unwrap();
        assert_eq!(s.status(), &Status::Finished);
        assert_eq!(s.context.output, "13\n");
        assert!(s.execution_report().unwrap().functions[1].instructions[0] > 0);
        assert_eq!(
            s.execution_report().unwrap().functions[1].instructions[1..]
                .iter()
                .sum::<u64>(),
            0
        );
        let mut s = Session::new(p.clone()).unwrap();
        a.run(&mut s, 1000).unwrap();
        assert_eq!(s.inlined_call_count(), 0);
    }
    let a = TieredBackend::default().compile(p).unwrap();
    assert!(a.precompile(1, Tier::CraneliftFast).is_err());
}
#[test]
fn forced_inline_can_use_a_real_frame_for_a_branching_function() {
    let p=Arc::new(compile("#[inline(always)]\nfn choose(n: i64) -> i64 { if n > 0 { return n + 1 }; return 0 }\nstd::io::print(choose(10))\n").unwrap());
    let a = CraneliftConfig::speed().compile(p.clone()).unwrap();
    let mut s = Session::new(p).unwrap();
    a.run(&mut s, 1000).unwrap();
    assert_eq!(s.context.output, "11\n");
    assert_eq!(s.body_inlined_call_count(), 1);
}
#[test]
fn inspection_does_not_analyze_unrelated_cold_bodies() {
    let options = cyallo::inspection::InspectionOptions {
        notices: true,
        ..Default::default()
    };
    let observer = Arc::new(cyallo::inspection::Inspection::new(options));
    let p = Arc::new(
        compile_for_execution(
            "fn cold() -> i64 { return missing }\nstd::io::print(1)\n",
            Default::default(),
        )
        .unwrap()
        .with_inspection(observer),
    );
    let a = TieredBackend::default().compile(p.clone()).unwrap();
    let mut s = Session::new(p.clone()).unwrap();
    a.run(&mut s, 1000).unwrap();
    p.emit_inspection();
    assert!(!p.functions()[1].is_analyzed());
    assert_eq!(s.context.output, "1\n");
}

#[test]
fn attributes_attach_to_exported_routines_impls_and_property_getters() {
    let ast=cyallo_parser::parse_modern("#[hint(cold)]\npub proc exported() {}\ntrait T { proc run(self: &mut Self) }\nrecord R { n: i64 }\nimpl T for R { #[opt(lazy)]\n#[inline(never)]\nproc run(self: &mut Self) { self.n += 1 } }\nproperty std::service::Context.answer: i64 { #[hint(hot)]\nproc get() { return 42 } }\n",false).unwrap();
    assert_eq!(ast.functions[0].attributes.heat, HeatHint::Cold);
    assert_eq!(&*ast.exports[0], "exported");
    assert_eq!(
        ast.implementations[0].methods[0].attributes.compilation,
        CompileHint::Lazy
    );
    assert_eq!(
        ast.implementations[0].methods[0].attributes.inlining,
        InlineHint::Never
    );
    assert_eq!(ast.properties[0].getter.attributes.heat, HeatHint::Hot);
}

#[test]
fn imported_attribute_spans_stay_in_the_source_archive() {
    use cyallo::modules::{SourceModule, compile_modules};
    let sources = [
        SourceModule::new(
            "math.ya",
            "#[hint(hot)]\n#[inline(preferred)]\npub fn inc(n: i64) -> i64 { return n + 1 }\n",
        ),
        SourceModule::new(
            "main.ya",
            "mod math;\nuse crate::math::inc\nstd::io::print(inc(41))\n",
        ),
    ];
    for jobs in [1, 4] {
        let p = compile_modules(
            1,
            &sources,
            cyallo::CompileOptions {
                jobs,
                ..Default::default()
            },
            false,
        )
        .unwrap();
        let f = p
            .functions()
            .iter()
            .find(|f| f.attributes.heat == HeatHint::Hot)
            .unwrap();
        assert_eq!(
            f.attributes.heat_span.unwrap().source_text(p.source()),
            "#[hint(hot)]"
        );
        assert_eq!(
            f.attributes.inline_span.unwrap().source_text(p.source()),
            "#[inline(preferred)]"
        );
        let mut s = Session::new(Arc::new(p)).unwrap();
        s.run(1000);
        assert_eq!(s.context.output, "42\n");
    }
}
