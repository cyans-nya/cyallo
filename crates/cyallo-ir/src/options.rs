#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Dialect {
    #[default]
    Modern,
    Compatibility,
    Strict,
    Simplified,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileOptions {
    pub dialect: Dialect,
    pub convenience_builtins: bool,
    /// Zero uses available cores; one forces serial compilation.
    pub jobs: usize,
}
