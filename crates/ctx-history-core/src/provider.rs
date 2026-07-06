use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentType, ArtifactKind, CaptureProvider, EventRole, EventType, Fidelity, RedactionState,
    SessionStatus,
};

pub const PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSupportStatus {
    Supported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Codex,
    #[serde(alias = "claude")]
    ClaudeCode,
    ClaudeCliCrp,
    Pi,
    OpenCode,
    Cursor,
    AntigravityCli,
    GeminiCli,
    Gemini,
    CopilotCli,
    Copilot,
    #[serde(
        rename = "windsurf",
        alias = "windsurf_cascade",
        alias = "windsurf-cascade"
    )]
    Windsurf,
    Zed,
    FactoryAiDroid,
    FactoryDroid,
    DroidFactoryAi,
    #[serde(rename = "openclaw", alias = "open_claw")]
    OpenClaw,
    Hermes,
    #[serde(rename = "nanoclaw", alias = "nano_claw")]
    NanoClaw,
    #[serde(rename = "astrbot", alias = "astr_bot")]
    AstrBot,
    Shelley,
    Cline,
    #[serde(rename = "roo_code", alias = "roo", alias = "roo-code")]
    RooCode,
    Continue,
    Crush,
    Goose,
    #[serde(rename = "lingma", alias = "qoder-cn", alias = "qoder_cn")]
    Lingma,
    Qoder,
    #[serde(rename = "warp")]
    Warp,
    #[serde(rename = "codebuddy", alias = "code_buddy", alias = "code-buddy")]
    CodeBuddy,
    #[serde(rename = "trae", alias = "trae-cn", alias = "trae_cn")]
    Trae,
    #[serde(rename = "openhands")]
    OpenHands,
    Cagent,
    #[serde(rename = "qwen_code", alias = "qwen", alias = "qwen-code")]
    QwenCode,
    #[serde(rename = "kiro_cli", alias = "kiro", alias = "kiro-cli")]
    KiroCli,
    #[serde(
        rename = "forgecode",
        alias = "forge",
        alias = "forge-code",
        alias = "forge_code"
    )]
    ForgeCode,
    #[serde(rename = "deepagents", alias = "deep-agents", alias = "dcode")]
    DeepAgents,
    #[serde(rename = "mistral_vibe", alias = "mistral-vibe", alias = "mistral")]
    MistralVibe,
    #[serde(rename = "tabnine", alias = "tabnine-cli", alias = "tabnine_cli")]
    Tabnine,
    Mux,
    #[serde(
        rename = "firebender",
        alias = "firebender-jetbrains",
        alias = "firebender_jetbrains"
    )]
    Firebender,
    #[serde(rename = "rovodev", alias = "rovo-dev", alias = "rovo_dev")]
    RovoDev,
    #[serde(rename = "kimi_code_cli", alias = "kimi", alias = "kimi-code-cli")]
    KimiCodeCli,
    Aider,
    ClineRoo,
    ContinueCody,
    #[serde(rename = "auggie", alias = "augment", alias = "augment-code")]
    Auggie,
    Junie,
    Kilo,
    SweAgent,
}

impl ProviderId {
    pub const ALL: [Self; 50] = [
        Self::Codex,
        Self::ClaudeCode,
        Self::ClaudeCliCrp,
        Self::Pi,
        Self::OpenCode,
        Self::Cursor,
        Self::AntigravityCli,
        Self::GeminiCli,
        Self::Gemini,
        Self::Tabnine,
        Self::CopilotCli,
        Self::Copilot,
        Self::Windsurf,
        Self::Zed,
        Self::FactoryAiDroid,
        Self::FactoryDroid,
        Self::DroidFactoryAi,
        Self::OpenClaw,
        Self::Hermes,
        Self::NanoClaw,
        Self::AstrBot,
        Self::Shelley,
        Self::Cline,
        Self::RooCode,
        Self::Continue,
        Self::Crush,
        Self::Goose,
        Self::Lingma,
        Self::Qoder,
        Self::Warp,
        Self::CodeBuddy,
        Self::Trae,
        Self::OpenHands,
        Self::Cagent,
        Self::QwenCode,
        Self::KiroCli,
        Self::ForgeCode,
        Self::DeepAgents,
        Self::MistralVibe,
        Self::Mux,
        Self::Firebender,
        Self::RovoDev,
        Self::KimiCodeCli,
        Self::Aider,
        Self::ClineRoo,
        Self::ContinueCody,
        Self::Auggie,
        Self::Junie,
        Self::Kilo,
        Self::SweAgent,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPathKind {
    NativeImport,
    PassiveCapture,
    Wrapper,
    NormalizedImport,
    Fixture,
    Detection,
    Research,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSourceTrust {
    ProviderNative,
    ProviderExport,
    WrapperObserved,
    Fixture,
    Synthetic,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRawRetention {
    #[default]
    None,
    PathReference,
    MetadataOnly,
    LocalBlob,
    Withheld,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRedactionBoundary {
    BeforeStore,
    BeforeExport,
    #[default]
    ManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCursorCheckpoint {
    pub stream: String,
    pub cursor: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderCursorRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<ProviderCursorCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<ProviderCursorCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderFidelityClaims {
    #[serde(default)]
    pub user_prompts: bool,
    #[serde(default)]
    pub assistant_messages: bool,
    #[serde(default)]
    pub tool_calls: bool,
    #[serde(default)]
    pub tool_output: bool,
    #[serde(default)]
    pub command_output: bool,
    #[serde(default)]
    pub files_touched: bool,
    #[serde(default)]
    pub artifacts: bool,
    #[serde(default)]
    pub model_identity: bool,
    #[serde(default)]
    pub costs: bool,
    #[serde(default)]
    pub token_usage: bool,
    #[serde(default)]
    pub parent_child_session_edges: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderArtifactDescriptor {
    pub provider_artifact_id: String,
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
    #[serde(default)]
    pub redaction_state: RedactionState,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSourceEnvelope {
    pub source_format: String,
    pub machine_id: String,
    pub observed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default)]
    pub raw_retention: ProviderRawRetention,
    #[serde(default)]
    pub redaction_boundary: ProviderRedactionBoundary,
    #[serde(default)]
    pub trust: ProviderSourceTrust,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProviderCursorRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSessionEnvelope {
    pub provider_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEventEnvelope {
    pub provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default)]
    pub redaction_state: RedactionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "super::default_metadata")]
    pub payload: Value,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCaptureEnvelope {
    #[serde(default = "provider_capture_envelope_schema_version")]
    pub schema_version: u32,
    pub provider: CaptureProvider,
    pub source: ProviderSourceEnvelope,
    pub session: ProviderSessionEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<ProviderEventEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSupportPath {
    pub kind: ProviderPathKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSupportEntry {
    pub id: ProviderId,
    #[serde(alias = "name", default)]
    pub display_name: String,
    pub status: ProviderSupportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_provider: Option<CaptureProvider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implemented_paths: Vec<ProviderSupportPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_detection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_detection: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_locations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hook_options: Vec<String>,
    #[serde(default)]
    pub imports_existing_history: bool,
    #[serde(default)]
    pub captures_new_runs_passively: bool,
    #[serde(default)]
    pub child_sessions_supported: bool,
    #[serde(default)]
    pub fidelity: ProviderFidelityClaims,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redaction_notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    #[serde(default)]
    pub public_docs: String,
    #[serde(default = "super::default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSupportMatrixDocument {
    #[serde(default = "provider_support_matrix_schema_version", alias = "version")]
    pub schema_version: u32,
    pub providers: Vec<ProviderSupportEntry>,
}

pub const fn provider_capture_envelope_schema_version() -> u32 {
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION
}

pub const fn provider_support_matrix_schema_version() -> u32 {
    PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::PathBuf};

    use super::{
        ProviderCaptureEnvelope, ProviderId, ProviderSupportMatrixDocument, ProviderSupportStatus,
    };

    fn workspace_file(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }

    #[test]
    fn provider_support_matrix_scaffold_parses_current_provider_rows() {
        let matrix = fs::read_to_string(workspace_file("docs/provider-support-matrix.json"))
            .expect("provider support matrix scaffold should exist");
        let parsed: ProviderSupportMatrixDocument =
            serde_json::from_str(&matrix).expect("matrix scaffold should parse");
        let ids = parsed
            .providers
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        let expected = [
            ProviderId::AntigravityCli,
            ProviderId::AstrBot,
            ProviderId::Auggie,
            ProviderId::ClaudeCode,
            ProviderId::Cline,
            ProviderId::Codex,
            ProviderId::CodeBuddy,
            ProviderId::Trae,
            ProviderId::Continue,
            ProviderId::Crush,
            ProviderId::Cursor,
            ProviderId::Windsurf,
            ProviderId::CopilotCli,
            ProviderId::FactoryAiDroid,
            ProviderId::Firebender,
            ProviderId::ForgeCode,
            ProviderId::DeepAgents,
            ProviderId::MistralVibe,
            ProviderId::Mux,
            ProviderId::GeminiCli,
            ProviderId::Tabnine,
            ProviderId::Goose,
            ProviderId::Hermes,
            ProviderId::Kilo,
            ProviderId::KiroCli,
            ProviderId::KimiCodeCli,
            ProviderId::Lingma,
            ProviderId::Qoder,
            ProviderId::Warp,
            ProviderId::Junie,
            ProviderId::NanoClaw,
            ProviderId::RovoDev,
            ProviderId::OpenCode,
            ProviderId::OpenClaw,
            ProviderId::OpenHands,
            ProviderId::Pi,
            ProviderId::QwenCode,
            ProviderId::RooCode,
            ProviderId::Shelley,
            ProviderId::Zed,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(parsed.schema_version, 1);
        assert_eq!(ids, expected);
    }

    #[test]
    fn provider_support_matrix_records_supported_statuses() {
        let matrix = fs::read_to_string(workspace_file("docs/provider-support-matrix.json"))
            .expect("provider support matrix scaffold should exist");
        let parsed: ProviderSupportMatrixDocument =
            serde_json::from_str(&matrix).expect("matrix scaffold should parse");

        for (id, status, env_name) in [
            (ProviderId::Codex, ProviderSupportStatus::Supported, "Codex"),
            (ProviderId::Pi, ProviderSupportStatus::Supported, "Pi"),
        ] {
            let entry = parsed
                .providers
                .iter()
                .find(|entry| entry.id == id)
                .unwrap_or_else(|| panic!("missing provider row for {id:?}"));
            assert_eq!(entry.status, status, "{id:?} support status changed");
            assert_eq!(entry.display_name, env_name);
        }
    }

    #[test]
    fn provider_support_matrix_rejects_missing_or_legacy_statuses() {
        let missing_status = r#"{
          "schema_version": 1,
          "providers": [{
            "id": "codex",
            "display_name": "Codex",
            "capture_provider": "codex",
            "implemented_paths": [{"kind": "native_import", "source_format": "codex_session_jsonl"}]
          }]
        }"#;
        assert!(serde_json::from_str::<ProviderSupportMatrixDocument>(missing_status).is_err());

        let legacy_status = r#"{
          "schema_version": 1,
          "providers": [{
            "id": "codex",
            "display_name": "Codex",
            "status": "local_import_when_supported",
            "capture_provider": "codex",
            "implemented_paths": [{"kind": "native_import", "source_format": "codex_session_jsonl"}]
          }]
        }"#;
        assert!(serde_json::from_str::<ProviderSupportMatrixDocument>(legacy_status).is_err());
    }

    #[test]
    fn provider_capture_envelope_round_trips_cursor_and_redaction_fields() {
        let sample = r#"{
          "schema_version": 1,
          "provider": "codex",
          "source": {
            "source_format": "normalized_provider_fixture_jsonl",
            "machine_id": "machine-1",
            "observed_at": "2026-06-23T12:00:00Z",
            "raw_retention": "metadata_only",
            "redaction_boundary": "before_export",
            "trust": "fixture",
            "fidelity": "imported",
            "cursor": {
              "after": {
                "stream": "provider:codex:fixture",
                "cursor": "line:2",
                "observed_at": "2026-06-23T12:00:01Z"
              }
            },
            "metadata": {"source": "fixture"}
          },
          "session": {
            "provider_session_id": "codex-session-1",
            "agent_type": "primary",
            "status": "imported",
            "started_at": "2026-06-23T12:00:00Z",
            "fidelity": "imported",
            "metadata": {"model": "gpt-5-codex"}
          },
          "event": {
            "provider_event_index": 1,
            "cursor": "line:2",
            "event_type": "message",
            "role": "assistant",
            "occurred_at": "2026-06-23T12:00:01Z",
            "fidelity": "imported",
            "redaction_state": "redacted",
            "payload": {"text": "redacted preview only"},
            "metadata": {"token_usage": 42}
          }
        }"#;

        let parsed: ProviderCaptureEnvelope =
            serde_json::from_str(sample).expect("envelope should parse");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(
            parsed
                .source
                .cursor
                .as_ref()
                .and_then(|cursor| cursor.after.as_ref())
                .map(|checkpoint| checkpoint.cursor.as_str()),
            Some("line:2")
        );
        assert_eq!(
            parsed
                .event
                .as_ref()
                .map(|event| event.redaction_state.as_str()),
            Some("redacted")
        );
    }
}
