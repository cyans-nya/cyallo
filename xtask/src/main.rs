//! Reconstruct the fork from an immutable crates.io archive and exact patches.
mod editors;

use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Task,
}
#[derive(Subcommand)]
enum Task {
    /// Generate editor parsers or package the development extension.
    Editors {
        #[command(subcommand)]
        command: editors::Task,
    },
    VendorWinch {
        #[command(subcommand)]
        command: Vendor,
    },
}
#[derive(Subcommand)]
enum Vendor {
    /// Verify archive, series, reconstruction, and the checked-in tree.
    Check {
        #[arg(long)]
        archive: Option<PathBuf>,
        #[arg(long)]
        fetch: bool,
    },
    /// Reconstruct the recorded revision into a new directory for review.
    Import {
        destination: PathBuf,
        #[arg(long)]
        archive: Option<PathBuf>,
        #[arg(long)]
        fetch: bool,
    },
    /// Try the exact patches on an isolated candidate; never changes production.
    Update {
        version: String,
        destination: PathBuf,
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Report upstream releases and changes to the reused compiler subsystems.
    CheckUpstream {
        #[arg(long)]
        output: PathBuf,
    },
}
fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(format!(
            "{cmd:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn manifest(root: &Path) -> Result<BTreeMap<String, String>> {
    // The provenance manifest deliberately contains only flat string fields.
    let mut fields = BTreeMap::new();
    for line in fs::read_to_string(root.join("vendor/UPSTREAM.toml"))?.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("invalid provenance field")?;
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or("provenance fields must be quoted strings")?;
        fields.insert(key.trim().into(), value.into());
    }
    Ok(fields)
}
fn field<'a>(m: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    Ok(m.get(key)
        .ok_or_else(|| format!("missing provenance field {key}"))?)
}
fn files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                return Err("vendor tree must not contain symlinks".into());
            }
            if ty.is_dir() {
                visit(root, &entry.path(), out)?;
            } else {
                out.insert(
                    entry.path().strip_prefix(root)?.to_path_buf(),
                    fs::read(entry.path())?,
                );
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out)?;
    Ok(out)
}
fn series(root: &Path, m: &BTreeMap<String, String>) -> Result<Vec<PathBuf>> {
    let dir = root.join("vendor/patches/winch-codegen");
    let listing = fs::read_to_string(dir.join("series"))?;
    let mut bytes = listing.as_bytes().to_vec();
    let mut patches = vec![];
    for name in listing
        .lines()
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        if Path::new(name).components().count() != 1 {
            return Err("invalid patch path".into());
        }
        let path = dir.join(name);
        bytes.extend_from_slice(&fs::read(&path)?);
        patches.push(path);
    }
    if hash(&bytes) != field(m, "patch_series_sha256")? {
        return Err(
            "patch series checksum mismatch; record reviewed patches and metadata together".into(),
        );
    }
    Ok(patches)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "caie-winch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn archive(version: &str, given: Option<PathBuf>, fetch: bool, temp: &Path) -> Result<PathBuf> {
    if !version.chars().all(|c| c.is_ascii_digit() || c == '.') || version.split('.').count() != 3 {
        return Err("expected a release version like 50.0.1".into());
    }
    if let Some(path) = given {
        return Ok(fs::canonicalize(path)?);
    }
    let cargo = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo")
        });
    let filename = format!("winch-codegen-{version}.crate");
    if let Ok(entries) = fs::read_dir(cargo.join("registry/cache")) {
        for entry in entries {
            let path = entry?.path().join(&filename);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    if !fetch {
        return Err("source archive is not cached; supply --archive or --fetch".into());
    }
    let path = temp.join(filename);
    run(Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
        ])
        .arg(format!(
            "https://static.crates.io/crates/winch-codegen/winch-codegen-{version}.crate"
        ))
        .arg("--output")
        .arg(&path))?;
    Ok(path)
}
fn reconstruct(
    root: &Path,
    m: &BTreeMap<String, String>,
    source: &Path,
    scratch: &Path,
    version: &str,
    verify: bool,
) -> Result<PathBuf> {
    if verify && hash(&fs::read(source)?) != field(m, "source_sha256")? {
        return Err("upstream source checksum mismatch".into());
    }
    run(Command::new("tar")
        .arg("-xzf")
        .arg(source)
        .arg("-C")
        .arg(scratch))?;
    let extracted = scratch.join(format!("winch-codegen-{version}"));
    let tree = scratch.join("tree");
    fs::create_dir(&tree)?;
    // Explicit import set excludes Cargo's registry/cache metadata and release lockfile.
    for name in [
        "Cargo.toml",
        "Cargo.toml.orig",
        "build.rs",
        "LICENSE",
        "src",
    ] {
        let path = extracted.join(name);
        if path.is_dir() {
            copy_tree(&path, &tree.join(name))?;
        } else {
            fs::copy(path, tree.join(name))?;
        }
    }
    for patch in series(root, m)? {
        // git apply requires exact context and does not use patch's fuzzy matching.
        let diagnostic = run(Command::new("git")
            .current_dir(&tree)
            .args(["apply", "--no-index", "--verbose"])
            .arg(patch))?;
        if !diagnostic.is_empty() {
            eprint!("{diagnostic}");
        }
    }
    Ok(tree)
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for (name, contents) in files(source)? {
        let path = destination.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, contents)?;
    }
    Ok(())
}
fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let command = match Cli::parse().command {
        Task::Editors { command } => return Ok(editors::run(root, command)?),
        Task::VendorWinch { command } => command,
    };
    let m = manifest(root)?;
    let scratch = Scratch::new()?;
    match command {
        Vendor::Check {
            archive: given,
            fetch,
        } => {
            let version = field(&m, "version")?;
            let source = archive(version, given, fetch, &scratch.0)?;
            let tree = reconstruct(root, &m, &source, &scratch.0, version, true)?;
            let expected = files(&tree)?;
            let actual = files(&root.join("vendor/winch-codegen"))?;
            if expected != actual {
                let changed: Vec<_> = expected
                    .keys()
                    .chain(actual.keys())
                    .filter(|key| expected.get(*key) != actual.get(*key))
                    .collect();
                return Err(format!("unrecorded vendor changes: {changed:?}").into());
            }
            println!(
                "Winch {version}: source checksum, ordered patches, and {} files verified",
                actual.len()
            );
        }
        Vendor::Import {
            destination,
            archive: given,
            fetch,
        } => {
            if destination.exists() {
                return Err("destination already exists".into());
            }
            let version = field(&m, "version")?;
            let source = archive(version, given, fetch, &scratch.0)?;
            let tree = reconstruct(root, &m, &source, &scratch.0, version, true)?;
            copy_tree(&tree, &destination)?;
            println!("Reconstructed Winch {version} at {}", destination.display());
        }
        Vendor::Update {
            version,
            destination,
            archive: given,
        } => {
            if destination.exists() {
                return Err("candidate destination already exists".into());
            }
            let source = archive(&version, given, true, &scratch.0)?;
            fs::create_dir_all(&destination)?;
            let checksum = hash(&fs::read(&source)?);
            let result = reconstruct(root, &m, &source, &scratch.0, &version, false);
            let status = match result {
                Ok(tree) => {
                    copy_tree(&tree, &destination.join("winch-codegen"))?;
                    "Exact-context patches applied. Review APIs, ABI, dependency cohort, and run native validation before updating the pin.".into()
                }
                Err(e) => format!("Patch application blocked: {e}"),
            };
            fs::write(
                destination.join("CANDIDATE.md"),
                format!(
                    "# Winch {version} candidate\n\nSource SHA256: `{checksum}`\n\n{status}\n\nProduction remains pinned to {} / {}. Update the commit, coherent compiler dependencies, checksums, patch rationale, native ARM64/x64 validation, and benchmark evidence together. No automatic pin advance.\n",
                    field(&m, "version")?,
                    field(&m, "commit")?
                ),
            )?;
            println!(
                "Candidate report: {}",
                destination.join("CANDIDATE.md").display()
            );
        }
        Vendor::CheckUpstream { output } => {
            let repo = scratch.0.join("upstream");
            run(Command::new("git")
                .args([
                    "clone",
                    "--quiet",
                    "--filter=blob:none",
                    "--bare",
                    field(&m, "repository")?,
                ])
                .arg(&repo))?;
            let tags = run(Command::new("git").arg("--git-dir").arg(&repo).args([
                "tag",
                "--list",
                "v*",
                "--sort=-version:refname",
            ]))?;
            let changes = run(Command::new("git")
                .arg("--git-dir")
                .arg(&repo)
                .args(["log", "--oneline", "--max-count=80"])
                .arg(format!("{}..HEAD", field(&m, "commit")?))
                .args([
                    "--",
                    "winch/codegen/src/isa",
                    "winch/codegen/src/regalloc.rs",
                    "winch/codegen/src/masm.rs",
                    "cranelift/jit",
                    "cranelift/module",
                    "Cargo.toml",
                ]))?;
            fs::write(
                output,
                format!(
                    "# Upstream compiler watch\n\nPinned: {} / {}\n\n## Recent releases\n\n```text\n{}\n```\n\n## Relevant changes since the pin (up to 80)\n\n```text\n{changes}```\n\nUse `cargo xtask vendor-winch update VERSION DESTINATION` for an isolated candidate. Production pins are never advanced by this check.\n",
                    field(&m, "version")?,
                    field(&m, "commit")?,
                    tags.lines().take(15).collect::<Vec<_>>().join("\n")
                ),
            )?;
        }
    }
    Ok(())
}
