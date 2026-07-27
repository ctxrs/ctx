use std::path::Path;

use anyhow::Result;

use super::{SERVER_ARGS, SERVER_COMMAND};

mod json;
mod toml;
mod yaml;

pub(super) use json::{JsonRoot, JsonServerShape};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ServerCommand<'a> {
    executable: &'a str,
    args: &'a [&'a str],
}

impl<'a> ServerCommand<'a> {
    pub(super) const fn new(executable: &'a str, args: &'a [&'a str]) -> Self {
        Self { executable, args }
    }

    pub(super) const fn executable(self) -> &'a str {
        self.executable
    }

    pub(super) const fn args(self) -> &'a [&'a str] {
        self.args
    }

    pub(super) fn argv(self) -> Vec<&'a str> {
        std::iter::once(self.executable)
            .chain(self.args.iter().copied())
            .collect()
    }

    pub(super) fn render_for_host(self) -> String {
        self.render(CommandShell::host())
    }

    fn render(self, shell: CommandShell) -> String {
        let argv = self.argv();
        match shell {
            CommandShell::Posix => argv
                .into_iter()
                .map(quote_posix_arg)
                .collect::<Vec<_>>()
                .join(" "),
            CommandShell::PowerShell => std::iter::once("&".to_owned())
                .chain(argv.into_iter().map(quote_powershell_arg))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

pub(super) const fn server_command() -> ServerCommand<'static> {
    ServerCommand::new(SERVER_COMMAND, SERVER_ARGS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandShell {
    Posix,
    PowerShell,
}

impl CommandShell {
    const fn host() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }
}

fn quote_posix_arg(value: &str) -> String {
    if is_bare_arg(value) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn quote_powershell_arg(value: &str) -> String {
    if is_bare_arg(value) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

fn is_bare_arg(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ConfigKind {
    CodexToml,
    GooseYaml,
    ContinueYaml,
    Json {
        root: JsonRoot,
        server: JsonServerShape,
    },
}

impl ConfigKind {
    pub(super) fn opencode_json() -> Self {
        Self::Json {
            root: JsonRoot::Mcp,
            server: JsonServerShape::OpenCodeLocal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigStatus {
    Current,
    Missing,
    Conflict,
    Invalid,
    Unsupported,
}

impl ConfigStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Missing => "missing",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid_config",
            Self::Unsupported => "unsupported",
        }
    }
}

pub(super) fn status(body: &str, kind: ConfigKind, path: &Path) -> Result<ConfigStatus> {
    match kind {
        ConfigKind::CodexToml => toml::status(body),
        ConfigKind::GooseYaml => yaml::status_goose(body),
        ConfigKind::ContinueYaml => yaml::status_continue(body),
        ConfigKind::Json { root, server } => json::status(body, root, server, path),
    }
}

pub(super) fn upsert(body: &str, kind: ConfigKind, force: bool, path: &Path) -> Result<String> {
    match kind {
        ConfigKind::CodexToml => toml::upsert(body, force),
        ConfigKind::GooseYaml => yaml::upsert_goose(body, force),
        ConfigKind::ContinueYaml => yaml::upsert_continue(body, force),
        ConfigKind::Json { root, server } => json::upsert(body, root, server, force, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_rendering_preserves_structured_argv() {
        let command = ServerCommand::new(
            r"C:\Program Files\ctx & tools\ctx-雪.exe",
            &[
                "",
                "two words",
                "$env:TEMP; Write-Output nope | Out-Null",
                "O'Brien",
                "%PATH% ^ !",
            ],
        );

        assert_eq!(
            command.argv(),
            vec![
                r"C:\Program Files\ctx & tools\ctx-雪.exe",
                "",
                "two words",
                "$env:TEMP; Write-Output nope | Out-Null",
                "O'Brien",
                "%PATH% ^ !",
            ]
        );
        assert_eq!(
            command.render(CommandShell::PowerShell),
            r"& 'C:\Program Files\ctx & tools\ctx-雪.exe' '' 'two words' '$env:TEMP; Write-Output nope | Out-Null' 'O''Brien' '%PATH% ^ !'"
        );
    }

    #[test]
    fn posix_rendering_preserves_structured_argv() {
        let command = ServerCommand::new(
            "/opt/ctx tools/ctx-雪",
            &["", "two words", "$(touch /tmp/nope);&|<>", "O'Brien"],
        );

        assert_eq!(
            command.argv(),
            vec![
                "/opt/ctx tools/ctx-雪",
                "",
                "two words",
                "$(touch /tmp/nope);&|<>",
                "O'Brien",
            ]
        );
        assert_eq!(
            command.render(CommandShell::Posix),
            r"'/opt/ctx tools/ctx-雪' '' 'two words' '$(touch /tmp/nope);&|<>' 'O'\''Brien'"
        );
    }

    #[test]
    fn standard_server_command_stays_compact() {
        assert_eq!(
            server_command().render(CommandShell::Posix),
            "ctx mcp serve"
        );
        assert_eq!(
            server_command().render(CommandShell::PowerShell),
            "& ctx mcp serve"
        );
        assert_eq!(
            server_command().render_for_host(),
            if cfg!(windows) {
                "& ctx mcp serve"
            } else {
                "ctx mcp serve"
            }
        );
    }
}
