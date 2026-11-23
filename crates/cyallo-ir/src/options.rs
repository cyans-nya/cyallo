#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Dialect {
    #[default]
    Modern,
    Compatibility,
    Strict,
    Simplified,
}
/// How the entry source is run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntryMode {
    /// A `.yas` script, snippet or REPL entry: top-level statements are the
    /// program. `main` is never searched for or run.
    #[default]
    Script,
    /// A `.ya` file: the entry declares `proc main()`, and every file's top
    /// level holds only declarations, `const` and immutable `let`. A file with
    /// top-level statements is an error, never a script.
    Main,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileOptions {
    pub entry: EntryMode,
    pub dialect: Dialect,
    pub convenience_builtins: bool,
    /// Zero uses available cores; one forces serial compilation.
    pub jobs: usize,
}
