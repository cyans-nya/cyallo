//! Interface digests and version rules for SDK modules.
//!
//! An item digest covers one declaration's canonical form: its path inside the
//! package, kind, signature, referenced type identities and `#[since]` version.
//! Comments, formatting, declaration order and parameter names do not affect it.
//! Opaque types digest as their identity and generic arity only.
use cyallo_ast::{DeclarationAnnotation, Span};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}
impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let (major, minor) = text.split_once('.')?;
        let number = |text: &str| {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse().ok())
                .flatten()
        };
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
        })
    }
}
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);
impl Digest {
    pub fn of(text: &str) -> Self {
        Self(Sha256::digest(text.as_bytes()).into())
    }
}
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}
impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Function,
    Record,
    OpaqueRecord,
    Enum,
    Trait,
    Alias,
    Method,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceItem {
    /// Path inside the package, for example `api::Context::data_dir`.
    pub path: Box<str>,
    pub kind: ItemKind,
    pub since: Option<Version>,
    pub canonical: Box<str>,
    pub digest: Digest,
    /// The item has no implementation in the SDK (a bodyless routine or an opaque
    /// record): an implementing component must bind it. Not part of the digest.
    pub requires_binding: bool,
    /// Parameter count of a routine or method. Not part of the digest.
    pub arity: Option<usize>,
}
impl InterfaceItem {
    pub fn bindable(mut self, requires_binding: bool, arity: Option<usize>) -> Self {
        self.requires_binding = requires_binding;
        self.arity = arity;
        self
    }
    pub fn new(path: &str, kind: ItemKind, since: Option<Version>, canonical: String) -> Self {
        let text = format!(
            "{kind:?} {path} since={} {canonical}",
            since.map_or("-".into(), |v| v.to_string())
        );
        Self {
            path: path.into(),
            kind,
            since,
            digest: Digest::of(&text),
            canonical: canonical.into(),
            requires_binding: false,
            arity: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldDigest {
    pub path: Box<str>,
    pub digest: Digest,
}
/// Everything an SDK package publishes, sorted by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterfaceDigests {
    pub items: Vec<InterfaceItem>,
    pub worlds: Vec<WorldDigest>,
}
impl InterfaceDigests {
    pub fn sorted(mut self) -> Self {
        self.items.sort_by(|a, b| a.path.cmp(&b.path));
        self.worlds.sort_by(|a, b| a.path.cmp(&b.path));
        self
    }
    pub fn item(&self, path: &str) -> Option<&InterfaceItem> {
        self.items.iter().find(|item| item.path.as_ref() == path)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkError {
    InvalidSince { text: Box<str> },
    ItemRemoved { path: Box<str> },
    WorldRemoved { path: Box<str> },
    ItemChanged { path: Box<str>, previous: Digest, current: Digest },
    NewItemWithoutSince { path: Box<str>, version: Version },
    NewItemSinceMismatch { path: Box<str>, since: Version, version: Version },
    ItemFromFuture { path: Box<str>, since: Version, version: Version },
}
impl fmt::Display for SdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSince { text } => {
                write!(f, "`#[since(\"{text}\")]` must be a `major.minor` version")
            }
            Self::ItemRemoved { path } => write!(f, "`{path}` was removed in a compatible release"),
            Self::WorldRemoved { path } => {
                write!(f, "world `{path}` was removed in a compatible release")
            }
            Self::ItemChanged { path, previous, current } => write!(
                f,
                "`{path}` changed in a compatible release ({previous} became {current})"
            ),
            Self::NewItemWithoutSince { path, version } => {
                write!(f, "new item `{path}` needs `#[since(\"{version}\")]`")
            }
            Self::NewItemSinceMismatch { path, since, version } => write!(
                f,
                "new item `{path}` is marked since {since}, but this release is {version}"
            ),
            Self::ItemFromFuture { path, since, version } => write!(
                f,
                "`{path}` is marked since {since}, which is newer than release {version}"
            ),
        }
    }
}
impl std::error::Error for SdkError {}
pub fn sdk_diagnostic(error: SdkError, span: Span) -> cyallo_ast::Diagnostic {
    cyallo_ast::Diagnostic::new("E175", error.to_string(), span)
}
/// The `#[since]` version of a declaration, when it has one.
pub fn since(annotations: &[DeclarationAnnotation]) -> Result<Option<Version>, SdkError> {
    annotations
        .iter()
        .find(|a| a.name.as_ref() == "since")
        .map(|a| Version::parse(&a.value).ok_or_else(|| SdkError::InvalidSince { text: a.value.clone() }))
        .transpose()
}
/// A compatible (minor) release keeps every earlier item byte-for-byte, adds only
/// items marked with its own version, and removes nothing.
pub fn check_minor_update(
    previous: &InterfaceDigests,
    next: &InterfaceDigests,
    version: Version,
) -> Result<(), Vec<SdkError>> {
    let mut errors = vec![];
    let before: BTreeMap<_, _> = previous.items.iter().map(|i| (&i.path, i)).collect();
    let after: BTreeMap<_, _> = next.items.iter().map(|i| (&i.path, i)).collect();
    for (path, old) in &before {
        match after.get(path) {
            None => errors.push(SdkError::ItemRemoved { path: (*path).clone() }),
            Some(new) if new.digest != old.digest => errors.push(SdkError::ItemChanged {
                path: (*path).clone(),
                previous: old.digest,
                current: new.digest,
            }),
            Some(_) => {}
        }
    }
    for (path, new) in &after {
        if let Some(since) = new.since
            && since > version
        {
            errors.push(SdkError::ItemFromFuture { path: (*path).clone(), since, version });
        }
        if before.contains_key(path) {
            continue;
        }
        match new.since {
            None => errors.push(SdkError::NewItemWithoutSince { path: (*path).clone(), version }),
            Some(since) if since != version => errors.push(SdkError::NewItemSinceMismatch {
                path: (*path).clone(),
                since,
                version,
            }),
            Some(_) => {}
        }
    }
    for old in &previous.worlds {
        if !next.worlds.iter().any(|w| w.path == old.path) {
            errors.push(SdkError::WorldRemoved { path: old.path.clone() });
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item(path: &str, since: Option<Version>, text: &str) -> InterfaceItem {
        InterfaceItem::new(path, ItemKind::Function, since, text.into())
    }
    fn digests(items: Vec<InterfaceItem>) -> InterfaceDigests {
        InterfaceDigests { items, worlds: vec![] }.sorted()
    }
    #[test]
    fn versions_parse_strictly_and_order_numerically() {
        assert_eq!(Version::parse("1.10"), Some(Version { major: 1, minor: 10 }));
        assert!(Version::parse("1.2") < Version::parse("1.10"));
        for bad in ["1", "1.", ".2", "1.2.3", "a.b", "-1.0", "1.+2"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }
    #[test]
    fn digest_depends_on_path_kind_since_and_signature_only() {
        let base = item("api::f", None, "fn()->i64");
        assert_eq!(base.digest, item("api::f", None, "fn()->i64").digest);
        assert_ne!(base.digest, item("api::g", None, "fn()->i64").digest);
        assert_ne!(base.digest, item("api::f", None, "fn()->bool").digest);
        assert_ne!(base.digest, item("api::f", Version::parse("1.1"), "fn()->i64").digest);
    }
    #[test]
    fn minor_updates_allow_only_new_marked_items() {
        let v10 = Version::parse("1.0").unwrap();
        let v11 = Version::parse("1.1").unwrap();
        let old = digests(vec![item("a", None, "x"), item("b", None, "y")]);
        let good = digests(vec![
            item("a", None, "x"),
            item("b", None, "y"),
            item("c", Some(v11), "z"),
        ]);
        assert_eq!(check_minor_update(&old, &good, v11), Ok(()));
        let bad = digests(vec![
            item("a", None, "changed"),
            item("c", None, "z"),
            item("d", Some(v10), "z"),
            item("e", Some(Version::parse("1.2").unwrap()), "z"),
        ]);
        let errors = check_minor_update(&old, &bad, v11).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, SdkError::ItemChanged { path, .. } if path.as_ref() == "a")));
        assert!(errors.contains(&SdkError::ItemRemoved { path: "b".into() }));
        assert!(errors.contains(&SdkError::NewItemWithoutSince { path: "c".into(), version: v11 }));
        assert!(errors.contains(&SdkError::NewItemSinceMismatch { path: "d".into(), since: v10, version: v11 }));
        assert!(errors.iter().any(|e| matches!(e, SdkError::ItemFromFuture { path, .. } if path.as_ref() == "e")));
    }
}
