//! Shared ASR identities, bounded staging, privacy and record serialization.
//! Provider acquisition and projection live in converter crates.
pub mod blobs;
pub mod content;
pub mod numbers;
pub mod privacy;
pub mod records;
pub mod source;
pub mod spool;
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
    Copilot,
    Droid,
    Kimi,
    Qoder,
    Junie,
    MistralVibe,
    Tabnine,
    Mux,
    Cursor,
    OpenCode,
    Kilo,
    MiMoCode,
    Kiro,
    Crush,
    Goose,
    Lingma,
    Warp,
    Hermes,
    AstrBot,
    Shelley,
    Zed,
    Firebender,
    ForgeCode,
    DeepAgents,
    Devin,
    OpenClaw,
    NanoClaw,
    CodeBuddy,
    Continue,
    OpenHands,
    Auggie,
    RovoDev,
    Cline,
    Roo,
    Fx,
}
impl Provider {
    pub const ALL: [Self; 43] = [
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::GrokBuild,
        Self::DeepSeekHarness,
        Self::Gemini,
        Self::Antigravity,
        Self::QwenCode,
        Self::Copilot,
        Self::Droid,
        Self::Kimi,
        Self::Qoder,
        Self::Junie,
        Self::MistralVibe,
        Self::Tabnine,
        Self::Mux,
        Self::Cursor,
        Self::OpenCode,
        Self::Kilo,
        Self::MiMoCode,
        Self::Kiro,
        Self::Crush,
        Self::Goose,
        Self::Lingma,
        Self::Warp,
        Self::Hermes,
        Self::AstrBot,
        Self::Shelley,
        Self::Zed,
        Self::Firebender,
        Self::ForgeCode,
        Self::DeepAgents,
        Self::Devin,
        Self::OpenClaw,
        Self::NanoClaw,
        Self::CodeBuddy,
        Self::Continue,
        Self::OpenHands,
        Self::Auggie,
        Self::RovoDev,
        Self::Cline,
        Self::Roo,
        Self::Fx,
    ];
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
            Self::CodeBuddy => "com.tencent.codebuddy",
            Self::Continue => "dev.continue",
            Self::OpenHands => "openhands",
            Self::Auggie => "auggie",
            Self::RovoDev => "com.atlassian.rovodev",
            Self::Cline => "cline",
            Self::Roo => "roo-code",
            Self::Fx => "com.vercel.fx",
        }
    }
    pub fn domain(self) -> &'static str {
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
            Self::MistralVibe => "ai.mistral.vibe",
            Self::Tabnine => "com.tabnine.cli",
            Self::Mux => "com.coder.mux",
            Self::Cursor => "com.cursor.agent",
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
            Self::CodeBuddy => "com.tencent.codebuddy",
            Self::Continue => "dev.continue",
            Self::OpenHands => "dev.all-hands.openhands",
            Self::Auggie => "com.augmentcode.auggie",
            Self::RovoDev => "com.atlassian.rovodev",
            Self::Cline => "bot.cline",
            Self::Roo => "com.roocode",
            Self::Fx => "com.vercel.fx",
        }
    }
    pub fn format(self) -> &'static str {
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
            Self::OpenCode => "opencode-sqlite",
            Self::Kilo => "opencode-sqlite",
            Self::MiMoCode => "opencode-sqlite",
            Self::Kiro => "kiro-cli-sqlite",
            Self::Crush => "crush-sqlite",
            Self::Goose => "goose-sqlite",
            Self::Lingma => "lingma-sqlite",
            Self::Warp => "warp-sqlite",
            Self::Hermes => "hermes-state-sqlite",
            Self::AstrBot => "astrbot-data-v4-sqlite",
            Self::Shelley => "shelley-sqlite",
            Self::Zed => "zed-threads-sqlite",
            Self::Firebender => "firebender-chat-history-sqlite",
            Self::ForgeCode => "forgecode-sqlite",
            Self::DeepAgents => "deepagents-langgraph-sqlite",
            Self::Devin => "devin-cli-sessions-sqlite",
            Self::OpenClaw => "openclaw-agent-sqlite",
            Self::NanoClaw => "nanoclaw-session-sqlite",
            Self::CodeBuddy => "codebuddy-history",
            Self::Continue => "continue-session-json",
            Self::OpenHands => "openhands-events",
            Self::Auggie => "auggie-session-json",
            Self::RovoDev => "rovodev-session-context",
            Self::Cline => "cline-task",
            Self::Roo => "cline-task",
            Self::Fx => "fx-event-log-v3",
        }
    }
    pub fn cli_journal(self) -> bool {
        matches!(self, Self::Copilot | Self::Droid | Self::Kimi | Self::Qoder)
    }
    pub fn journal(self) -> bool {
        matches!(
            self,
            Self::Junie | Self::MistralVibe | Self::Tabnine | Self::Mux | Self::Cursor
        )
    }
    pub fn is_sqlite(self) -> bool {
        matches!(
            self,
            Self::OpenCode | Self::Kilo | Self::MiMoCode | Self::Kiro
        )
    }
    pub fn sqlite(self) -> bool {
        matches!(self, Self::Crush | Self::Goose | Self::Lingma | Self::Warp)
    }
    pub fn is_relational(self) -> bool {
        matches!(
            self,
            Self::Hermes | Self::AstrBot | Self::Shelley | Self::Zed
        )
    }
    pub fn is_batch8(self) -> bool {
        matches!(
            self,
            Self::Firebender | Self::ForgeCode | Self::DeepAgents | Self::Devin
        )
    }
    pub fn mixed(self) -> bool {
        matches!(self, Self::OpenClaw | Self::NanoClaw)
    }
    pub fn batch10(self) -> bool {
        matches!(
            self,
            Self::CodeBuddy | Self::Continue | Self::OpenHands | Self::Auggie | Self::RovoDev
        )
    }
    pub fn captures(self) -> bool {
        matches!(self, Self::Cline | Self::Roo | Self::Fx)
    }
    pub fn needs_namespace(self) -> bool {
        matches!(self, Self::Cline | Self::Roo)
    }
    pub fn from_agent(agent: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.agent() == agent)
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
    /// Stable installation/profile identity for locally scoped native stores.
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
