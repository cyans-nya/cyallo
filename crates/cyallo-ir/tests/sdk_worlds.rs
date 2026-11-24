use cyallo_ir::namespace;
fn link(source: &str) -> Result<cyallo_ir::module_graph::ResolvedProgram, cyallo_ast::Diagnostic> {
    namespace::single(cyallo_parser::parse(source).unwrap())
}
fn message(source: &str) -> String {
    link(source).expect_err("source must be rejected").message
}
const MODULES: &str = "
pub mod io {
    pub proc print(text: string) {}
}
pub mod fs {
    pub fn read_to_string(path: string) -> string { return path }
    pub proc write(path: string, data: string) {}
    pub fn first<T>(values: [4]T) -> T { return values[0] }
}
pub interface mod api {
    pub opaque record Context;
}
";
fn with(world: &str) -> String {
    format!("{MODULES}\n{world}\n")
}
#[test]
fn includes_flatten_imports_and_exports_and_keep_origins() {
    let graph = link(&with(
        "
pub world Base { import io }
pub world Extension {
    include Base
    import fs {
        fn read_to_string(path: string) -> string;
        fn first<U>(values: [4]U) -> U;
    }
    import api as host_api
    export proc activate(ctx: i64) -> i64;
}
pub world Both {
    include Base
    include Extension
}
",
    ))
    .unwrap();
    let find = |name: &str| {
        graph
            .worlds
            .iter()
            .find(|w| w.path.ends_with(name))
            .unwrap()
    };
    let base = find("Base");
    let extension = find("Extension");
    let fields: Vec<_> = extension.imports.iter().map(|i| i.field.as_ref()).collect();
    assert_eq!(fields, ["io", "fs", "host_api"]);
    assert_eq!(extension.imports[0].origin, base.id);
    assert_eq!(extension.imports[1].origin, extension.id);
    assert_eq!(
        extension.imports[1].members.as_deref().unwrap(),
        [Box::<str>::from("read_to_string"), "first".into()]
    );
    assert!(extension.imports[0].members.is_none());
    assert_eq!(extension.includes, [base.id]);
    assert_eq!(extension.exports[0].name.as_ref(), "activate");
    // A diamond include does not duplicate what both paths share.
    let both = find("Both");
    assert_eq!(both.imports.len(), 3);
    assert_eq!(both.exports.len(), 1);
    assert_eq!(both.includes, [base.id, extension.id]);
}
#[test]
fn world_errors_are_typed_and_precise() {
    for (world, expected) in [
        ("world A { include Missing }", "unresolved world `Missing`"),
        (
            "world A { include B }\nworld B { include A }",
            "includes itself",
        ),
        ("world A { include A }", "includes itself"),
        ("world A { import nowhere }", "does not name a module"),
        (
            "world A { import io\n import io }",
            "two imports are named `io`",
        ),
        (
            "world A { import io\n import fs as io }",
            "two imports are named `io`",
        ),
        (
            "world A { import fs { fn nothing() -> i64; } }",
            "no accessible routine `nothing`",
        ),
        (
            "world A { import fs { fn read_to_string(path: i64) -> string; } }",
            "differs from the one in `fs`",
        ),
        (
            "world A { import fs { proc read_to_string(path: string) -> string; } }",
            "differs from the one in `fs`",
        ),
        (
            "world A { import fs { fn read_to_string(path: string) -> i64; } }",
            "differs from the one in `fs`",
        ),
        (
            "world A { export proc f() -> i64;\n export proc f() -> i64; }",
            "declared more than once",
        ),
        (
            "world A { export proc f(x: Unknown) -> i64; }",
            "unresolved name",
        ),
    ] {
        let message = link(&with(world))
            .err()
            .unwrap_or_else(|| panic!("{world} must be rejected"))
            .message;
        assert!(message.contains(expected), "{world}: {message}");
    }
}
#[test]
fn conflicting_imports_through_includes_are_rejected_but_aliases_resolve_them() {
    let conflict = with(
        "world A { import io }\nworld B { import fs as io }\nworld C { include A\n include B }",
    );
    assert!(message(&conflict).contains("two imports are named `io`"));
    let aliased = with(
        "world A { import io }\nworld B { import fs as files }\nworld C { include A\n include B }",
    );
    link(&aliased).unwrap();
}
#[test]
fn private_module_members_cannot_be_imported_by_a_world() {
    let source = "
mod hidden { fn secret() -> i64 { return 1 } }
world A { import hidden { fn secret() -> i64; } }
";
    assert!(message(source).contains("no accessible routine `secret`"));
}
