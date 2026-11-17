use cyallo::{
    CompileOptions, Context, Session, Status,
    core_package::{CorePackage, SourceText},
    import_policy::ImportPolicy,
};
use cyallo_ir::bindings::NativeBindings;
use std::sync::Arc;

fn sources() -> Vec<(String, String)> {
    CorePackage::embedded()
        .sources()
        .iter()
        .map(|s| (s.path.into(), s.source.into()))
        .collect()
}
fn package<'a>(sources: &'a [(String, String)], bindings: NativeBindings) -> CorePackage<'a> {
    CorePackage::from_trusted_sources(
        "std.ya",
        sources.iter().map(|(path, source)| SourceText { path, source }),
    )
    .unwrap()
    .with_bindings(bindings)
}
fn compile(source: &str, core: &CorePackage<'_>) -> Result<cyallo::ir::Program, cyallo::Diagnostic> {
    cyallo::compile_with_core_package(source, CompileOptions::default(), &ImportPolicy::default(), core)
}
fn output(program: cyallo::ir::Program) -> String {
    let mut session = Session::with_context(Arc::new(program), Context::default()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        session.run(3);
        match session.status() {
            Status::Paused => {}
            Status::Waiting => session.wait_for_wakeup(std::time::Duration::from_millis(5)),
            _ => break,
        }
        assert!(std::time::Instant::now() < deadline, "execution timed out");
    }
    assert_eq!(session.status(), &Status::Finished);
    session.context.output.clone()
}
const CELL_PROGRAM: &str = "let cell = std::cell::Cell::new(1)\nstd::io::print(cell.replace(5), cell.replace(9))\n";
/// The embedded table with every path rewritten, as a provider would after moving
/// or renaming declarations in its source.
fn rename_paths(f: impl Fn(&str) -> String) -> NativeBindings {
    NativeBindings::embedded()
        .iter()
        .fold(NativeBindings::new(), |table, (path, key)| table.bind(&f(path), key))
}
fn edit_cell(f: impl Fn(&str) -> String) -> Vec<(String, String)> {
    let mut all = sources();
    let cell = all.iter_mut().find(|(path, _)| path == "std/cell.ya").unwrap();
    cell.1 = f(&cell.1);
    all
}
#[test]
fn the_cell_family_is_bound_by_the_provider_not_by_source_annotations() {
    let all = sources();
    let cell = &all.iter().find(|(path, _)| path == "std/cell.ya").unwrap().1;
    assert!(!cell.contains("cyallo_internal"), "{cell}");
    let program = compile(CELL_PROGRAM, &package(&all, NativeBindings::embedded())).unwrap();
    assert_eq!(output(program), "15\n");
}
#[test]
fn without_bindings_a_bodyless_declaration_has_no_implementation() {
    let all = sources();
    let error = compile(CELL_PROGRAM, &package(&all, NativeBindings::new())).err().unwrap();
    assert!(
        error.message.contains("source callable requires an implementation body")
            || error.message.contains("incompatible with its native ABI"),
        "{}",
        error.message
    );
}
#[test]
fn renaming_a_declaration_moves_its_binding_with_the_provider_table() {
    let mut renamed = edit_cell(|s| s.replace("Cell", "Slot"));
    let prelude = renamed.iter_mut().find(|(path, _)| path == "std/prelude.ya").unwrap();
    prelude.1 = prelude.1.replace("std::cell::Cell", "std::cell::Slot");
    // The old path no longer exists: the stale binding is a typed link error.
    let error = compile("std::io::print(1)", &package(&renamed, NativeBindings::embedded())).err().unwrap();
    assert!(error.message.contains("binds `std::cell::Cell`"), "{}", error.message);
    // Updating the provider table (and nothing in the compiler) restores it.
    let table = rename_paths(|path| path.replace("std::cell::Cell", "std::cell::Slot"));
    let program = compile(
        "let cell = std::cell::Slot::new(1)\nstd::io::print(cell.replace(5), cell.replace(9))\n",
        &package(&renamed, table),
    )
    .unwrap();
    assert_eq!(output(program), "15\n");
}
#[test]
fn a_binding_and_a_source_annotation_for_the_same_declaration_conflict() {
    let annotated = edit_cell(|s| s.replace("pub record Cell<T> {}", "#[cyallo_internal(\"opaque:clone\")]\npub record Cell<T> {}"));
    let error = compile("std::io::print(1)", &package(&annotated, NativeBindings::embedded())).err().unwrap();
    assert!(error.message.contains("both a source annotation and a provider binding"), "{}", error.message);
}
#[test]
fn unknown_keys_and_wrong_kinds_are_rejected_like_source_annotations() {
    let all = sources();
    let table = NativeBindings::embedded().bind("std::cell::Cell::new", "no_such_key");
    let error = compile("std::io::print(1)", &package(&all, table)).err().unwrap();
    assert!(error.message.contains("unknown internal ABI key"), "{}", error.message);
    let table = NativeBindings::embedded().bind("std::cell::Cell::new", "option_struct");
    let error = compile("std::io::print(1)", &package(&all, table)).err().unwrap();
    assert!(error.message.contains("not valid on"), "{}", error.message);
}
#[test]
fn user_source_never_receives_provider_bindings() {
    // A binding for a path in the user's own package is never applied to it.
    let all = sources();
    let table = NativeBindings::embedded().bind("crate::forged", "cell_new");
    let error = compile("pub proc forged(value: i64) -> i64;\nstd::io::print(1)\n", &package(&all, table)).err().unwrap();
    assert!(error.message.contains("binds `crate::forged`"), "{}", error.message);
}

fn run_until_finished(source: &str, core: &CorePackage<'_>) -> String {
    output(compile(source, core).unwrap_or_else(|e| panic!("{}: {source}", e.message)))
}
const CLOCK_PROGRAM: &str = "let t = std::time::clock()\nlet u = t.clone()\nu.sleep(1 as u64)\nstd::io::print(\"slept\")\n";
#[test]
fn an_opaque_host_type_is_known_by_its_declaration_not_its_name() {
    // Renaming the declaration (source and provider table) changes nothing else:
    // the compiler never asked whether a type was called `Clock`.
    let mut renamed = sources();
    let time = renamed.iter_mut().find(|(path, _)| path == "std/time.ya").unwrap();
    time.1 = time.1.replace("Clock", "Timer");
    let table = rename_paths(|path| path.replace("std::time::Clock", "std::time::Timer"));
    let core = package(&renamed, table);
    assert_eq!(run_until_finished(CLOCK_PROGRAM, &core), "slept\n");
    // The original name is gone from the standard package.
    let error = compile("let t: std::time::Clock = std::time::clock()", &core).err().unwrap();
    assert!(error.message.contains("unresolved"), "{}", error.message);
}
#[test]
fn a_user_type_with_the_same_name_is_a_different_type() {
    let all = sources();
    let core = package(&all, NativeBindings::embedded());
    assert_eq!(run_until_finished(CLOCK_PROGRAM, &core), "slept\n");
    let error = compile(
        "record Clock {}\nlet c: Clock = std::time::clock()\n",
        &core,
    )
    .err()
    .expect("a user `Clock` must not accept the host clock");
    assert!(error.message.contains("Clock"), "{}", error.message);
}
#[test]
fn opaque_traits_come_from_the_provider_binding() {
    let all = sources();
    // `std::mem::clone` requires a cloneable type; the provider's `clone` trait decides.
    let copy = "let t = std::time::clock()\nlet u = std::mem::clone(&t)\nstd::io::print(1)\n";
    compile(copy, &package(&all, NativeBindings::embedded())).unwrap();
    let no_clone = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::time::Clock" { "opaque:transfer,effectful" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(copy, &package(&all, no_clone)).is_err(), "a non-cloneable host type was cloned");
    // The lifecycle trait is read from the binding too: the real binding makes a
    // clock effectful, so a pure `fn` cannot create one.
    let source = "fn make() { let c = std::time::clock() }\nstd::io::print(1)\n";
    assert!(compile(source, &package(&all, NativeBindings::embedded())).is_err());
    // Malformed trait flags are an unknown key.
    let bad = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::time::Clock" { "opaque:telepathy" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    let error = compile("std::io::print(1)", &package(&all, bad)).err().unwrap();
    assert!(error.message.contains("unknown internal ABI key"), "{}", error.message);
}

const CHANNEL_PROGRAM: &str = "let endpoints = std::channel::unbounded<i64>()\nlet tx = endpoints.0.clone()\nlet rx = endpoints.1.clone()\ntx.send(7).unwrap()\nstd::io::print(rx.recv().unwrap())\n";
#[test]
fn channel_ends_are_opaque_generic_types_known_by_declaration() {
    let all = sources();
    assert_eq!(run_until_finished(CHANNEL_PROGRAM, &package(&all, NativeBindings::embedded())), "7\n");
    // Rename both ends in source and table: nothing else notices.
    let mut renamed = sources();
    let channel = renamed.iter_mut().find(|(path, _)| path == "std/channel.ya").unwrap();
    channel.1 = channel.1.replace("Sender", "Tx").replace("Receiver", "Rx");
    let table = rename_paths(|path| {
        path.replace("std::channel::Sender", "std::channel::Tx")
            .replace("std::channel::Receiver", "std::channel::Rx")
    });
    assert_eq!(run_until_finished(CHANNEL_PROGRAM, &package(&renamed, table)), "7\n");
    // A user type called `Sender` does not become a channel end.
    let error = compile(
        "record Sender<T> { value: T }\nlet s: Sender<i64> = std::channel::unbounded<i64>().0\n",
        &package(&all, NativeBindings::embedded()),
    )
    .err()
    .expect("a user Sender must not accept the host sender");
    assert!(error.message.contains("Sender"), "{}", error.message);
}
#[test]
fn transferability_of_channel_ends_comes_from_the_declared_trait() {
    let all = sources();
    // A channel of a non-transferable element is rejected.
    let bad = "let endpoints = std::channel::unbounded<gc i64>()\n";
    let error = compile(bad, &package(&all, NativeBindings::embedded())).err().unwrap();
    assert!(!error.message.is_empty());
    // Dropping the provider's `transfer` flag makes the end itself non-transferable:
    // a channel of channel ends is then rejected.
    let nested = "let outer = std::channel::unbounded<std::channel::Sender<i64>>()\n";
    compile(nested, &package(&all, NativeBindings::embedded())).unwrap();
    let no_transfer = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::channel::Sender" { "opaque:effectful,clone" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(nested, &package(&all, no_transfer)).is_err());
}

const TASK_PROGRAM: &str = "let job = std::task::spawn(proc() -> i64 { return 41 + 1 })\nstd::io::print(job.join().completed())\n";
#[test]
fn task_handles_and_outcomes_are_opaque_types_known_by_declaration() {
    let all = sources();
    assert_eq!(run_until_finished(TASK_PROGRAM, &package(&all, NativeBindings::embedded())), "42\n");
    // Rename the handle and the outcome in source and table: nothing else notices.
    let mut renamed = sources();
    let task = renamed.iter_mut().find(|(path, _)| path == "std/task.ya").unwrap();
    task.1 = task.1.replace("TaskOutcome", "Completion").replace("Task", "Job");
    let table = rename_paths(|path| {
        path.replace("std::task::TaskOutcome", "std::task::Completion")
            .replace("std::task::Task", "std::task::Job")
    });
    assert_eq!(run_until_finished(TASK_PROGRAM, &package(&renamed, table)), "42\n");
    // `spawn` takes its handle type from its declaration, but the contract still
    // constrains the shape: a handle whose argument is not the body's result is rejected.
    let mut wrong = sources();
    let task = wrong.iter_mut().find(|(path, _)| path == "std/task.ya").unwrap();
    task.1 = task.1.replace("-> Task<T>;", "-> Task<string>;");
    let error = compile(TASK_PROGRAM, &package(&wrong, NativeBindings::embedded())).err().unwrap();
    assert!(error.message.contains("TaskSpawn"), "{}", error.message);
    // A declared result that is not an opaque host type at all is an invalid signature.
    let mut bool_result = sources();
    let task = bool_result.iter_mut().find(|(path, _)| path == "std/task.ya").unwrap();
    task.1 = task.1.replace("-> Task<T>;", "-> bool;");
    let error = compile(
        "let task = std::task::spawn(proc() -> i64 { return 1 })",
        &package(&bool_result, NativeBindings::embedded()),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, "E173", "{}", error.message);
}
#[test]
fn a_task_handle_is_a_unique_resource_by_declared_traits() {
    let all = sources();
    // A handle cannot be cloned or sent: the provider declares neither `clone` nor `transfer`.
    let cloned = "let job = std::task::spawn(proc() -> i64 { return 1 })\nlet copy = std::mem::clone(&job)\n";
    assert!(compile(cloned, &package(&all, NativeBindings::embedded())).is_err());
    let sent = "let ch = std::channel::unbounded<std::task::Task<i64>>()\n";
    assert!(compile(sent, &package(&all, NativeBindings::embedded())).is_err());
    // Granting the traits in the provider table is enough to allow both.
    let open = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::task::Task" { "opaque:effectful,resource,clone,transfer" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    compile(sent, &package(&all, open.clone())).unwrap();
    compile(cloned, &package(&all, open)).unwrap();
}

#[test]
fn recursive_types_through_an_opaque_type_depend_on_its_declared_indirection() {
    let all = sources();
    let node = "record Node { next: std::mem::manual::ArenaCell<Node> }\nstd::io::print(1)\n";
    // The provider declares `indirect`, so a recursive record may refer to itself through it.
    assert_eq!(run_until_finished(node, &package(&all, NativeBindings::embedded())), "1\n");
    // Without `indirect` the same declaration is an infinitely sized type.
    let direct = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::mem::manual::ArenaCell" { "opaque:effectful" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(node, &package(&all, direct)).is_err());
}

const GENERATOR_PROGRAM: &str = "let make = gen proc(count: i64) -> std::generator::Generator<i64, (), i64> {\n    let mut index = 0\n    while index < count {\n        yield index\n        index += 1\n    }\n    return count\n}\nlet mut values = make(2)\nstd::io::print(values.start().yielded())\nstd::io::print(values.resume().yielded())\nstd::io::print(values.resume().completed())\n";
#[test]
fn generators_are_recognized_by_the_declared_generator_trait_not_by_name() {
    let all = sources();
    assert_eq!(run_until_finished(GENERATOR_PROGRAM, &package(&all, NativeBindings::embedded())), "0\n1\n2\n");
    // Rename the type: the language only needs the provider's `generator` trait.
    let mut renamed = sources();
    for (_, source) in renamed.iter_mut() {
        *source = source
            .replace("std::generator::Generator", "std::generator::Coroutine")
            .replace("Generator<", "Coroutine<")
            .replace("pub record Generator", "pub record Coroutine")
            .replace("impl<Y, R, C> Generator", "impl<Y, R, C> Coroutine");
    }
    let table = rename_paths(|path| path.replace("std::generator::Generator", "std::generator::Coroutine"));
    let program = GENERATOR_PROGRAM.replace("Generator<", "Coroutine<");
    assert_eq!(run_until_finished(&program, &package(&renamed, table)), "0\n1\n2\n");
    // Without the trait the same declaration is an ordinary opaque type: it cannot be
    // the result of a generator procedure.
    let plain = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::generator::Generator" { "opaque:effectful,resource" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(GENERATOR_PROGRAM, &package(&all, plain)).is_err());
}

const WEAK_PROGRAM: &str = "let owner = rc 8\nlet weak = owner.downgrade()\nstd::io::print(weak.upgrade().is_some())\nproc named() -> std::rc::Weak<i64> { let o = rc 1; return o.downgrade() }\n";
#[test]
fn weak_references_are_recognized_by_the_declared_weak_trait_not_by_name() {
    let all = sources();
    assert_eq!(run_until_finished(WEAK_PROGRAM, &package(&all, NativeBindings::embedded())), "true\n");
    // Rename the type everywhere: only the `weak` trait matters to the language.
    let mut renamed = sources();
    for (_, source) in renamed.iter_mut() {
        *source = source
            .replace("std::rc::Weak;", "std::rc::Ref;")
            .replace("pub record Weak<T>", "pub record Ref<T>")
            .replace("impl<T> Weak<T>", "impl<T> Ref<T>");
    }
    let table = rename_paths(|path| match path.strip_prefix("std::rc::Weak") {
        Some(rest) if rest.is_empty() || rest.starts_with("::") => format!("std::rc::Ref{rest}"),
        _ => path.to_string(),
    });
    let program = WEAK_PROGRAM.replace("std::rc::Weak", "std::rc::Ref");
    assert_eq!(run_until_finished(&program, &package(&renamed, table)), "true\n");
    // Without the trait `downgrade` has no weak type to return.
    let plain = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::rc::Weak" { "opaque:effectful,resource,clone,indirect" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(WEAK_PROGRAM, &package(&all, plain)).is_err());
}

const CELL_DEREF_PROGRAM: &str = "let cell = std::cell::Cell::new(1)\ncell.* = cell.* + 1\nstd::io::print(cell.*)\n";
#[test]
fn a_cell_dereferences_because_its_declaration_says_so() {
    let all = sources();
    assert_eq!(run_until_finished(CELL_DEREF_PROGRAM, &package(&all, NativeBindings::embedded())), "2\n");
    // Without the `deref` trait the same declaration is an opaque type that cannot be `.*`'d,
    // and `Cell::new` no longer accepts it as the result of a constructor.
    let plain = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::cell::Cell" { "opaque:clone" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(CELL_DEREF_PROGRAM, &package(&all, plain)).is_err());
}
const PATH_PROGRAM: &str = "record Bag { items: Vec<i64> }\nproc locate(bag: gc Bag, index: i64) -> std::lookup::StoragePath<i64> {\n    return std::lookup::path(bag).target().field(\"items\").element(index)\n}\nlet bag = gc Bag { items: Vec[4, 5] }\nstd::io::print(std::lookup::mapped(bag, 1, locate).unwrap())\n";
#[test]
fn storage_paths_and_locations_are_recognized_by_declared_traits() {
    let all = sources();
    // Renamed in source and table, with the exact path entries moved.
    let mut renamed = sources();
    let lookup = renamed.iter_mut().find(|(path, _)| path == "std/lookup.ya").unwrap();
    lookup.1 = lookup.1.replace("StoragePath", "Route");
    let prelude = renamed.iter_mut().find(|(path, _)| path == "std/prelude.ya").unwrap();
    prelude.1 = prelude.1.replace("StoragePath", "Route");
    let table = rename_paths(|path| match path.strip_prefix("std::lookup::StoragePath") {
        Some(rest) if rest.is_empty() || rest.starts_with("::") => format!("std::lookup::Route{rest}"),
        _ => path.to_string(),
    });
    let program = PATH_PROGRAM.replace("StoragePath", "Route");
    let first = compile(PATH_PROGRAM, &package(&all, NativeBindings::embedded()));
    let second = compile(&program, &package(&renamed, table));
    // Both compile or both fail the same way: the name changed, the semantics did not.
    assert_eq!(first.is_ok(), second.is_ok(), "{:?} vs {:?}", first.err().map(|e| e.message), second.err().map(|e| e.message));
    // Without the `path` trait a mapper cannot return the type.
    let plain = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::lookup::StoragePath" { "opaque:clone,indirect,target" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(first.is_ok(), "the path program must compile with the real binding");
    assert!(compile(PATH_PROGRAM, &package(&all, plain)).is_err());
}

const ALLOCATOR_PROGRAM: &str = r#"
use std::mem::{Allocator, Layout, Allocation, AllocError}
record Counting { allocated: gc Cell<i64>, released: gc Cell<i64> }
impl Allocator for Counting {
    proc allocate(self: &mut Self, layout: Layout) -> Result<Allocation, AllocError> {
        self.allocated.*.* = self.allocated.*.* + 1
        return std::mem::allocate(layout)
    }
    proc deallocate(self: &mut Self, allocation: Allocation) {
        self.released.*.* = self.released.*.* + 1
        std::mem::deallocate(allocation)
    }
}
let allocated = gc Cell::new(0)
let released = gc Cell::new(0)
let owner = rc(Counting { allocated: allocated, released: released }) 3
std::mem::drop(owner)
std::io::print(allocated.*.*, released.*.*, std::mem::Layout::new(8 as usize, 4 as usize).unwrap().size())
"#;
#[test]
fn the_allocator_protocol_is_stated_by_declared_roles_not_names() {
    let all = sources();
    assert_eq!(run_until_finished(ALLOCATOR_PROGRAM, &package(&all, NativeBindings::embedded())), "118\n");
    // Rename both types in source and table: the protocol only needs the roles.
    let mut renamed = sources();
    for (_, source) in renamed.iter_mut() {
        *source = source.replace("Layout", "Shape").replace("Allocation", "Block");
    }
    let table = rename_paths(|path| path.replace("std::mem::Layout", "std::mem::Shape").replace("std::mem::Allocation", "std::mem::Block"));
    let program = ALLOCATOR_PROGRAM.replace("Layout", "Shape").replace("Allocation", "Block");
    assert_eq!(run_until_finished(&program, &package(&renamed, table)), "118\n");
    // Without the `layout` role an allocator's `allocate` is not recognized.
    let no_layout = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::mem::Layout" { "opaque:copy,clone" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile(ALLOCATOR_PROGRAM, &package(&all, no_layout)).is_err());
    // A layout is copyable by its declared trait: using it twice is allowed.
    let twice = "let layout = std::mem::Layout::new(8 as usize, 8 as usize).unwrap()\nstd::io::print(layout.size(), layout.align())\n";
    assert_eq!(run_until_finished(twice, &package(&all, NativeBindings::embedded())), "88\n");
}

const COLLECTION_PROGRAM: &str = "let mut v = Vec[4, 5]\nv.push(6)\nlet mut d = VecDeque[1, 2]\nd.push_front(0)\nlet r = std::collections::Vec::from(0..3)\nstd::io::print(v[2], v.len(), d[0], d.len(), r.len())\n";
#[test]
fn collections_are_recognized_by_declared_traits_not_by_name() {
    let all = sources();
    assert_eq!(run_until_finished(COLLECTION_PROGRAM, &package(&all, NativeBindings::embedded())), "63033\n");
    // Rename both types in source and table; literals, indexing, push and from all follow.
    let mut renamed = sources();
    for (_, source) in renamed.iter_mut() {
        *source = source
            .replace("{Vec, VecDeque}", "{Seq, Ring}")
            .replace("std::collections::VecDeque", "std::collections::Ring")
            .replace("std::collections::Vec", "std::collections::Seq")
            .replace("VecDeque", "Ring")
            .replace("Vec<", "Seq<")
            .replace("record Vec", "record Seq")
            .replace("impl<T> Vec", "impl<T> Seq");
    }
    let table = rename_paths(|path| {
        if let Some(rest) = path.strip_prefix("std::collections::VecDeque") {
            format!("std::collections::Ring{rest}")
        } else if let Some(rest) = path.strip_prefix("std::collections::Vec") {
            format!("std::collections::Seq{rest}")
        } else {
            path.to_string()
        }
    });
    let program = COLLECTION_PROGRAM
        .replace("VecDeque[", "Ring[")
        .replace("Vec[", "Seq[")
        .replace("std::collections::Vec", "std::collections::Seq");
    let core = package(&renamed, table);
    let output = run_until_finished(&format!("use std::collections::{{Seq, Ring}}\n{program}"), &core);
    assert_eq!(output, "63033\n");
}
#[test]
fn only_a_double_ended_collection_grows_at_the_front() {
    let all = sources();
    let front = "let mut v = Vec[4, 5]\nv.push_front(3)\n";
    assert!(compile(front, &package(&all, NativeBindings::embedded())).is_err());
    // Without `collection` the literal head is not a collection at all.
    let plain = NativeBindings::embedded().map(|path, key| {
        let key = if path == "std::collections::Vec" { "opaque:clone,transfer,indirect" } else { key };
        Some((path.to_string(), key.to_string()))
    });
    assert!(compile("let v = Vec[1]\n", &package(&all, plain)).is_err());
}
