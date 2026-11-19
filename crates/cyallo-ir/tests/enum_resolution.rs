use cyallo_ast::*;
use cyallo_ir::{composites, namespace};
fn resolve(source: &str) -> cyallo_ir::module_graph::ResolvedProgram {
    namespace::single(cyallo_parser::parse(source).unwrap()).unwrap()
}
#[test]
fn enum_variants_and_import_aliases_retain_defining_declarations() {
    let graph = resolve(
        "pub enum Choice { Empty, Value: i64 }\nuse Choice::{Value as Make, Empty as Nothing}\nlet ctor = Make\nlet value = Nothing\n",
    );
    let module = &graph.modules[graph.entry_index()];
    let declaration = &module.source.enums[0];
    let variant = declaration.variants[1].resolved.as_ref().unwrap();
    let imported = module
        .bindings
        .iter()
        .find(|b| b.name.as_ref() == "Make")
        .unwrap();
    assert_eq!(variant.declaration.0, imported.declaration);
    assert!(
        module
            .source
            .functions
            .iter()
            .any(|f| f.name == variant.symbol && f.origin == RoutineOrigin::EnumConstructor)
    );
    let StmtKind::Binding(_, _, _, _, value) = &module.source.body[1].kind else {
        panic!()
    };
    assert!(
        matches!(&value.kind,ExprKind::Call(symbol,args) if *symbol==declaration.variants[0].resolved.as_ref().unwrap().symbol && args.is_empty())
    );
}
#[test]
fn payload_contracts_are_nominal_and_constructor_bodies_keep_variant_ids() {
    let mut graph = specialize(
        "enum First { Value: i64 }\nenum Second { Value: i64 }\nlet a = First::Value(1)\nlet b = Second::Value(2)\n",
    );
    let native = graph.native.clone();
    let entry = graph.entry_index();
    let (types, contracts) = graph.with_sources(|asts| {
        let types = composites::resolve_modules(asts, &native).unwrap();
        let contracts = asts[entry]
            .functions
            .iter()
            .filter(|f| f.origin == RoutineOrigin::EnumConstructor)
            .cloned()
            .collect::<Vec<_>>();
        (types, contracts)
    });
    let declarations = &graph.modules[graph.entry_index()].source.enums;
    let first = &types[declarations[0].name.as_ref()];
    let second = &types[declarations[1].name.as_ref()];
    assert_ne!(first, second);
    let Type::Enum(first) = first else { panic!() };
    assert_eq!(first.variants[0].payload, Some(Type::Integer));
    assert_eq!(contracts.len(), 2);
    for constructor in contracts {
        let Type::Enum(contract) = &constructor.result else {
            panic!()
        };
        let StmtKind::Return(Expr {
            kind: ExprKind::EnumConstruct(variant, ty, Some(payload)),
            ..
        }) = &constructor.body[0].kind
        else {
            panic!()
        };
        assert_eq!(ty, &constructor.result);
        assert!(contract.variants.iter().any(|v| v.declaration == *variant));
        assert!(matches!(&payload.kind,ExprKind::Name(name) if *name==constructor.params[0].name));
    }
}
#[test]
fn reexports_keep_variant_identity_and_cannot_cross_private_modules() {
    let graph = resolve(
        "mod implementation { pub enum E { Value: i64 } }\npub use implementation::E\nuse E::Value as make\nlet value = make(1)\n",
    );
    let root = &graph.modules[graph.entry_index()];
    let child = graph
        .modules
        .iter()
        .find(|m| m.path.as_ref() == "crate::implementation")
        .unwrap();
    let original = &child.source.enums[0].variants[0]
        .resolved
        .as_ref()
        .unwrap()
        .declaration;
    assert_eq!(
        root.bindings
            .iter()
            .find(|b| b.name.as_ref() == "make")
            .unwrap()
            .declaration,
        original.0
    );
    let error = namespace::single(
        cyallo_parser::parse(
            "mod secret { enum E { Value: i64 } }\nlet value = secret::E::Value(1)\n",
        )
        .unwrap(),
    )
    .unwrap();
    let StmtKind::Binding(_, _, _, _, value) =
        &error.modules[error.entry_index()].source.body[0].kind
    else {
        panic!()
    };
    assert!(matches!(value.kind, ExprKind::Invalid(_)));
}
#[test]
fn unit_variant_call_has_a_typed_diagnostic() {
    let error = namespace::single(
        cyallo_parser::parse("enum E { Empty }\nlet value = E::Empty()\n").unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code, "E184");
    assert!(error.message.contains("does not take parentheses"));
}
#[test]
fn recursive_enum_layouts_require_indirection() {
    let mut direct = resolve("enum List { End, Next: List }\n");
    let native = direct.native.clone();
    let error = direct
        .with_sources(|asts| {
            cyallo_ir::generics::specialize_modules(
                asts,
                &native,
                cyallo_ir::CompileOptions::default(),
            )
        })
        .unwrap_err();
    assert_eq!(error.code, "E184");
    let mut indirect = specialize("enum List { End, Next: gc List }\n");
    let native = indirect.native.clone();
    let types = indirect
        .with_sources(|asts| composites::resolve_modules(asts, &native))
        .unwrap();
    let name = &indirect.modules[indirect.entry_index()].source.enums[0].name;
    let Type::Enum(contract) = &types[name.as_ref()] else {
        panic!()
    };
    assert!(
        matches!(&contract.variants[1].payload,Some(Type::Managed{pointee,..}) if matches!(pointee.as_ref(),Type::EnumRef(..)))
    );
}
fn specialize(source: &str) -> cyallo_ir::module_graph::ResolvedProgram {
    let mut graph = resolve(source);
    let native = graph.native.clone();
    graph
        .with_sources(|asts| {
            cyallo_ir::generics::specialize_modules(
                asts,
                &native,
                cyallo_ir::CompileOptions::default(),
            )
        })
        .unwrap();
    graph
}
#[test]
fn generic_constructors_infer_payloads_and_contextual_unit_arguments() {
    let graph = specialize(
        "enum Maybe<T> { Some: T, None }\nlet some = Maybe::Some(3)\nlet none: Maybe<i64> = Maybe::None\n",
    );
    let source = &graph.modules[graph.entry_index()].source;
    assert_eq!(source.enums.len(), 1);
    let Type::Enum(contract) = &source
        .functions
        .iter()
        .find(|f| f.origin == RoutineOrigin::EnumConstructor)
        .unwrap()
        .result
    else {
        panic!()
    };
    assert_eq!(contract.identity.arguments, vec![Type::Integer]);
    assert_eq!(contract.variants[0].payload, Some(Type::Integer));
    assert!(
        source
            .functions
            .iter()
            .filter(|f| f.origin == RoutineOrigin::EnumConstructor)
            .all(|f| f.type_params.is_empty())
    );
}
#[test]
fn expected_return_contract_supplies_arguments_absent_from_payload() {
    let graph = specialize(
        "enum Outcome<T, E> { Good: T, Bad: E }\nfn fail() -> Outcome<i64, string> { return Outcome::Bad(\"error\") }\nlet value = fail()\n",
    );
    let source = &graph.modules[graph.entry_index()].source;
    let constructor = source
        .functions
        .iter()
        .find(|f| f.origin == RoutineOrigin::EnumConstructor)
        .unwrap();
    let Type::Enum(contract) = &constructor.result else {
        panic!()
    };
    assert_eq!(
        contract.identity.arguments,
        vec![Type::Integer, Type::String]
    );
    assert_eq!(constructor.params[0].ty, Type::String);
}
#[test]
fn enum_instances_and_constructors_stay_in_the_defining_module() {
    let graph = specialize(
        "mod values { pub enum Maybe<T> { Some: T, None } }\nuse values::Maybe as M\nlet some = M::Some(3)\nlet none: M<i64> = M::None\n",
    );
    let child = graph
        .modules
        .iter()
        .find(|m| m.path.as_ref() == "crate::values")
        .unwrap();
    assert_eq!(child.source.enums.len(), 1);
    assert_eq!(child.source.functions.len(), 2);
    assert!(graph.modules[graph.entry_index()].source.enums.is_empty());
}

#[test]
fn unused_generic_enums_are_checked_without_forcing_concrete_instances() {
    for source in [
        "enum Bad<T> { Next: Bad<T> }\n",
        "record Boxed<T> { value: T }\nenum Bad<T> { Next: Boxed<Bad<T>> }\n",
        "record Boxed<T> { value: T }\nenum Bad<T> { Next: gc Boxed<T,T> }\n",
    ] {
        let mut graph = resolve(source);
        let native = graph.native.clone();
        let error = graph
            .with_sources(|asts| {
                cyallo_ir::generics::specialize_modules(
                    asts,
                    &native,
                    cyallo_ir::CompileOptions::default(),
                )
            })
            .unwrap_err();
        assert_eq!(error.code, "E184");
    }
    specialize(
        "enum Wrap<T> { Value: T }\nenum Finite { Value: Wrap<Wrap<i64>> }\nenum Recursive<T> { Empty, Next: gc Recursive<T> }\n",
    );
}
