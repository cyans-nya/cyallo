//! Declared-file module discovery within an explicit directory read grant.
//!
//! This loader returns ordinary user source data. Neither source filenames nor
//! the spelling of a declared module can obtain trusted core provenance.

use crate::{Diagnostic, SourceModule, Span};
use cyallo_ast::ModuleDeclaration;
use std::{
    collections::HashSet,
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleIoOperation {
    CanonicalizeEntry,
    CanonicalizeGrant,
    InspectGrant,
    ResolveDirectory,
    InspectDirectory,
    InspectCandidate,
    CanonicalizeSource,
    ReadSource,
}
impl fmt::Display for ModuleIoOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CanonicalizeEntry => "resolve entry module",
            Self::CanonicalizeGrant => "resolve module read grant",
            Self::InspectGrant => "inspect module read grant",
            Self::ResolveDirectory => "resolve declared module directory",
            Self::InspectDirectory => "inspect declared module directory",
            Self::InspectCandidate => "inspect declared module candidate",
            Self::CanonicalizeSource => "resolve declared module source",
            Self::ReadSource => "read module source",
        })
    }
}

#[derive(Debug)]
pub enum ModuleLoadError {
    Io {
        operation: ModuleIoOperation,
        path: PathBuf,
        source: io::Error,
        span: Span,
    },
    GrantNotDirectory {
        path: PathBuf,
    },
    SourceNotFile {
        path: PathBuf,
        span: Span,
    },
    OutsideGrant {
        path: PathBuf,
        resolved: PathBuf,
        grant: PathBuf,
        span: Span,
    },
    InvalidRelativeDirectory {
        path: PathBuf,
        grant: PathBuf,
        span: Span,
    },
    LeafSymlink {
        path: PathBuf,
        span: Span,
    },
    MissingSource {
        module: Box<str>,
        candidates: [PathBuf; 2],
        span: Span,
    },
    AmbiguousSource {
        module: Box<str>,
        candidates: [PathBuf; 2],
        span: Span,
    },
    CyclicSource {
        path: PathBuf,
        chain: Vec<PathBuf>,
        span: Span,
    },
    Syntax {
        path: PathBuf,
        diagnostic: Diagnostic,
    },
}
impl ModuleLoadError {
    pub fn span(&self) -> Span {
        match self {
            Self::Io { span, .. }
            | Self::SourceNotFile { span, .. }
            | Self::OutsideGrant { span, .. }
            | Self::InvalidRelativeDirectory { span, .. }
            | Self::LeafSymlink { span, .. }
            | Self::MissingSource { span, .. }
            | Self::AmbiguousSource { span, .. }
            | Self::CyclicSource { span, .. } => *span,
            Self::Syntax { diagnostic, .. } => diagnostic.span,
            Self::GrantNotDirectory { .. } => Span::default(),
        }
    }
}
impl fmt::Display for ModuleLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
                ..
            } => write!(f, "cannot {operation} `{}`: {source}", path.display()),
            Self::GrantNotDirectory { path } => write!(
                f,
                "module read grant is not a directory: `{}`",
                path.display()
            ),
            Self::SourceNotFile { path, .. } => write!(
                f,
                "module source is not a regular file: `{}`",
                path.display()
            ),
            Self::OutsideGrant {
                path,
                resolved,
                grant,
                ..
            } => write!(
                f,
                "module path `{}` resolves outside read grant `{}` to `{}`",
                path.display(),
                grant.display(),
                resolved.display()
            ),
            Self::InvalidRelativeDirectory { path, grant, .. } => write!(
                f,
                "declared module directory `{}` is not relative to read grant `{}`",
                path.display(),
                grant.display()
            ),
            Self::LeafSymlink { path, .. } => {
                write!(f, "module sources cannot be symlinks: `{}`", path.display())
            }
            Self::MissingSource {
                module, candidates, ..
            } => write!(
                f,
                "expected exactly one source for module {module}; neither `{}` nor `{}` is a file",
                candidates[0].display(),
                candidates[1].display()
            ),
            Self::AmbiguousSource {
                module, candidates, ..
            } => write!(
                f,
                "expected exactly one source for module {module}; both `{}` and `{}` are files",
                candidates[0].display(),
                candidates[1].display()
            ),
            Self::CyclicSource { path, chain, .. } => {
                write!(f, "cyclic module source `{}`", path.display())?;
                for path in chain {
                    write!(f, " → `{}`", path.display())?;
                }
                Ok(())
            }
            Self::Syntax { path, diagnostic } => {
                write!(f, "cannot parse module `{}`: {diagnostic}", path.display())
            }
        }
    }
}
impl std::error::Error for ModuleLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Syntax { diagnostic, .. } => Some(diagnostic),
            _ => None,
        }
    }
}

/// Owns loader diagnostic mapping. Syntax retains its parser category and
/// source-local span; the filename is added only at this presentation boundary.
pub fn module_load_diagnostic(error: ModuleLoadError) -> Diagnostic {
    let span = error.span();
    match error {
        ModuleLoadError::Syntax {
            path,
            mut diagnostic,
        } => {
            diagnostic.message = format!("module `{}`: {}", path.display(), diagnostic.message);
            diagnostic
        }
        error => Diagnostic::new("E140", error.to_string(), span),
    }
}

fn io_at<T>(
    operation: ModuleIoOperation,
    path: &Path,
    span: Span,
    result: io::Result<T>,
) -> Result<T, ModuleLoadError> {
    result.map_err(|source| ModuleLoadError::Io {
        operation,
        path: path.to_owned(),
        source,
        span,
    })
}
fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[derive(Debug, Clone)]
pub struct ReadGrant {
    root: PathBuf,
}
impl ReadGrant {
    /// Canonicalizes the host-selected directory and owns its path. Retains no
    /// caller storage and grants no language/compiler trust to its contents.
    pub fn directory(path: &Path) -> Result<Self, ModuleLoadError> {
        let root = io_at(
            ModuleIoOperation::CanonicalizeGrant,
            path,
            Span::default(),
            fs::canonicalize(path),
        )?;
        let metadata = io_at(
            ModuleIoOperation::InspectGrant,
            &root,
            Span::default(),
            fs::metadata(&root),
        )?;
        if !metadata.is_dir() {
            return Err(ModuleLoadError::GrantNotDirectory { path: root });
        }
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn check(&self, path: &Path, resolved: &Path, span: Span) -> Result<(), ModuleLoadError> {
        if resolved.starts_with(&self.root) {
            Ok(())
        } else {
            Err(ModuleLoadError::OutsideGrant {
                path: path.to_owned(),
                resolved: resolved.to_owned(),
                grant: self.root.clone(),
                span,
            })
        }
    }

    /// Resolves each declared ancestor before touching the next component. An
    /// outside symlink directory is rejected even if its requested leaf is absent.
    fn directory_for(&self, path: &Path, span: Span) -> Result<Option<PathBuf>, ModuleLoadError> {
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            ModuleLoadError::InvalidRelativeDirectory {
                path: path.to_owned(),
                grant: self.root.clone(),
                span,
            }
        })?;
        let mut current = io_at(
            ModuleIoOperation::ResolveDirectory,
            &self.root,
            span,
            fs::canonicalize(&self.root),
        )?;
        self.check(&self.root, &current, span)?;
        for component in relative.components() {
            match component {
                Component::Normal(name) => current.push(name),
                Component::CurDir => continue,
                _ => {
                    return Err(ModuleLoadError::InvalidRelativeDirectory {
                        path: path.to_owned(),
                        grant: self.root.clone(),
                        span,
                    });
                }
            }
            let resolved = match fs::canonicalize(&current) {
                Ok(resolved) => resolved,
                Err(source) if absent(&source) => return Ok(None),
                Err(source) => {
                    return Err(ModuleLoadError::Io {
                        operation: ModuleIoOperation::ResolveDirectory,
                        path: current,
                        source,
                        span,
                    });
                }
            };
            self.check(&current, &resolved, span)?;
            let metadata = io_at(
                ModuleIoOperation::InspectDirectory,
                &resolved,
                span,
                fs::metadata(&resolved),
            )?;
            if !metadata.is_dir() {
                return Ok(None);
            }
            current = resolved;
        }
        Ok(Some(current))
    }
}

#[derive(Debug, Clone)]
pub struct ModuleLoader {
    grant: ReadGrant,
}
impl ModuleLoader {
    pub fn new(grant: ReadGrant) -> Self {
        Self { grant }
    }
    /// The host-selected entry explicitly grants its canonical parent directory.
    pub fn for_entry(path: &Path) -> Result<Self, ModuleLoadError> {
        let entry = io_at(
            ModuleIoOperation::CanonicalizeEntry,
            path,
            Span::default(),
            fs::canonicalize(path),
        )?;
        Ok(Self::new(ReadGrant::directory(
            entry.parent().unwrap_or(Path::new(".")),
        )?))
    }
    pub fn grant(&self) -> &ReadGrant {
        &self.grant
    }

    /// Resolves a runtime-selected file under this explicit grant, rejecting leaf symlinks.
    pub fn resolve_file(&self, path: &Path) -> Result<PathBuf, ModuleLoadError> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.grant.root().join(path)
        };
        let load = Load {
            grant: &self.grant,
            active: HashSet::new(),
            stack: vec![],
            sources: vec![],
        };
        load.candidate(&path, Span::default())?
            .map(|candidate| candidate.canonical)
            .ok_or_else(|| ModuleLoadError::Io {
                operation: ModuleIoOperation::InspectCandidate,
                path,
                source: io::Error::from(io::ErrorKind::NotFound),
                span: Span::default(),
            })
    }

    /// Loads declared source files only. All per-load identities and cycle state
    /// are local to this call; errors do not poison later loads of this instance.
    pub fn load(&self, entry: &Path) -> Result<Vec<SourceModule>, ModuleLoadError> {
        let canonical = io_at(
            ModuleIoOperation::CanonicalizeEntry,
            entry,
            Span::default(),
            fs::canonicalize(entry),
        )?;
        self.grant.check(entry, &canonical, Span::default())?;
        let metadata = io_at(
            ModuleIoOperation::InspectCandidate,
            &canonical,
            Span::default(),
            fs::metadata(&canonical),
        )?;
        if !metadata.is_file() {
            return Err(ModuleLoadError::SourceNotFile {
                path: canonical,
                span: Span::default(),
            });
        }
        let directory = canonical.parent().unwrap_or(self.grant.root());
        let mut load = Load {
            grant: &self.grant,
            active: HashSet::new(),
            stack: vec![],
            sources: vec![],
        };
        load.file(&canonical, &canonical, directory, Span::default())?;
        Ok(load.sources)
    }
}

struct Candidate {
    label: PathBuf,
    canonical: PathBuf,
}
struct Load<'a> {
    grant: &'a ReadGrant,
    active: HashSet<PathBuf>,
    stack: Vec<PathBuf>,
    sources: Vec<SourceModule>,
}
impl Load<'_> {
    fn candidate(&self, label: &Path, span: Span) -> Result<Option<Candidate>, ModuleLoadError> {
        let parent = label.parent().unwrap_or(self.grant.root());
        let Some(directory) = self.grant.directory_for(parent, span)? else {
            return Ok(None);
        };
        let Some(name) = label.file_name() else {
            return Ok(None);
        };
        let path = directory.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if absent(&source) => return Ok(None),
            Err(source) => {
                return Err(ModuleLoadError::Io {
                    operation: ModuleIoOperation::InspectCandidate,
                    path,
                    source,
                    span,
                });
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(ModuleLoadError::LeafSymlink {
                path: label.to_owned(),
                span,
            });
        }
        if !metadata.is_file() {
            return Ok(None);
        }
        let canonical = io_at(
            ModuleIoOperation::CanonicalizeSource,
            &path,
            span,
            fs::canonicalize(&path),
        )?;
        self.grant.check(label, &canonical, span)?;
        Ok(Some(Candidate {
            label: label.to_owned(),
            canonical,
        }))
    }

    fn declarations(
        &mut self,
        directory: &Path,
        modules: &[ModuleDeclaration],
    ) -> Result<(), ModuleLoadError> {
        for module in modules {
            let child_dir = directory.join(module.name.as_ref());
            if let Some(body) = &module.body {
                self.declarations(&child_dir, &body.modules)?;
                continue;
            }
            let candidates = [
                directory.join(format!("{}.ya", module.name)),
                child_dir.join("mod.ya"),
            ];
            let first = self.candidate(&candidates[0], module.span)?;
            let second = self.candidate(&candidates[1], module.span)?;
            let selected = match (first, second) {
                (Some(_), Some(_)) => {
                    return Err(ModuleLoadError::AmbiguousSource {
                        module: module.name.clone(),
                        candidates,
                        span: module.span,
                    });
                }
                (None, None) => {
                    return Err(ModuleLoadError::MissingSource {
                        module: module.name.clone(),
                        candidates,
                        span: module.span,
                    });
                }
                (Some(source), None) | (None, Some(source)) => source,
            };
            self.file(
                &selected.label,
                &selected.canonical,
                &child_dir,
                module.span,
            )?;
        }
        Ok(())
    }

    fn file(
        &mut self,
        label: &Path,
        canonical: &Path,
        directory: &Path,
        span: Span,
    ) -> Result<(), ModuleLoadError> {
        self.grant.check(label, canonical, span)?;
        if !self.active.insert(canonical.to_owned()) {
            let mut chain = self.stack.clone();
            chain.push(canonical.to_owned());
            return Err(ModuleLoadError::CyclicSource {
                path: canonical.to_owned(),
                chain,
                span,
            });
        }
        self.stack.push(canonical.to_owned());
        let result = (|| {
            let source = io_at(
                ModuleIoOperation::ReadSource,
                canonical,
                span,
                fs::read_to_string(canonical),
            )?;
            let ast = cyallo_parser::parse_modern(&source, false).map_err(|diagnostic| {
                ModuleLoadError::Syntax {
                    path: label.to_owned(),
                    diagnostic,
                }
            })?;
            self.declarations(directory, &ast.modules)?;
            self.sources
                .push(SourceModule::new(label.display().to_string(), source));
            Ok(())
        })();
        self.stack.pop();
        self.active.remove(canonical);
        result
    }
}

/// Grants the explicitly selected canonical entry's parent directory and loads
/// its declared graph. Descendants cannot widen this directory grant.
pub fn load_path_checked(path: &Path) -> Result<Vec<SourceModule>, ModuleLoadError> {
    ModuleLoader::for_entry(path)?.load(path)
}
pub fn load_path(path: &Path) -> Result<Vec<SourceModule>, Diagnostic> {
    load_path_checked(path).map_err(module_load_diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for attempt in 0.. {
                let path = std::env::temp_dir().join(format!(
                    "cyallo-module-loader-{}-{nonce}-{attempt}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(fs::canonicalize(path).unwrap()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create test directory: {error}"),
                }
            }
            unreachable!()
        }
        fn write(&self, path: &str, source: &str) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, source).unwrap();
            path
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_only_declared_files_and_returns_children_before_the_entry() {
        let temp = TempDir::new();
        let entry = temp.write("main.ya", "mod math;\n");
        let child = temp.write("math.ya", "pub const VALUE = 42\n");
        fs::write(temp.0.join("ignored.ya"), [0xff]).unwrap();
        fs::write(temp.0.join("ignored.psc"), [0xff]).unwrap();
        let sources = load_path_checked(&entry).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(Path::new(&sources[0].path), child);
        assert_eq!(Path::new(&sources[1].path), entry);
        assert_eq!(sources[0].source, "pub const VALUE = 42\n");
    }

    #[test]
    fn directory_module_and_inline_child_directory_use_the_native_rule() {
        let temp = TempDir::new();
        let entry = temp.write("main.ya", "mod group {\n mod directory;\n}\n");
        let child = temp.write("group/directory/mod.ya", "mod nested;\n");
        let nested = temp.write("group/directory/nested.ya", "pub const VALUE = 3\n");
        let sources = load_path_checked(&entry).unwrap();
        assert_eq!(sources.len(), 3);
        assert_eq!(Path::new(&sources[0].path), nested);
        assert_eq!(Path::new(&sources[1].path), child);
        assert_eq!(Path::new(&sources[2].path), entry);
    }

    #[test]
    fn missing_and_ambiguous_candidates_are_distinct_typed_failures() {
        let temp = TempDir::new();
        let entry = temp.write("main.ya", "mod child;\n");
        assert!(matches!(
            load_path_checked(&entry),
            Err(ModuleLoadError::MissingSource { .. })
        ));
        temp.write("child.ya", "");
        temp.write("child/mod.ya", "");
        let error = load_path_checked(&entry).unwrap_err();
        assert!(matches!(error, ModuleLoadError::AmbiguousSource { .. }));
        assert_eq!(module_load_diagnostic(error).code, "E140");
    }

    #[test]
    fn io_and_syntax_failures_retain_paths_and_concrete_causes() {
        let temp = TempDir::new();
        let missing = temp.0.join("missing.ya");
        let error = load_path_checked(&missing).unwrap_err();
        assert!(
            matches!(error,ModuleLoadError::Io {operation:ModuleIoOperation::CanonicalizeEntry,ref path,ref source,..} if *path==missing && source.kind()==io::ErrorKind::NotFound)
        );
        assert!(std::error::Error::source(&error).is_some());
        let entry = temp.write("main.ya", "mod child;\n");
        fs::write(temp.0.join("child.ya"), [0xff]).unwrap();
        assert!(
            matches!(load_path_checked(&entry),Err(ModuleLoadError::Io {operation:ModuleIoOperation::ReadSource,source,..}) if source.kind()==io::ErrorKind::InvalidData)
        );
        temp.write("child.ya", "proc broken( {\n");
        let error = load_path_checked(&entry).unwrap_err();
        assert!(matches!(error, ModuleLoadError::Syntax { .. }));
        let diagnostic = module_load_diagnostic(error);
        assert_eq!(diagnostic.code, "E002");
        assert!(diagnostic.message.contains("child.ya"));
    }

    #[test]
    fn explicit_grants_and_reused_loader_instances_have_no_ambient_state() {
        let temp = TempDir::new();
        let entry = temp.write("inside/main.ya", "mod child;\n");
        let outside = temp.write("outside/main.ya", "");
        let loader = ModuleLoader::new(ReadGrant::directory(&temp.0.join("inside")).unwrap());
        assert!(matches!(
            loader.load(&outside),
            Err(ModuleLoadError::OutsideGrant { .. })
        ));
        assert!(matches!(
            loader.load(&entry),
            Err(ModuleLoadError::MissingSource { .. })
        ));
        temp.write("inside/child.ya", "");
        assert_eq!(loader.load(&entry).unwrap().len(), 2);
        assert_eq!(
            ModuleLoader::new(loader.grant().clone())
                .load(&entry)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn std_shaped_files_are_ordinary_source_data() {
        let temp = TempDir::new();
        let entry = temp.write("main.ya", "mod std;\n");
        let source = "#[cyallo_internal(\"task_struct\")]\npub record Counterfeit<T> {}\n";
        let child = temp.write("std.ya", source);
        let sources = load_path_checked(&entry).unwrap();
        assert_eq!(Path::new(&sources[0].path), child);
        assert_eq!(sources[0].source, source);
        // SourceModule carries text/path only; the namespace assigns User trust.
    }

    #[cfg(unix)]
    #[test]
    fn rejects_leaf_links_and_out_of_grant_intermediate_directories() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new();
        let entry = temp.write("inside/main.ya", "mod child;\n");
        let outside = temp.write("outside/mod.ya", "");
        symlink(&outside, temp.0.join("inside/child.ya")).unwrap();
        assert!(matches!(
            load_path_checked(&entry),
            Err(ModuleLoadError::LeafSymlink { .. })
        ));
        fs::remove_file(temp.0.join("inside/child.ya")).unwrap();
        symlink(temp.0.join("outside"), temp.0.join("inside/child")).unwrap();
        assert!(matches!(
            load_path_checked(&entry),
            Err(ModuleLoadError::OutsideGrant { .. })
        ));
        // Deny the directory boundary even when no requested leaf exists there.
        fs::remove_file(&outside).unwrap();
        assert!(matches!(
            load_path_checked(&entry),
            Err(ModuleLoadError::OutsideGrant { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_file_identity_detects_cycles_through_in_grant_directory_links() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new();
        let entry = temp.write("mod.ya", "mod child;\n");
        symlink(&temp.0, temp.0.join("child")).unwrap();
        assert!(
            matches!(load_path_checked(&entry),Err(ModuleLoadError::CyclicSource {ref chain,..}) if chain.len()==2 && chain[0]==chain[1])
        );
    }

    #[cfg(unix)]
    #[test]
    fn allowed_directory_aliases_retain_declared_paths_for_namespace_linking() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new();
        let entry = temp.write("main.ya", "mod alias;\n");
        temp.write("actual/mod.ya", "pub const VALUE = 7\n");
        symlink(temp.0.join("actual"), temp.0.join("alias")).unwrap();
        let sources = load_path_checked(&entry).unwrap();
        assert_eq!(Path::new(&sources[0].path), temp.0.join("alias/mod.ya"));
        let asts = sources
            .iter()
            .map(|source| cyallo_parser::parse(&source.source).unwrap())
            .collect();
        let paths = sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>();
        assert!(cyallo_ir::namespace::link(asts, &paths, sources.len() - 1).is_ok());
    }
}
