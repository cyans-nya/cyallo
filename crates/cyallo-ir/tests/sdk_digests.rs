use cyallo_ir::namespace;
use cyallo_ir::sdk::{self, SdkError, Version};
fn digests(source: &str) -> sdk::InterfaceDigests {
    namespace::single(cyallo_parser::parse(source).unwrap())
        .unwrap_or_else(|e| panic!("{e}\n{source}"))
        .interface
}
const V10: &str = r#"
pub interface mod api {
    pub opaque record Context;
    impl Context {
        pub fn data_dir(self: &Self) -> string;
        pub fn open(name: string) -> Context;
    }
    pub record Error { message: string, code: i64 }
    pub open enum Failure {
        Denied: string,
        Missing,
    }
    pub trait Instance {
        proc deactivate(self: Self)
    }
    pub proc log(message: string);
    fn private_helper() -> i64;
}
pub world Base {
    export proc activate(ctx: api::Context) -> i64;
}
"#;
#[test]
fn interface_items_cover_public_declarations_with_stable_paths() {
    let d = digests(V10);
    let paths: Vec<_> = d.items.iter().map(|i| i.path.as_ref()).collect();
    assert_eq!(
        paths,
        [
            "api::Context",
            "api::Context::data_dir",
            "api::Context::open",
            "api::Error",
            "api::Failure",
            "api::Instance",
            "api::log",
        ]
    );
    assert_eq!(d.worlds.len(), 1);
    assert_eq!(d.worlds[0].path.as_ref(), "Base");
    assert!(d.item("api::private_helper").is_none());
    let context = d.item("api::Context").unwrap();
    assert_eq!(context.kind, sdk::ItemKind::OpaqueRecord);
}
#[test]
fn digests_ignore_formatting_order_names_and_comments_but_not_meaning() {
    let base = digests(V10);
    let reshuffled = V10
        .replace("message: string)", "text: string)") // parameter name
        .replace(
            "pub proc log(text: string);",
            "pub proc log(text: string); // comment",
        )
        .replace(
            "pub record Error { message: string, code: i64 }",
            "pub record Error {\n  message: string,\n  code: i64\n}",
        );
    let same = digests(&reshuffled);
    assert_eq!(base.items, same.items);
    assert_eq!(base.worlds, same.worlds);
    let changed = digests(&V10.replace("code: i64", "code: bool"));
    assert_ne!(
        base.item("api::Error").unwrap().digest,
        changed.item("api::Error").unwrap().digest
    );
    assert_eq!(
        base.item("api::log").unwrap().digest,
        changed.item("api::log").unwrap().digest
    );
    let new_world = digests(&V10.replace("-> i64;\n}", "-> bool;\n}"));
    assert_ne!(base.worlds[0].digest, new_world.worlds[0].digest);
}
#[test]
fn type_identity_not_spelling_decides_the_digest() {
    let direct = digests(
        "pub mod shared { pub record Event { name: string } }\npub interface mod api { pub proc emit(e: crate::shared::Event); }\n",
    );
    let aliased = digests(
        "pub mod shared { pub record Event { name: string } }\npub interface mod api {\n use crate::shared::Event as Renamed\n pub proc emit(e: Renamed);\n}\n",
    );
    assert_eq!(
        direct.item("api::emit").unwrap().digest,
        aliased.item("api::emit").unwrap().digest
    );
    let other = digests(
        "pub mod shared { pub record Event { name: string } }\npub mod other { pub record Event { name: string } }\npub interface mod api { pub proc emit(e: crate::other::Event); }\n",
    );
    assert_ne!(
        direct.item("api::emit").unwrap().digest,
        other.item("api::emit").unwrap().digest
    );
}
#[test]
fn compatible_releases_are_checked_against_the_previous_artifact() {
    let v11 = Version::parse("1.1").unwrap();
    let previous = digests(V10);
    let next = digests(&V10.replace(
        "pub proc log(message: string);",
        "pub proc log(message: string);\n    #[since(\"1.1\")]\n    pub proc warn(message: string);",
    ));
    assert_eq!(sdk::check_minor_update(&previous, &next, v11), Ok(()));
    let unmarked = digests(&V10.replace(
        "pub proc log(message: string);",
        "pub proc log(message: string);\n    pub proc warn(message: string);",
    ));
    let errors = sdk::check_minor_update(&previous, &unmarked, v11).unwrap_err();
    assert_eq!(
        errors,
        [SdkError::NewItemWithoutSince {
            path: "api::warn".into(),
            version: v11
        }]
    );
    let removed = digests(&V10.replace("pub proc log(message: string);", ""));
    let errors = sdk::check_minor_update(&previous, &removed, v11).unwrap_err();
    assert_eq!(
        errors,
        [SdkError::ItemRemoved {
            path: "api::log".into()
        }]
    );
    let narrowed = digests(&V10.replace(
        "pub proc log(message: string);",
        "pub proc log(message: i64);",
    ));
    assert!(matches!(
        sdk::check_minor_update(&previous, &narrowed, v11).unwrap_err()[0],
        SdkError::ItemChanged { .. }
    ));
}
#[test]
fn malformed_since_versions_are_rejected() {
    let source =
        "pub interface mod api {\n #[since(\"one\")]\n pub proc log(message: string);\n}\n";
    let error = namespace::single(cyallo_parser::parse(source).unwrap())
        .err()
        .unwrap();
    assert!(
        error.message.contains("must be a `major.minor` version"),
        "{}",
        error.message
    );
}

#[test]
fn the_handshake_compares_only_locked_items_and_reports_all_differences() {
    let original = digests(V10);
    let error_type = original.item("api::Error").unwrap().digest;
    let log = original.item("api::log").unwrap().digest;
    let failure = original.item("api::Failure").unwrap().digest;
    let changed = digests(&V10.replace("code: i64", "code: bool"));
    assert_eq!(
        changed.check_used_items(&[("api::log".into(), log)].into()),
        Ok(())
    );

    let candidate = digests(
        &V10.replace("code: i64", "code: bool")
            .replace("    pub proc log(message: string);\n", ""),
    );
    let changed_error = candidate.item("api::Error").unwrap().digest;
    let locked = [
        ("api::log".into(), log),
        ("api::Failure".into(), failure),
        ("api::Error".into(), error_type),
    ]
    .into();
    assert_eq!(
        candidate.check_used_items(&locked).unwrap_err().differences,
        [
            sdk::ItemDifference {
                path: "api::Error".into(),
                expected: error_type,
                available: Some(changed_error)
            },
            sdk::ItemDifference {
                path: "api::log".into(),
                expected: log,
                available: None
            },
        ]
    );
}
