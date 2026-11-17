use cyallo_ast::*;
use cyallo_parser::{index_modern, parse};
use cyallo_compile_jobs::Jobs;

const SDK: &str = r#"
pub interface mod api {
    #[since("1.0")]
    pub opaque record Context;
    pub opaque record Handle<T>;
    pub record Error { message: string }
    pub proc log(message: string);
}
pub world Base {
    import std::io;
}
pub world Extension {
    include Base;
    import std::fs {
        fn read_to_string(read path: string) -> string;
    }
    import api as host_api;
    export proc activate(ctx: i64) -> i64;
}
impl interface host_sdk::api for crate::api_impl {
    type Context = crate::ExtensionContext;
    type Handle = i64;
}
"#;

#[test]
fn sdk_and_world_declarations_parse_with_identity_preserving_ast() {
    let ast = parse(SDK).unwrap();
    let api = &ast.modules[0];
    assert!(api.interface && api.public);
    let inner = api.body.as_ref().unwrap();
    assert_eq!(inner.records.len(), 3);
    assert!(inner.records[0].opaque && inner.records[1].opaque && !inner.records[2].opaque);
    assert_eq!(inner.records[0].annotations[0].name.as_ref(), "since");
    assert_eq!(inner.records[1].type_params, [Box::<str>::from("T")]);
    assert!(inner.records[0].fields.is_empty());
    assert!(inner.exports.iter().any(|e| e.as_ref() == "$type$Context"));

    assert_eq!(ast.worlds.len(), 2);
    let extension = &ast.worlds[1];
    assert_eq!(extension.name.as_ref(), "Extension");
    assert!(extension.public);
    assert!(matches!(&extension.items[0], WorldItem::Include { path, .. } if path.as_ref() == "Base"));
    let WorldItem::Import { path, members: Some(members), .. } = &extension.items[1] else {
        panic!("expected an import with a member list")
    };
    assert_eq!(path.as_ref(), "std::fs");
    assert_eq!(members[0].name.as_ref(), "read_to_string");
    assert!(
        matches!(&extension.items[2], WorldItem::Import { alias: Some(alias), members: None, .. } if alias.as_ref() == "host_api")
    );
    assert!(matches!(&extension.items[3], WorldItem::Export { function } if function.name.as_ref() == "activate"));

    let implementation = &ast.interface_implementations[0];
    assert_eq!(implementation.interface.as_ref(), "host_sdk::api");
    assert_eq!(implementation.module.as_ref(), "crate::api_impl");
    assert_eq!(implementation.bindings.len(), 2);
    assert!(ast.implementations.is_empty() && ast.inherent_implementations.is_empty());
}

#[test]
fn parallel_parsing_matches_the_sequential_parser() {
    let indexed = index_modern(SDK).unwrap();
    let sequential = parse(SDK).unwrap();
    for jobs in [Jobs(1), Jobs(4)] {
        assert_eq!(
            format!("{sequential:?}"),
            format!("{:?}", indexed.parse(false, jobs).unwrap())
        );
    }
}

#[test]
fn malformed_sdk_syntax_is_rejected() {
    for (source, expected) in [
        ("opaque record R { value: i64 }", "no fields"),
        ("world W { frobnicate x }", "expected include, import or export"),
        ("world W { export proc f() { return } }", "signature only"),
        ("world W { import a { fn f() -> i64 { return 1 } } }", "signatures only"),
        (
            "impl interface a::b for c { type T = i64\n type T = i64 }",
            "duplicate type binding",
        ),
    ] {
        let error = parse(source).unwrap_err();
        assert!(
            error.message.contains(expected),
            "{source}: {}",
            error.message
        );
    }
}

#[test]
fn contextual_words_remain_ordinary_identifiers() {
    let ast = parse("let world = 1\nlet interface = 2\nlet opaque = 3\n").unwrap();
    assert!(ast.worlds.is_empty() && ast.modules.is_empty() && ast.records.is_empty());
}
