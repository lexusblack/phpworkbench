use zed_extension_api as zed;

struct DepsGuardExtension;

impl zed::Extension for DepsGuardExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        match language_server_id.as_ref() {
            "deps_guard" => Ok(zed::Command {
                command: "/Users/admin/.cargo/bin/deps-audit".into(),
                args: vec!["--lsp".into()],
                env: vec![],
            }),
            id => Err(format!("unknown language server: {id}")),
        }
    }
}

zed::register_extension!(DepsGuardExtension);
