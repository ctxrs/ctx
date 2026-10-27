//! Native history to Agent Session Record 0.1. No Core store or network access.
mod antigravity;
mod batch8;
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
mod cli_journals;
mod journals;
mod kiro;
mod opencode;
mod nanoclaw;
mod native;
mod openclaw;
mod privacy;
mod qwen;
mod records;
mod relational;
mod source;
mod spool;
mod opencode_source;
mod sqlite;
mod tools;
mod tree;

pub use bundle::export;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ASR SQLite source: {0}")]
    NativeSqlite(#[from] ctx_history_source_sqlite::SqliteSourceAccessError),
    #[error("ASR source I/O: {0}")]
    SourceIo(#[from] ctx_history_source_io::SourceIoError),
    #[error("ASR native source: {0}")]
    Provider(#[from] ctx_history_provider_codex::CaptureError),
    #[error("ASR I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("ASR index: {0}")]
    Sql(#[from] ruopencode_source::Error),
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
    Copilot,
    Droid,
    Kimi,
    Qoder,
    OpenCode,
    Kilo,
    MiMoCode,
    Kiro,
    Hermes,
    AstrBot,
    Shelley,
    Zed,
    OpenClaw,
    NanoClaw,
}
impl Provider {
    pub(crate) const ALL: [Self; 20] = [Self::Claude,Self::Codex,Self::Pi,Self::GrokBuild,Self::DeepSeekHarness,Self::Gemini,Self::Antigravity,Self::QwenCode,Self::Copilot,Self::Droid,Self::Kimi,Self::Qoder,Self::OpenCode,Self::Kilo,Self::MiMoCode,Self::Kiro,Self::Hermes,Self::AstrBot,Self::Shelley,Self::Zed];
    pub(crate) fn cli_journal(self) -> bool {
        matches!(self, Self::Copilot | Self::Droid | Self::Kimi | Self::Qoder)
    Junie,
    MistralVibe,
    Tabnine,
    Mux,
    Cursor,
}
impl Provider {
    pub(crate) fn journal(self) -> bool {
        matches!(self, Self::Junie | Self::MistralVibe | Self::Tabnine | Self::Mux | Self::Cursor)
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
    Firebender,
    ForgeCode,
    DeepAgents,
    Devin,
}
impl Provider {
    pub(crate) fn is_batch8(self) -> bool {
        matches!(
            self,
            Self::Firebender | Self::ForgeCode | Self::DeepAgents | Self::Devin
        )
    }
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
            Self::Copilot => "copilot-cli",
            Self::Droid => "droid",
            Self::Kimi => "com.moonshot.kimi-code",
            Self::Qoder => "com.qoder.cli",
            Self::Junie => "junie",
            Self::MistralVibe => "ai.mistral.vibe",
            Self::Tabnine => "com.tabnine.cli",
            Self::Mux => "com.coder.mux",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Kilo => "kilo",
            Self::MiMoCode => "com.xiaomi.mimocode",
            Self::Kiro => "kiro",
            Self::Crush => "crush",
            Self::Goose => "goose",
            Self::Lingma => "com.aliyun.lingma",
            Self::Warp => "warp",
            Self::Hermes => "com.nousresearch.hermes",
            Self::AstrBot => "app.astrbot",
            Self::Shelley => "dev.exe.shelley",
            Self::Zed => "zed",
            Self::Firebender => "com.firebender",
            Self::ForgeCode => "dev.forgecode",
            Self::DeepAgents => "com.langchain.deepagents",
            Self::Devin => "ai.cognition.devin",
            Self::OpenClaw => "openclaw",
            Self::NanoClaw => "dev.nanoclaw",
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
            Self::Copilot => "com.github.copilot-cli",
            Self::Droid => "ai.factory.droid",
            Self::Kimi => "com.moonshot.kimi-code",
            Self::Qoder => "com.qoder.cli",
            Self::Junie => "com.jetbrains.junie",
            Self::Cursor => "com.cursor.agent",
            Self::MistralVibe | Self::Tabnine | Self::Mux => self.agent(),
            Self::OpenCode => "ai.opencode",
            Self::Kilo => "ai.kilocode",
            Self::MiMoCode => "com.xiaomi.mimocode",
            Self::Kiro => "dev.kiro.cli",
            Self::Crush => "sh.charm.crush",
            Self::Goose => "xyz.block.goose",
            Self::Lingma => "com.aliyun.lingma",
            Self::Warp => "dev.warp.agent",
            Self::Hermes => "com.nousresearch.hermes",
            Self::AstrBot => "app.astrbot",
            Self::Shelley => "dev.exe.shelley",
            Self::Zed => "dev.zed",
            Self::Firebender => "com.firebender",
            Self::ForgeCode => "dev.forgecode",
            Self::DeepAgents => "com.langchain.deepagents",
            Self::Devin => "ai.cognition.devin",
            Self::OpenClaw => "ai.openclaw",
            Self::NanoClaw => "dev.nanoclaw",
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
            Self::Copilot => "copilot-cli-events",
            Self::Droid => "factory-droid-jsonl",
            Self::Kimi => "kimi-code-wire-jsonl",
            Self::Qoder => "qoder-transcript-jsonl",
            Self::Junie => "junie-events-jsonl",
            Self::MistralVibe => "mistral-vibe-session",
            Self::Tabnine => "tabnine-chat-jsonl",
            Self::Mux => "mux-chat-jsonl",
            Self::Cursor => "cursor-cli-transcript",
            Self::OpenCode | Self::Kilo | Self::MiMoCode => "opencode-sqlite",
            Self::Kiro => "kiro-cli-sqlite",
            Self::Crush => "crush-sqlite",
            Self::Goose => "goose-sqlite",
            Self::Lingma => "lingma-sqlite",
            Self::Warp => "warp-sqlite",
            Self::Firebender => "firebender-chat-history-sqlite",
            Self::ForgeCode => "forgecode-sqlite",
            Self::DeepAgents => "deepagents-langgraph-sqlite",
            Self::Devin => "devin-cli-sessions-sqlite",
        }
    }
    pub fn is_sqlite(self) -> bool {
        matches!(
            self,
            Self::OpenCode | Self::Kilo | Self::MiMoCode | Self::Kiro
        )
    }
    pub(crate) fn from_agent(agent: &str) -> Result<Self> {
        [
            Self::Claude,
            Self::Codex,
            Self::OpenCode,
            Self::Kilo,
            Self::MiMoCode,
            Self::Kiro,
        ]
        .into_iter()
        .find(|p| p.agent() == agent)
        .ok_or_else(|| Error::Invalid("unknown ASR provider".into()))
    }
            Self::Hermes => "hermes-state-sqlite",
            Self::AstrBot => "astrbot-data-v4-sqlite",
            Self::Shelley => "shelley-sqlite",
            Self::Zed => "zed-threads-sqlite",
        }
    }
    pub fn is_relational(self) -> bool {
        matches!(
            self,
            Self::Hermes | Self::AstrBot | Self::Shelley | Self::Zed
        )
            Self::OpenClaw => "openclaw-agent-sqlite",
            Self::NanoClaw => "nanoclaw-session-sqlite",
        }
    }
    pub(crate) fn mixed(self) -> bool {
        matches!(self, Self::OpenClaw | Self::NanoClaw)
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
    /// Stable installation identity, required for directory-keyed Kiro stores.
    /// Stable installation/profile identity required for Goose local session counters.
    /// Stable installation namespace for native stores that do not record one.
    pub namespace: Option<String>,
    pub out: PathBuf,
    /// Bound for a native line, SQLite row, and assembled content field.
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
