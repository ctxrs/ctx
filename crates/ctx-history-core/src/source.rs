use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{sync::SyncMetadata, CoreError};

text_enum! {
    pub enum CaptureSourceKind {
        ProviderImport => "provider_import",
        ProviderHook => "provider_hook",
        DirectCli => "direct_cli",
        Manual => "manual",
    }
    default Manual
}

text_enum! {
    pub enum CaptureProvider {
        Codex => "codex",
        Claude => "claude",
        Pi => "pi",
        OpenCode => "opencode",
        Kilo => "kilo",
        KiroCli => "kiro_cli",
        Antigravity => "antigravity",
        Gemini => "gemini",
        Tabnine => "tabnine",
        Cursor => "cursor",
        Windsurf => "windsurf",
        Zed => "zed",
        CopilotCli => "copilot_cli",
        FactoryAiDroid => "factory_ai_droid",
        QwenCode => "qwen_code",
        KimiCodeCli => "kimi_code_cli",
        Auggie => "auggie",
        Junie => "junie",
        Firebender => "firebender",
        ForgeCode => "forgecode",
        DeepAgents => "deepagents",
        MistralVibe => "mistral_vibe",
        Mux => "mux",
        RovoDev => "rovodev",
        OpenClaw => "openclaw",
        Hermes => "hermes",
        NanoClaw => "nanoclaw",
        AstrBot => "astrbot",
        Shelley => "shelley",
        Continue => "continue",
        OpenHands => "openhands",
        Cline => "cline",
        RooCode => "roo_code",
        Crush => "crush",
        Goose => "goose",
        Lingma => "lingma",
        Qoder => "qoder",
        Warp => "warp",
        CodeBuddy => "codebuddy",
        Trae => "trae",
        Shell => "shell",
        Git => "git",
        Jj => "jj",
        Gh => "gh",
        Custom => "custom",
        Unknown => "unknown",
    }
    default Unknown
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSourceDescriptor {
    pub kind: CaptureSourceKind,
    pub provider: CaptureProvider,
    pub machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSource {
    pub id: Uuid,
    #[serde(flatten)]
    pub descriptor: CaptureSourceDescriptor,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}
