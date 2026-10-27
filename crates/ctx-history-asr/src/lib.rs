//! Native history to Agent Session Record 0.1. No Core store or network access.
mod antigravity;
mod bundle;
mod claude;
mod codex;
mod content;
mod deepseek;
mod grok;
mod jsonl;
mod pi;
mod conversation_source;
mod gemini;
mod genai;
mod privacy;
mod qwen;
mod records;
mod source;
mod spool;
mod tools;
mod tree;

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
    Pi,
    GrokBuild,
    DeepSeekHarness,
    Gemini,
    Antigravity,
    QwenCode,
}
impl Provider {
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::GrokBuild => "grok-build",
            Self::DeepSeekHarness => "com.deepseek.harness",
            Self::Gemini => "gemini-cli",
            Self::Antigravity => "antigravity",
            Self::QwenCode => "qwen-code",
        }
    }
    pub(crate) fn domain(self) -> &'static str {
        match self {
            Self::Claude => "com.anthropic.claude-code",
            Self::Codex => "com.openai.codex",
            Self::Pi => "io.github.badlogic.pi",
            Self::GrokBuild => "ai.x.grok-build",
            Self::DeepSeekHarness => "com.deepseek.harness",
            Self::Gemini => "com.google.gemini-cli",
            Self::Antigravity => "com.google.antigravity",
            Self::QwenCode => "io.github.qwenlm.qwen-code",
        }
    }
    pub(crate) fn format(self) -> &'static str {
        match self {
            Self::Claude => "claude-code-jsonl",
            Self::Codex => "codex-rollout",
            Self::Pi => "pi-jsonl",
            Self::GrokBuild => "grok-build-acp-v1-jsonl",
            Self::DeepSeekHarness => "deepseek-harness-session-jsonl",
            Self::Gemini => "gemini-cli-chat-recording",
            Self::Antigravity => "antigravity-transcript-jsonl",
            Self::QwenCode => "qwen-code-chat-jsonl",
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
