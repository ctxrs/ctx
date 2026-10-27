//! Native history to Agent Session Record 0.1. No Core store or network access.
mod bundle;
mod claude;
mod codex;
mod content;
mod privacy;
mod records;
mod source;
mod spool;
mod sqlite;
mod tools;

pub use bundle::export;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ASR native source: {0}")]
    Provider(#[from] ctx_history_provider_codex::CaptureError),
    #[error("ASR I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("ASR index: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("ASR JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Claude,
    Codex,
    Crush,
    Goose,
    Lingma,
    Warp,
}
impl Provider {
    pub(crate) fn sqlite(self) -> bool {
        matches!(self, Self::Crush | Self::Goose | Self::Lingma | Self::Warp)
    }
    pub(crate) fn from_agent(agent: &str) -> Option<Self> {
        [
            Self::Claude,
            Self::Codex,
            Self::Crush,
            Self::Goose,
            Self::Lingma,
            Self::Warp,
        ]
        .into_iter()
        .find(|p| p.agent() == agent)
    }
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Crush => "crush",
            Self::Goose => "goose",
            Self::Lingma => "com.aliyun.lingma",
            Self::Warp => "warp",
        }
    }
    pub(crate) fn domain(self) -> &'static str {
        match self {
            Self::Claude => "com.anthropic.claude-code",
            Self::Codex => "com.openai.codex",
            Self::Crush => "sh.charm.crush",
            Self::Goose => "xyz.block.goose",
            Self::Lingma => "com.aliyun.lingma",
            Self::Warp => "dev.warp.agent",
        }
    }
    pub(crate) fn format(self) -> &'static str {
        match self {
            Self::Claude => "claude-code-jsonl",
            Self::Codex => "codex-rollout",
            Self::Crush => "crush-sqlite",
            Self::Goose => "goose-sqlite",
            Self::Lingma => "lingma-sqlite",
            Self::Warp => "warp-sqlite",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Input {
    pub provider: Provider,
    pub path: PathBuf,
}
#[derive(Debug, Clone)]
pub struct Options {
    pub inputs: Vec<Input>,
    /// Exact native session ID; absent exports all selected sessions.
    pub session: Option<String>,
    /// Stable installation/profile identity required for Goose local session counters.
    pub namespace: Option<String>,
    pub out: PathBuf,
    /// Bound for a native line and an assembled content field.
    pub max_record_bytes: usize,
}
impl Options {
    pub fn new(inputs: Vec<Input>, out: PathBuf) -> Self {
        Self {
            inputs,
            session: None,
            namespace: None,
            out,
            max_record_bytes: ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES,
        }
    }
}
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub sessions: u64,
    pub unchanged: u64,
    pub snapshots: u64,
    pub records: u64,
    pub source_bytes: u64,
    pub output_bytes: u64,
    pub gaps: u64,
    pub unidentified_sources: u64,
}

#[cfg(test)]
mod tests;
