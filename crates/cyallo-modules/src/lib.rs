//! Owned source loading and runtime load failures, independent of VM storage.
pub mod loader;
pub use cyallo_ast::{Diagnostic, Span};
pub use loader::{ModuleLoadError, ModuleLoader, ReadGrant};
use std::{fmt, path::PathBuf, sync::Arc};

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub path: String,
    pub source: String,
}
impl SourceModule {
    pub fn new(path: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: source.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    MissingCompilePlan,
    ChangedExistingRecord,
    ChangedExistingTrait,
    ChangedExistingImplementation,
    ChangedExistingKey,
    MissingExistingRoutine,
    ChangedExistingGlobal,
    InvalidInitializer,
    SourceArchiveOverflow,
    MissingModuleSource,
}
impl fmt::Display for PublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingCompilePlan => "program has no retained module compilation context",
            Self::ChangedExistingRecord => "publication would change an existing record layout",
            Self::ChangedExistingTrait => "publication would change an existing trait interface",
            Self::ChangedExistingImplementation => {
                "publication would change an existing trait implementation"
            }
            Self::ChangedExistingKey => "publication would change an existing service key",
            Self::MissingExistingRoutine => "publication would remove an existing routine",
            Self::ChangedExistingGlobal => "publication would change an existing global type",
            Self::SourceArchiveOverflow => "module source archive exceeds the supported span space",
            Self::MissingModuleSource => "loaded module tree is missing a declared source",
            Self::InvalidInitializer => "module initializer cannot be published safely",
        })
    }
}
impl std::error::Error for PublicationError {}

#[derive(Debug, Clone)]
pub enum LoadError {
    NoReadGrant,
    AmbiguousExport {
        path: PathBuf,
        name: Box<str>,
    },
    Cancelled,
    Load(Arc<ModuleLoadError>),
    Check {
        path: PathBuf,
        diagnostic: Diagnostic,
    },
    Publication(PublicationError),
    Host(cyallo_host::HostError),
}
impl LoadError {
    pub fn is_not_found(&self) -> bool {
        match self {
            Self::Load(error) => match error.as_ref() {
                ModuleLoadError::MissingSource { .. } => true,
                ModuleLoadError::Io { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
                _ => false,
            },
            _ => false,
        }
    }
    pub fn is_denied(&self) -> bool {
        matches!(self, Self::NoReadGrant)
            || matches!(self, Self::Load(error) if matches!(error.as_ref(), ModuleLoadError::OutsideGrant{..} | ModuleLoadError::LeafSymlink{..} | ModuleLoadError::InvalidRelativeDirectory{..}))
    }
    pub fn is_check_error(&self) -> bool {
        matches!(self, Self::Check { .. } | Self::AmbiguousExport { .. })
            || matches!(self,Self::Load(error) if matches!(error.as_ref(),ModuleLoadError::Syntax{..}))
    }
}
impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("runtime load was cancelled"),
            Self::AmbiguousExport { path, name } => write!(
                f,
                "module `{}` has ambiguous value/type exports named `{name}`",
                path.display()
            ),
            Self::NoReadGrant => f.write_str("runtime load requires an explicit module read grant"),
            Self::Load(error) => error.fmt(f),
            Self::Check { path, diagnostic } => write!(
                f,
                "module `{}` failed checking: {diagnostic}",
                path.display()
            ),
            Self::Publication(error) => error.fmt(f),
            Self::Host(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(error) => Some(error.as_ref()),
            Self::Check { diagnostic, .. } => Some(diagnostic),
            Self::Publication(error) => Some(error),
            Self::Host(error) => Some(error),
            _ => None,
        }
    }
}

impl PartialEq for LoadError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::NoReadGrant, Self::NoReadGrant) | (Self::Cancelled, Self::Cancelled) => true,
            (Self::Load(a), Self::Load(b)) => Arc::ptr_eq(a, b),
            (
                Self::Check {
                    path: a,
                    diagnostic: x,
                },
                Self::Check {
                    path: b,
                    diagnostic: y,
                },
            ) => a == b && x == y,
            (
                Self::AmbiguousExport { path: a, name: x },
                Self::AmbiguousExport { path: b, name: y },
            ) => a == b && x == y,
            (Self::Publication(a), Self::Publication(b)) => a == b,
            (Self::Host(a), Self::Host(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_retain_their_variants_and_presentation() {
        let error = LoadError::Load(Arc::new(ModuleLoadError::Io {
            operation: loader::ModuleIoOperation::InspectCandidate,
            path: "plugin.ya".into(),
            source: std::io::ErrorKind::NotFound.into(),
            span: Span::default(),
        }));
        assert!(error.is_not_found());
        assert!(!error.is_denied());
        assert!(
            matches!(&error,LoadError::Load(cause) if matches!(cause.as_ref(),ModuleLoadError::Io{path,source,..} if path==std::path::Path::new("plugin.ya") && source.kind()==std::io::ErrorKind::NotFound))
        );
        let missing = LoadError::AmbiguousExport {
            path: "plugin.ya".into(),
            name: "answer".into(),
        };
        assert!(missing.is_check_error());
        assert_eq!(
            missing.to_string(),
            "module `plugin.ya` has ambiguous value/type exports named `answer`"
        );
        assert!(LoadError::NoReadGrant.is_denied());
        assert_eq!(
            LoadError::Cancelled.to_string(),
            "runtime load was cancelled"
        );
        assert_ne!(
            LoadError::Cancelled,
            LoadError::Host(cyallo_host::HostError::Stopped)
        );
    }
}
