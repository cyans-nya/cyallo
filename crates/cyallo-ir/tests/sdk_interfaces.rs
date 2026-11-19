use cyallo_ir::namespace;
fn link(source: &str) -> Result<cyallo_ir::module_graph::ResolvedProgram, cyallo_ast::Diagnostic> {
    namespace::single(cyallo_parser::parse(source).unwrap())
}
fn message(source: &str) -> String {
    link(source).err().expect("source must be rejected").message
}
const SDK: &str = "
pub interface mod api {
    pub opaque record Context;
    pub opaque record Logger;
    pub proc log(message: string);
}
";
#[test]
fn bound_opaque_types_link_and_missing_or_double_bindings_are_rejected() {
    let ok = format!(
        "{SDK}
mod host {{
    pub record ContextImpl {{ name: string }}
    pub record LoggerImpl {{ level: i64 }}
}}
impl interface api for host {{
    type Context = host::ContextImpl
    type Logger = host::LoggerImpl
}}
"
    );
    link(&ok).unwrap();

    let missing = format!(
        "{SDK}
mod host {{ pub record ContextImpl {{ name: string }} }}
impl interface api for host {{ type Context = host::ContextImpl }}
"
    );
    assert!(message(&missing).contains("leave opaque type `Logger` unbound"));

    let double = format!(
        "{SDK}
mod host {{
 pub record A {{ n: i64 }}
 pub record B {{ n: i64 }}
}}
impl interface api for host {{ type Context = host::A\n type Logger = host::A }}
impl interface api for host {{ type Context = host::B }}
"
    );
    assert!(message(&double).contains("already bound"));
}
#[test]
fn an_interface_without_implementers_links_unbound() {
    link(SDK).unwrap();
}
#[test]
fn binding_errors_are_typed_and_precise() {
    for (extra, expected) in [
        (
            "impl interface nope for host { }",
            "unresolved interface module",
        ),
        (
            "impl interface host for host { }",
            "is not an `interface mod`",
        ),
        (
            "impl interface api for nowhere { }",
            "unresolved implementing module",
        ),
        (
            "impl interface api for host { type Missing = host::A }",
            "is not an opaque record",
        ),
        (
            "impl interface api for host { type Context = host::Unknown\n type Logger = host::A }",
            "unresolved name",
        ),
    ] {
        let source = format!("{SDK}\nmod host {{ pub record A {{ n: i64 }} }}\n{extra}\n");
        let message = message(&source);
        assert!(message.contains(expected), "{extra}: {message}");
    }
}
#[test]
fn generic_opaque_bindings_and_cyclic_bindings_are_rejected() {
    let generic = "
pub interface mod api { pub opaque record Handle<T>; }
mod host { pub record H { n: i64 } }
impl interface api for host { type Handle = host::H }
";
    assert!(message(generic).contains("generic opaque type `Handle`"));
    let cyclic = "
pub interface mod api { pub opaque record A; pub opaque record B; }
mod host {}
impl interface api for host { type A = api::B\n type B = api::A }
";
    let rejected = message(cyclic);
    assert!(rejected.to_lowercase().contains("cycl"), "{rejected}");
}
#[test]
fn opaque_records_cannot_be_constructed_with_literals() {
    let source = format!("{SDK}\nlet value = api::Context {{}}\n");
    assert!(message(&source).contains("opaque record cannot be constructed"));
}
