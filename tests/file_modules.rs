//! Files are modules: no `mod` line is needed, `d/mod.ya` gates directory
//! children, and a directory without `mod.ya` is an open namespace.
use cyallo::{
    Session, Status,
    modules::{Compiler, SourceModule},
};
use std::sync::Arc;

fn module(path: &str, source: &str) -> SourceModule {
    SourceModule::new(path, source)
}

fn run(sources: &[SourceModule]) -> Result<String, cyallo::Diagnostic> {
    let program = Compiler::default().compile(0, sources, Default::default(), true)?;
    let mut session = Session::new(Arc::new(program)).unwrap();
    session.run(100000);
    assert_eq!(session.status(), &Status::Finished);
    Ok(session.context.output)
}

#[test]
fn a_file_is_a_module_without_a_mod_line() {
    let sources = [
        module(
            "main.ya",
            "use crate::lib::answer\nstd::io::print(answer())\n",
        ),
        module("lib.ya", "pub fn answer() -> i64 { return 42 }\n"),
    ];
    assert_eq!(run(&sources).unwrap(), "42\n");
}

#[test]
fn private_items_of_a_file_module_stay_private() {
    let sources = [
        module(
            "main.ya",
            "use crate::lib::hidden\nstd::io::print(hidden())\n",
        ),
        module("lib.ya", "fn hidden() -> i64 { return 1 }\n"),
    ];
    assert!(run(&sources).is_err());
}

#[test]
fn a_directory_without_mod_is_an_open_namespace() {
    let sources = [
        module("main.ya", "use crate::d::b::f\nstd::io::print(f())\n"),
        module("d/b.ya", "pub fn f() -> i64 { return 7 }\n"),
    ];
    assert_eq!(run(&sources).unwrap(), "7\n");
}

#[test]
fn mod_ya_gates_its_children_until_it_re_exports_them() {
    let gated = |mod_source: &str| {
        [
            module("main.ya", "use crate::d::b::f\nstd::io::print(f())\n"),
            module("d/mod.ya", mod_source),
            module("d/b.ya", "pub fn f() -> i64 { return 9 }\n"),
        ]
    };
    assert!(run(&gated("")).is_err(), "child is private to d");
    assert_eq!(run(&gated("pub use b\n")).unwrap(), "9\n");
}

#[test]
fn explicit_mod_lines_keep_working() {
    let sources = [
        module(
            "main.ya",
            "mod lib;\nuse crate::lib::answer\nstd::io::print(answer())\n",
        ),
        module("lib.ya", "pub fn answer() -> i64 { return 3 }\n"),
    ];
    assert_eq!(run(&sources).unwrap(), "3\n");
}

fn failure(sources: &[SourceModule]) -> cyallo::Diagnostic {
    run(sources).expect_err("the layout must be rejected")
}

#[test]
fn a_file_beside_its_own_directory_is_a_hard_error() {
    // `d.ya` next to `d/b.ya` (implicit) and next to `d/mod.ya` are both ambiguous.
    for directory_file in ["d/b.ya", "d/mod.ya"] {
        let error = failure(&[
            module("main.ya", ""),
            module("d.ya", "pub const D = 1\n"),
            module(directory_file, "pub const B = 2\n"),
        ]);
        assert_eq!(error.code, "E140");
        assert!(error.message.contains("both `d.ya` and the directory `d/`"));
    }
    // An explicit `mod d;` does not make the layout unambiguous.
    let error = failure(&[
        module("main.ya", "mod d;\n"),
        module("d.ya", ""),
        module("d/b.ya", ""),
    ]);
    assert!(error.message.contains("both `d.ya` and the directory `d/`"));
}

#[test]
fn an_inline_module_and_a_file_of_the_same_name_are_a_hard_error() {
    for file in ["b.ya", "b/mod.ya"] {
        let error = failure(&[
            module("main.ya", "mod b {\n    pub const X = 1\n}\n"),
            module(file, "pub const Y = 2\n"),
        ]);
        assert_eq!(error.code, "E140");
        assert!(error.message.contains("declared inline and also defined"));
        // The error points at the inline declaration.
        assert_ne!(error.span, cyallo::Span::default());
    }
}

#[test]
fn directories_and_inline_modules_with_distinct_names_stay_legal() {
    // An inline module may load a child file from its own directory, as in Rust.
    let sources = [
        module(
            "main.ya",
            "mod group {\n    pub mod child;\n}\nuse crate::group::child::f\nstd::io::print(f())\n",
        ),
        module("group/child.ya", "pub fn f() -> i64 { return 6 }\n"),
    ];
    assert_eq!(run(&sources).unwrap(), "6\n");
}
