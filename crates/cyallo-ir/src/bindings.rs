//! Implementation bindings for a core package, supplied by its provider.
//!
//! The SDK source declares signatures and carries no annotations. The provider that
//! implements the SDK says which engine operation or representation each bodyless
//! declaration is, by SDK item path. The linker applies the table to the provider's
//! own source before resolution; user source never receives bindings.
//!
//! An item path is the declaring module path, then the item name, then (for methods)
//! the method name: `std::cell::Cell`, `std::cell::Cell::new`. Moving or renaming a
//! declaration changes its path, so its binding changes with it; a binding whose path
//! no longer exists is a typed link error rather than silently ignored.
use std::collections::BTreeMap;
/// Maps SDK item paths to the engine's private semantic keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeBindings {
    entries: BTreeMap<Box<str>, Box<str>>,
}
impl NativeBindings {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn bind(mut self, path: &str, key: &str) -> Self {
        self.entries.insert(path.into(), key.into());
        self
    }
    /// Rebuild the table entry by entry: a provider adapting its source (moving or
    /// renaming declarations, swapping a key) edits the table the same way. Returning
    /// `None` drops the binding, leaving the declaration without an implementation.
    pub fn map(self, f: impl Fn(&str, &str) -> Option<(String, String)>) -> Self {
        self.iter()
            .filter_map(|(path, key)| f(path, key))
            .fold(Self::new(), |table, (path, key)| table.bind(&path, &key))
    }
    pub fn get(&self, path: &str) -> Option<&str> {
        self.entries.get(path).map(|key| key.as_ref())
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(p, k)| (p.as_ref(), k.as_ref()))
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Parse `path key` lines; blank lines and `#` comments are ignored. A path bound
    /// twice to different keys is a mistake in the table and panics at load, since
    /// the embedded table is fixed at build time.
    pub fn parse(table: &str) -> Self {
        let mut bindings = Self::new();
        for line in table.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Paths may contain spaces (`std::gc::gc T::clone`); the key is the last word.
            let (path, key) = line.rsplit_once(' ').expect("binding line is `path key`");
            let path = path.trim();
            if let Some(previous) = bindings.get(path) {
                assert_eq!(previous, key, "conflicting bindings for {path}");
            }
            bindings = bindings.bind(path, key);
        }
        bindings
    }
    /// The embedded standard package's bindings: every native declaration of `std`.
    /// The table lives in `bindings/std.bindings`, next to the provider, not in source.
    pub fn embedded() -> Self {
        Self::parse(include_str!("../bindings/std.bindings"))
    }
}
