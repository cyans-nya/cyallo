use cyallo::backend::{
    CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
};
use cyallo::{CompileOptions, Context, Session, Status, compile, compile_for_execution};
use cyallo_ast::{ExprKind, Locality, MemoryKind, StmtKind, Type};
use std::{collections::HashMap, sync::Arc, time::Duration};

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
        for engine in engines {
            let mut session = Session::with_context(
                program.clone(),
                Context {
                    gc: cyallo::runtime::gc::GcConfig::stress(),
                    ..Default::default()
                },
            )
            .unwrap();
            for step in 0..10_000 {
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
            assert_eq!(session.context.output, expected);
        }
    }
}

#[test]
fn parser_keeps_storage_and_locality_as_independent_semantic_fields() {
    for (syntax, kind, locality) in [
        ("gc", MemoryKind::Gc, Locality::Local),
        ("rc", MemoryKind::Rc, Locality::Local),
        ("@local gc", MemoryKind::Gc, Locality::Local),
        ("@local rc", MemoryKind::Rc, Locality::Local),
        ("@shared gc", MemoryKind::Gc, Locality::Shared),
        ("@shared rc", MemoryKind::Rc, Locality::Shared),
    ] {
        let source = format!("let value: {syntax} i64 = {syntax} 42");
        let ast = cyallo_parser::parse(&source).unwrap();
        let StmtKind::Binding(_, Some(ty), _, _, value) = &ast.body[0].kind else {
            panic!("expected annotated binding")
        };
        assert_eq!(
            ty,
            &Type::Managed {
                kind,
                locality,
                lifetime: None,
                pointee: Box::new(Type::Integer)
            }
        );
        assert!(
            matches!(&value.kind, ExprKind::ManagedNew(actual_kind, actual_locality, None, _) if *actual_kind == kind && *actual_locality == locality)
        );
        assert_eq!(
            &source[value.span.start as usize..value.span.end as usize],
            format!("{syntax} 42")
        );
        let inferred =
            cyallo_ir::inference::expression_type(value, &HashMap::new(), &Default::default())
                .unwrap();
        assert_eq!(&inferred, ty);
        assert_eq!(
            ty.to_string(),
            format!(
                "{}{kind} i64",
                if locality == Locality::Shared {
                    "@shared "
                } else {
                    ""
                }
            )
        );
    }
}

#[test]
fn local_managed_language_example_preserves_aliases_rc_and_weak_ownership() {
    parity(
        include_str!("../examples/concurrency/ownership/local-managed.ya"),
        "42\n2\n7\ntrue\n",
    );
}

#[test]
fn local_annotations_survive_generic_inference_and_qualified_module_lookup() {
    parity(
        r#"
        fn identity<T>(value: @local gc T) -> gc T { return value }
        proc keep<T>(value: @local rc T) -> rc T { return value }
        let state = identity(@local gc 42)
        let owner = keep(@local rc 7)
        let other = rc::clone(&owner)
        let same = gc::clone(&state)
        std::io::print(same.*, ":", other.*)
    "#,
        "42:7\n",
    );
}

#[test]
fn locality_is_not_erased_by_type_comparison_or_substitution() {
    let local =
        cyallo_ir::memory::managed_type(MemoryKind::Rc, Locality::Local, None, Type::Integer);
    let shared =
        cyallo_ir::memory::managed_type(MemoryKind::Rc, Locality::Shared, None, Type::Integer);
    assert!(!cyallo_ir::memory::same_regions(&local, &shared));
    assert!(!cyallo_ir::declarations::equivalent_types(&local, &shared));
    let ast = cyallo_parser::parse("proc inspect(value: @shared rc i64) {}").unwrap();
    assert_eq!(ast.functions[0].params[0].ty, shared);
    let mut ast = cyallo_parser::parse(
        r#"
        fn identity<T>(value: T) -> T { return value }
        let value: @shared rc i64 = @shared rc 42
        let kept = identity<@shared rc i64>(value)
    "#,
    )
    .unwrap();
    cyallo_ir::generics::specialize(&mut ast, CompileOptions::default()).unwrap();
    let specialized = ast
        .functions
        .iter()
        .find(|function| function.type_params.is_empty())
        .unwrap();
    assert_eq!(specialized.params[0].ty, shared);
    assert_eq!(specialized.result, shared);
}

#[test]
fn shared_syntax_cannot_silently_allocate_or_promote_local_storage() {
    for source in [
        "let value = @shared gc 42",
        "let value = @shared rc 42",
        "proc accept(value: @shared gc i64) {}",
        "record Container { value: @shared rc i64 }",
        "let value: [1]@shared gc i64 = [gc 1]",
        "let value = gc 42; let same: @shared gc i64 = value",
    ] {
        assert_eq!(compile(source).unwrap_err().code, "E179", "{source}");
        assert_eq!(
            compile_for_execution(source, CompileOptions::default())
                .unwrap_err()
                .code,
            "E179",
            "{source}"
        );
    }
}

#[test]
fn legacy_sigils_and_invalid_or_conflicting_modifiers_have_targeted_diagnostics() {
    for source in [
        "let value = @gc 1",
        "let value = @rc 1",
        "proc accept(value: @gc i64) {}",
        "proc accept(value: @rc i64) {}",
        "let value = @local @shared gc 1",
        "let value = @local @local rc 1",
        "let value = @shared @local gc 1",
        "let value = @unknown gc 1",
        "let value = @local 1",
        "let value: @shared i64 = 1",
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, "E178", "{source}: {error}");
        assert!(source[error.span.start as usize..error.span.end as usize].starts_with('@'));
    }
    use cyallo_ast::diagnostic::ManagedSyntaxError;
    assert_eq!(
        ManagedSyntaxError::LegacySigil(MemoryKind::Gc).to_string(),
        "use `gc` without `@`; locality modifiers are `@local` and `@shared`"
    );
}

#[test]
fn explicit_local_managed_values_still_cannot_cross_coroutines() {
    for source in [
        "let state = @local gc 42; let task = std::task::spawn(proc() -> i64 { return state.* })",
        "let state = @local rc 42; let task = std::task::spawn(proc() -> i64 { return state.* })",
        "let channel = std::channel::unbounded<@local gc i64>()",
        "let channel = std::channel::unbounded<@local rc i64>()",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}
