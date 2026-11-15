use cyallo::{
    CompileOptions, Context, Session, Status,
    backend::{
        CraneliftBackend, CraneliftSpeedBackend, Executable, NativeBackend, Tier, TieredBackend,
    },
};
use cyallo_shapes::DynamicError;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn engines(source: &str, check: impl Fn(&Session)) {
    for deferred in [false, true] {
        let program = Arc::new(
            if deferred {
                cyallo::compile_for_execution(source, CompileOptions::default())
            } else {
                cyallo::compile(source)
            }
            .unwrap(),
        );
        let tiered = TieredBackend::default().compile(program.clone()).unwrap();
        for index in 0..program.functions().len() {
            tiered.precompile(index, Tier::Winch).unwrap();
        }
        assert!(tiered.wait_for_compilation(Duration::from_secs(30)));
        let engines: Vec<Option<Box<dyn Executable>>> = vec![
            None,
            Some(Box::new(tiered)),
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
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(engine) = &engine {
                    engine.run(&mut session, 1).unwrap();
                } else {
                    session.run(1);
                }
                match session.status() {
                    Status::Paused => {}
                    Status::Waiting => session.wait_for_wakeup(Duration::from_millis(1)),
                    _ => break,
                }
                assert!(Instant::now() < deadline, "{:?}", session.status());
            }
            check(&session);
        }
    }
}
fn output(source: &str, expected: &str) {
    engines(source, |session| {
        assert_eq!(session.status(), &Status::Finished);
        assert_eq!(session.context.output, expected);
    });
}
fn failure(source: &str, expected: DynamicError) {
    engines(source, |session| {
        let Status::Failed(error) = session.status() else {
            panic!("{:?}", session.status())
        };
        assert_eq!(
            error.error,
            cyallo::runtime::RuntimeError::Dynamic(expected.clone())
        );
    });
}

#[test]
fn shape_changes_keep_exact_keys_identity_and_cached_slots() {
    output(
        r#"
let item=dynamic { score:1, Score:2 }
let alias=std::mem::clone(&item)
for i in 0..10 {
    alias["score"] += 1
    alias.extra=i
    std::io::print(item.score, ":", item.Score, ":", item.extra)
    delete alias.extra
}
item.score="changed"
std::io::print(alias.score, ":", item==alias)
"#,
        "2:2:0\n3:2:1\n4:2:2\n5:2:3\n6:2:4\n7:2:5\n8:2:6\n9:2:7\n10:2:8\n11:2:9\nchanged:true\n",
    );
    failure(
        "let item=dynamic { known:1 }; std::io::print(item.unknown)",
        DynamicError::MissingProperty("unknown".into()),
    );
}

#[test]
fn required_contracts_hold_through_erasure_and_all_aliases() {
    failure(
        "let item=object { name:\"Ada\" }; let raw:dynamic=item; raw.name=3",
        DynamicError::Type {
            expected: cyallo::Type::String,
            actual: cyallo::Type::Integer,
        },
    );
    failure(
        "let item=object { name:\"Ada\" }; let raw:dynamic=item; delete raw.name",
        DynamicError::RequiredProperty("name".into()),
    );
    failure(
        "let item=dynamic { name:\"Ada\" }; type Named=object {name:string,..}; let typed=std::object::view<Named>(item).unwrap(); delete item.name",
        DynamicError::RequiredProperty("name".into()),
    );
    failure(
        "let item=dynamic { value:1 }; let alias=std::mem::clone(&item); std::object::freeze(item); alias.value=2",
        DynamicError::Readonly,
    );
}

#[test]
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
fn dynamic_calls_and_short_circuiting_check_runtime_types() {
    output(
        r#"
let mut calls=0
proc test()->bool { calls+=1; return true }
let no:dynamic=false
let yes:dynamic=true
std::io::print(no && test(), ":", yes || test(), ":", calls)
let native:dynamic=std::num::abs
std::io::print(native(-42))
let text:dynamic="abc"
let array:dynamic=[10,20,30]
std::io::print(text[1], ":", array[2])
"#,
        "false:true:0\n42\nb:30\n",
    );
    failure(
        "fn id(x:i64)->i64 {return x}; let call:dynamic=id; call()",
        DynamicError::ArgumentCount {
            expected: 1,
            actual: 0,
        },
    );
    failure(
        "fn id(x:i64)->i64 {return x}; let call:dynamic=id; call(\"wrong\")",
        DynamicError::Type {
            expected: cyallo::Type::Integer,
            actual: cyallo::Type::String,
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
fn repl_objects_keep_specialized_callables_between_entries() {
    let mut repl = cyallo::repl::Repl::new(CompileOptions::default()).unwrap();
    for source in [
        "let bag=dynamic {callback:std::num::abs}",
        "std::io::print(bag.callback(-4))",
        "fn another()->i64 {return 0}",
        "std::io::print(bag.callback(-8))",
    ] {
        assert_eq!(
            repl.push_line(source).unwrap(),
            cyallo::repl::Submission::Ready
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(repl.session().status(), Status::Paused | Status::Waiting) {
            repl.run(1);
            if repl.session().status() == &Status::Waiting {
                repl.session().wait_for_wakeup(Duration::from_millis(1));
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(repl.session().status(), &Status::Finished);
    }
    assert_eq!(repl.session().context.output, "4\n8\n");
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
