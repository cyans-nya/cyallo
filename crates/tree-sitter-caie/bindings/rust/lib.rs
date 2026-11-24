//! Highlighting grammar for the compiler-supported CAIE subset.
use tree_sitter_language::LanguageFn;
unsafe extern "C" {
    fn tree_sitter_caie() -> *const ();
}
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_caie) };
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
