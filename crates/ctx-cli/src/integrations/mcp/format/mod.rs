use std::path::Path;

use anyhow::Result;

mod json;
mod toml;
mod yaml;

pub(super) use json::{JsonRoot, JsonServerShape};

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
