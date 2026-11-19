//! Run-time module loading through the verified interpreter. Expected lines
//! are written by hand from docs/dynamic-import.md: `load` returns a namespace
//! object or a failure value, exports read and call like properties, a view
//! requires the exports it names, and loading the same module again gives the
//! same namespace.

use cyallo::{
    bytecode::verified::{ExecutionLimits, ExecutionStatus, OutputSink, VerifiedModule},
    compile,
    module_loader::{ModuleLoader, ReadGrant},
};
use std::sync::{Arc, Mutex};

fn run(source: &str, stress: bool) -> Vec<String> {
    let loader = ModuleLoader::new(
        ReadGrant::directory(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap(),
    );
    let program = Arc::new(
        compile(source)
            .unwrap_or_else(|error| panic!("{error:?}\n{source}"))
            .with_module_loader(loader),
    );
    let module = VerifiedModule::lower_shared(program, 0).unwrap();
    let limits = ExecutionLimits {
        fuel: 20_000_000,
        frames: 64,
    };
    let mut activation = module.start(&[], limits).unwrap();
    activation.set_gc_stress(stress);
    let lines = Arc::new(Mutex::new(Vec::new()));
    {
        let lines = lines.clone();
        activation.set_output(OutputSink::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    }
    let status = activation.resume(usize::MAX);
    assert!(
        matches!(status, ExecutionStatus::Returned(None)),
        "{status:?}"
    );
    lines.lock().unwrap().clone()
}

const PLUGIN: &str = "examples/modules/dynamic/plugin.ya";

#[test]
fn exports_read_and_call_like_properties() {
    let source = format!(
        r#"
let module = std::module::load("{PLUGIN}").unwrap()
std::io::print(module.label)
let name = "twice"
std::io::print(module[name](21))
"#
    );
    for stress in [false, true] {
        assert_eq!(run(&source, stress), ["runtime", "42"]);
    }
}

#[test]
fn a_view_requires_the_exports_it_names_and_reports_what_is_missing() {
    let source = format!(
        r#"
type Needs = object {{ label: string, twice: fn(i64) -> i64, .. }}
type Absent = object {{ nothing: string, .. }}
let module = std::module::load("{PLUGIN}").unwrap()
let view = std::object::view<Needs>(module).unwrap()
std::io::print(view.label, " ", view.twice(4))
let bad = std::object::view<Absent>(module)
std::io::print(bad.is_err())
"#
    );
    assert_eq!(run(&source, true), ["runtime 8", "true"]);
}

#[test]
fn a_missing_module_is_a_failure_value_with_its_kind() {
    let source = r#"
let missing = std::module::load("examples/modules/dynamic/absent.ya")
let error = missing.unwrap_err()
std::io::print(error.is_not_found(), " ", error.is_denied())
"#;
    assert_eq!(run(source, false), ["true false"]);
}

#[test]
fn loading_the_same_module_again_gives_the_same_namespace() {
    let source = format!(
        r#"
let first = std::module::load("{PLUGIN}").unwrap()
let second = std::module::load("{PLUGIN}").unwrap()
std::io::print(first == second)
std::io::print(second.label)
"#
    );
    assert_eq!(run(&source, true), ["true", "runtime"]);
}

#[test]
fn procedures_of_a_loaded_module_run_in_the_calling_task() {
    let source = format!(
        r#"
type Plugin = object {{ update: proc(gc Cell<i64>) -> i64, .. }}
let module = std::module::load("{PLUGIN}").unwrap()
let plugin = std::object::view<Plugin>(module).unwrap()
let state = gc Cell::new(40)
std::io::print(plugin.update(state.clone()))
std::io::print(state.*.*)
"#
    );
    assert_eq!(run(&source, true), ["42", "42"]);
}
