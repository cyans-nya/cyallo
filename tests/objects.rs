use cyallo::{CompileOptions, Dialect, compile, compile_with_options, vm::{Context, Session, Status}};
use cyallo_shapes::DynamicError;
use std::sync::Arc;

fn failure(source: &str, expected: DynamicError) {
    let mut session = Session::new(Arc::new(compile(source).unwrap())).unwrap();
    while session.run(1000) == &Status::Paused {}
    let Status::Failed(error) = session.status() else {
        panic!("{:?}", session.status())
    };
    assert_eq!(error.error, cyallo::runtime::RuntimeError::Dynamic(expected));
}

#[test]
#[ignore = "verified engine gap; see docs/legacy-removal-plan.md"]
fn nested_views_are_checked_before_establishing_any_contract() {
    output(
        r#"
type Nested=object { child:object {name:string,..}, required:i64,.. }
let raw=dynamic { child:dynamic {name:"Ada"}, required:false }
std::io::print(std::object::view<Nested>(raw).is_err())
raw.child.name=12
std::io::print(raw.child.name)
let valid=dynamic { child:dynamic {name:"Ada"}, required:7, extra:true }
let typed=std::object::view<Nested>(valid).unwrap()
typed.child.name="Grace"
std::io::print(valid.child.name, ":", typed.extra)
"#,
        "true\n12\nGrace:true\n",
    );
    failure(
        "type Nested=object {child:object {name:string,..},..}; let raw=dynamic {child:dynamic {name:\"Ada\"}}; let typed=std::object::view<Nested>(raw).unwrap(); raw.child.name=1",
        DynamicError::Type {
            expected: cyallo::Type::String,
            actual: cyallo::Type::Integer,
        },
    );
}

#[test]
fn dynamic_storage_cannot_erase_borrows_or_cross_coroutines() {
    for source in [
        "let value=1; let raw:dynamic=&value",
        "let value=1; let raw=object {value:&value}",
        "let value=1; let raw:dynamic=proc[&value]() {std::io::print(value)}",
        "let raw=dynamic {value:1}; let task=std::task::spawn(proc(){std::io::print(raw.value)})",
        "let endpoints=std::channel::bounded<dynamic>(1 as usize)",
        "let raw=dynamic {value:1}; let address=&raw.value",
        "let item=object {value:1}; let borrowed=&item; borrowed.value=2",
        "let item=object {value:1}; let borrowed=&item; borrowed.value+=2",
        "const data=dynamic {value:1}",
        "proc store(value:dynamic) {}; let value=1; store(proc[&value](){std::io::print(value)})",
    ] {
        assert!(cyallo::compile(source).is_err(), "{source}");
    }
}

#[test]
fn structural_aliases_resolve_in_their_defining_module_and_reject_cycles() {
    output(
        "mod types {pub type Count=i64; pub type Counter=object {value:Count,..}}; let item:types::Counter=object {value:42}; std::io::print(item.value)",
        "42\n",
    );
    for source in [
        "type A=B; type B=A; let value:A=object {}",
        "type Bad=object {value:i64,value:string}; let value:Bad=object {value:1}",
    ] {
        assert!(cyallo::compile(source).is_err());
    }
    let mut source = String::new();
    for index in 0..260 {
        source.push_str(&format!("type Alias{index}=Alias{}\n", index + 1));
    }
    source.push_str("type Alias260=i64\nlet value:Alias0=1");
    assert_eq!(cyallo::compile(&source).unwrap_err().code, "E181");
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

