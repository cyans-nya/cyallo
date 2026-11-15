//! Bundle actual standard source files, discovering assets from the filesystem.
//! This build step has no knowledge of exports, types, signatures or operations.

use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
enum BundleError {
    Io { path: PathBuf, source: io::Error },
    MissingEnvironment(&'static str),
    InvalidPath(PathBuf),
    Symlink(PathBuf),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::MissingEnvironment(name) => {
                write!(f, "missing build environment variable {name}")
            }
            Self::InvalidPath(path) => write!(f, "invalid standard asset path {}", path.display()),
            Self::Symlink(path) => write!(f, "standard asset is a symlink: {}", path.display()),
        }
    }
}

impl Error for BundleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Borrows path; transfers a successful filesystem result without retaining it.
fn io_at<T>(path: &Path, result: io::Result<T>) -> Result<T, BundleError> {
    result.map_err(|source| BundleError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Borrows directory through return; appends owned regular `.ya` paths to files.
fn discover(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), BundleError> {
    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in io_at(directory, fs::read_dir(directory))? {
        let entry = io_at(directory, entry)?;
        let path = entry.path();
        let kind = io_at(&path, entry.file_type())?;
        if kind.is_symlink() {
            return Err(BundleError::Symlink(path));
        }
        if kind.is_dir() {
            discover(&path, files)?;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "ya") {
            files.push(path);
        }
    }
    Ok(())
}

/// Reads repository-owned source into a generated file owned by Cargo's OUT_DIR.
/// Retains no input resources and leaves standard source files unchanged.
fn main() -> Result<(), BundleError> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .ok_or(BundleError::MissingEnvironment("CARGO_MANIFEST_DIR"))?;
    let root = Path::new(&manifest).join("../..");
    let mut paths = vec![root.join("std.ya")];
    discover(&root.join("std"), &mut paths)?;
    paths.sort();
    let mut generated = String::from("pub const SOURCES: &[Source] = &[\n");
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
        if io_at(&path, fs::symlink_metadata(&path))?
            .file_type()
            .is_symlink()
        {
            return Err(BundleError::Symlink(path));
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| BundleError::InvalidPath(path.clone()))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| BundleError::InvalidPath(path.clone()))?;
        let source = io_at(&path, fs::read_to_string(&path))?;
        generated.push_str(&format!(
            "    Source {{ path: {relative:?}, source: {source:?} }},\n"
        ));
    }
    generated.push_str("];\n");
    let output = PathBuf::from(
        std::env::var_os("OUT_DIR").ok_or(BundleError::MissingEnvironment("OUT_DIR"))?,
    )
    .join("standard_sources.rs");
    io_at(&output, fs::write(&output, generated))
}
