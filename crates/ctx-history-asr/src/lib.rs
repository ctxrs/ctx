//! Native history to Agent Session Record 0.1. No Core store or network access.
mod batch10;
mod bundle;
mod claude;
mod codex;
mod content;
mod privacy;
mod records;
mod source;
mod spool;
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
    CodeBuddy,
    Continue,
    OpenHands,
    Auggie,
    RovoDev,
}
impl Provider {
    pub(crate) fn batch10(self) -> bool {
        matches!(
            self,
            Self::CodeBuddy | Self::Continue | Self::OpenHands | Self::Auggie | Self::RovoDev
        )
    }
    pub(crate) fn from_agent(agent: &str) -> Option<Self> {
        [
            Self::Claude,
            Self::Codex,
            Self::CodeBuddy,
            Self::Continue,
            Self::OpenHands,
            Self::Auggie,
            Self::RovoDev,
        ]
        .into_iter()
        .find(|p| p.agent() == agent)
    }
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::CodeBuddy => "com.tencent.codebuddy",
            Self::Continue => "dev.continue",
            Self::OpenHands => "openhands",
            Self::Auggie => "auggie",
            Self::RovoDev => "com.atlassian.rovodev",
        }
    }
    pub(crate) fn domain(self) -> &'static str {
        match self {
            Self::Claude => "com.anthropic.claude-code",
            Self::Codex => "com.openai.codex",
            Self::CodeBuddy => "com.tencent.codebuddy",
            Self::Continue => "dev.continue",
            Self::OpenHands => "dev.all-hands.openhands",
            Self::Auggie => "com.augmentcode.auggie",
            Self::RovoDev => "com.atlassian.rovodev",
        }
    }
    pub(crate) fn format(self) -> &'static str {
        match self {
            Self::Claude => "claude-code-jsonl",
            Self::Codex => "codex-rollout",
            Self::CodeBuddy => "codebuddy-history",
            Self::Continue => "continue-session-json",
            Self::OpenHands => "openhands-events",
            Self::Auggie => "auggie-session-json",
            Self::RovoDev => "rovodev-session-context",
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
    pub out: PathBuf,
    /// Bound for a native line and an assembled content field.
    pub max_record_bytes: usize,
}
impl Options {
    pub fn new(inputs: Vec<Input>, out: PathBuf) -> Self {
        Self {
            inputs,
            session: None,
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
