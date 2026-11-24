use zed_extension_api::{self as zed, LanguageServerId, settings::LspSettings};

mod dev_server;

struct CyalloExtension;

impl zed::Extension for CyalloExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let binary = LspSettings::for_worktree(id.as_ref(), worktree)?.binary;
        let path = binary.as_ref().and_then(|b| b.path.clone())
            .or_else(|| worktree.which("cya-ls"))
            .or_else(|| dev_server::PATH.map(str::to_owned))
            .ok_or_else(|| "Build cya-ls with `cargo build -p cya-ls` and set lsp.cya-ls.binary.path, or install cya-ls on PATH".to_owned())?;
        let args = binary
            .and_then(|b| b.arguments)
            .unwrap_or_else(|| vec!["--stdio".into()]);
        Ok(zed::Command {
            command: path,
            args,
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(CyalloExtension);
