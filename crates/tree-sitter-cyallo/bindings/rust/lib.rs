//! Highlighting grammar; the Cyallo compiler remains the syntax authority.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_cyallo() -> *const ();
}
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_cyallo) };
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
