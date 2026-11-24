//! Reproducible parser generation and self-contained local editor artifacts.
use clap::Subcommand;
use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

const CLI_VERSION: &str = "0.27.2";
const LANGUAGES: &[&str] = &["cyallo", "caie"];
const GENERATED: &[&str] = &[
    "src/parser.c",
    "src/grammar.json",
    "src/node-types.json",
    "src/tree_sitter/parser.h",
    "src/tree_sitter/alloc.h",
    "src/tree_sitter/array.h",
];

#[derive(Subcommand)]
pub enum Task {
    /// Regenerate parsers with the pinned CLI, or verify them without changing source.
    Generate {
        #[arg(long)]
        check: bool,
    },
    /// Prepare target/editor-dev/zed for Zed's "Install Dev Extension" command.
    ZedDev,
}
#[derive(Debug)]
pub enum EditorError {
    Io(std::io::Error),
    Command {
        command: String,
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },
    Utf8(std::string::FromUtf8Error),
    StaleParser(PathBuf),
    Symlink(PathBuf),
    CliVersion(String),
}
impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Utf8(e) => e.fmt(f),
            Self::Command {
                command,
                status,
                stdout,
                stderr,
            } => write!(f, "{command} failed ({status}):\n{stdout}{stderr}"),
            Self::StaleParser(path) => write!(
                f,
                "{} differs from the pinned generator; run cargo xtask editors generate",
                path.display()
            ),
            Self::Symlink(path) => write!(
                f,
                "unexpected symlink in editor sources: {}",
                path.display()
            ),
            Self::CliVersion(actual) => {
                write!(f, "expected Tree-sitter {CLI_VERSION}, got {actual}")
            }
        }
    }
}
impl Error for EditorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Utf8(e) => Some(e),
            _ => None,
        }
    }
}
impl From<std::io::Error> for EditorError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
type Result<T> = std::result::Result<T, EditorError>;

fn command(cmd: &mut Command) -> Result<String> {
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(EditorError::Command {
            command: format!("{cmd:?}"),
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(EditorError::Utf8)
}
fn generator(directory: &Path) -> Result<()> {
    let mut cmd = if let Some(cli) = std::env::var_os("TREE_SITTER_CLI") {
        Command::new(cli)
    } else {
        let installed = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.join(".vite-plus/bin/vpx"));
        let mut cmd = Command::new(
            installed
                .filter(|p| p.is_file())
                .unwrap_or_else(|| "vpx".into()),
        );
        cmd.arg(format!("tree-sitter-cli@{CLI_VERSION}"));
        cmd
    };
    // A caller-supplied binary must have the same version as the download pin.
    let version = command(cmd.current_dir(directory).arg("--version"))?;
    if version.trim() != format!("tree-sitter {CLI_VERSION}") {
        return Err(EditorError::CliVersion(version.trim().into()));
    }
    // Rebuild the command because the version argument belongs to a separate run.
    let program = cmd.get_program().to_owned();
    let args: Vec<_> = cmd.get_args().map(|a| a.to_owned()).collect();
    let mut generate = Command::new(program);
    generate
        .args(&args[..args.len() - 1])
        .current_dir(directory)
        .args(["generate", "--abi", "14", "--js-runtime", "native"]);
    command(&mut generate)?;
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" || name == "node_modules" {
            continue;
        }
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            return Err(EditorError::Symlink(entry.path()));
        }
        let target = destination.join(name);
        if ty.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn snapshot(path: &Path) -> Result<String> {
    command(Command::new("git").current_dir(path).args([
        "init",
        "--quiet",
        "--initial-branch=main",
    ]))?;
    command(
        Command::new("git")
            .current_dir(path)
            .args(["add", "--", "."]),
    )?;
    let tree = command(Command::new("git").current_dir(path).arg("write-tree"))?;
    let revision = command(
        Command::new("git")
            .current_dir(path)
            .args([
                "commit-tree",
                tree.trim(),
                "-m",
                "Local editor grammar snapshot",
            ])
            .env("GIT_AUTHOR_NAME", "Cyallo editor build")
            .env("GIT_COMMITTER_NAME", "Cyallo editor build")
            .env("GIT_AUTHOR_EMAIL", "editor-build@localhost")
            .env("GIT_COMMITTER_EMAIL", "editor-build@localhost")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
    )?;
    command(Command::new("git").current_dir(path).args([
        "update-ref",
        "refs/heads/main",
        revision.trim(),
    ]))?;
    Ok(revision.trim().into())
}
fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(url, "%{byte:02X}").expect("String writes cannot fail");
        }
    }
    url
}

pub fn run(root: &Path, task: Task) -> Result<()> {
    match task {
        Task::Generate { check } => {
            for language in LANGUAGES {
                let source = root.join(format!("crates/tree-sitter-{language}"));
                if check {
                    let work = root.join(format!("target/editor-grammar-check/{language}"));
                    fs::create_dir_all(&work)?;
                    for name in ["grammar.js", "tree-sitter.json"] {
                        fs::copy(source.join(name), work.join(name))?;
                    }
                    generator(&work)?;
                    for name in GENERATED {
                        if fs::read(work.join(name))? != fs::read(source.join(name))? {
                            return Err(EditorError::StaleParser(source.join(name)));
                        }
                    }
                } else {
                    generator(&source)?;
                }
                println!(
                    "{} {language} parser (Tree-sitter {CLI_VERSION}, ABI 14)",
                    if check { "Verified" } else { "Generated" }
                );
            }
        }
        Task::ZedDev => {
            let output = root.join("target/editor-dev");
            let extension = output.join("zed");
            copy_directory(&root.join("editors/zed"), &extension)?;
            let server = fs::canonicalize(root)?
                .join("target/debug")
                .join(format!("cya-ls{}", std::env::consts::EXE_SUFFIX));
            fs::write(
                extension.join("src/dev_server.rs"),
                format!(
                    "pub const PATH: Option<&str> = Some({:?});\n",
                    server.to_string_lossy()
                ),
            )?;
            let mut manifest = fs::read_to_string(root.join("editors/zed/extension.toml"))?;
            for language in LANGUAGES {
                let grammar = root.join(format!("crates/tree-sitter-{language}"));
                let local = output.join(format!("grammars/{language}"));
                copy_directory(&grammar, &local)?;
                let revision = snapshot(&local)?;
                let repository = file_url(&fs::canonicalize(&local)?);
                manifest.push_str(&format!(
                    "\n[grammars.{language}]\nrepository = {repository:?}\nrev = {revision:?}\n"
                ));
                fs::copy(
                    grammar.join("queries/highlights.scm"),
                    extension.join(format!("languages/{language}/highlights.scm")),
                )?;
            }
            fs::write(extension.join("extension.toml"), manifest)?;
            println!(
                "Install this folder with Zed's Extensions: Install Dev Extension command:\n{}",
                extension.display()
            );
        }
    }
    Ok(())
}
