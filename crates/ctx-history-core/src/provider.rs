use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentType, ArtifactKind, CaptureProvider, EventRole, EventType, Fidelity, RedactionState,
    SessionStatus,
};

pub const PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_SUPPORT_MATRIX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMatrixPriority {
    P0,
    P1,
    #[default]
    P2,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSupportStatus {
    LocalImport,
    LocalImportWhenSupported,
    SupportedLive,
    SupportedImport,
    SupportedWrapper,
    NormalizedImportOnly,
    FixtureOnly,
    DetectedUnsupported,
    #[default]
    Blocked,
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
    #[serde(
        rename = "openloaf",
        alias = "loaf",
        alias = "open-loaf",
        alias = "open_loaf"
    )]
    OpenLoaf,
    Cursor,
    AntigravityCli,
    GeminiCli,
    Gemini,
    CopilotCli,
    Copilot,
    #[serde(
        rename = "windsurf",
        alias = "devin_desktop",
        alias = "devin-desktop",
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
    #[serde(rename = "bob", alias = "ibm-bob", alias = "ibm_bob")]
    Bob,
    Continue,
    Crush,
    Goose,
    Dexto,
    #[serde(rename = "lingma", alias = "qoder-cn", alias = "qoder_cn")]
    Lingma,
    Qoder,
    Pochi,
    #[serde(rename = "warp")]
    Warp,
    #[serde(rename = "codebuddy", alias = "code_buddy", alias = "code-buddy")]
    CodeBuddy,
    #[serde(rename = "aider_desk", alias = "aider-desk")]
    AiderDesk,
    #[serde(rename = "trae")]
    Trae,
    #[serde(rename = "openhands")]
    OpenHands,
    Cagent,
    #[serde(rename = "qwen_code", alias = "qwen", alias = "qwen-code")]
    QwenCode,
    #[serde(rename = "autohand_code", alias = "autohand", alias = "autohand-code")]
    AutohandCode,
    #[serde(rename = "kiro_cli", alias = "kiro", alias = "kiro-cli")]
    KiroCli,
    #[serde(rename = "iflow_cli", alias = "iflow", alias = "iflow-cli")]
    IflowCli,
    #[serde(rename = "jazz")]
    Jazz,
    #[serde(
        rename = "forgecode",
        alias = "forge",
        alias = "forge-code",
        alias = "forge_code"
    )]
    ForgeCode,
    #[serde(rename = "deepagents", alias = "deep-agents", alias = "dcode")]
    DeepAgents,
    #[serde(
        rename = "mistral_vibe",
        alias = "mistral-vibe",
        alias = "mistral",
        alias = "vibe"
    )]
    MistralVibe,
    Mux,
    #[serde(rename = "reasonix", alias = "deepseek-reasonix")]
    Reasonix,
    #[serde(rename = "adal", alias = "adal-cli", alias = "adal_cli")]
    Adal,
    #[serde(
        rename = "kode",
        alias = "shareai-kode",
        alias = "shareai_kode",
        alias = "shareai_lab_kode"
    )]
    Kode,
    #[serde(rename = "neovate", alias = "neovate-code", alias = "neovate_code")]
    Neovate,
    #[serde(rename = "command_code", alias = "command-code", alias = "commandcode")]
    CommandCode,
    #[serde(
        rename = "firebender",
        alias = "firebender-jetbrains",
        alias = "firebender_jetbrains"
    )]
    Firebender,
    Terramind,
    #[serde(rename = "rovodev", alias = "rovo-dev", alias = "rovo_dev")]
    RovoDev,
    #[serde(rename = "cortex_code", alias = "cortex", alias = "cortex-code")]
    CortexCode,
    #[serde(rename = "kimi_code_cli", alias = "kimi", alias = "kimi-code-cli")]
    KimiCodeCli,
    Aider,
    ClineRoo,
    ContinueCody,
    #[serde(rename = "auggie", alias = "augment", alias = "augment-code")]
    Auggie,
    Devin,
    Eve,
    Junie,
    Kilo,
    SweAgent,
    Amp,
}

impl ProviderId {
    pub const ALL: [Self; 67] = [
        Self::Codex,
        Self::ClaudeCode,
        Self::ClaudeCliCrp,
        Self::Pi,
        Self::OpenCode,
        Self::OpenLoaf,
        Self::Cursor,
        Self::AntigravityCli,
        Self::GeminiCli,
        Self::Gemini,
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
        Self::Bob,
        Self::Continue,
        Self::Crush,
        Self::Goose,
        Self::Dexto,
        Self::Lingma,
        Self::Qoder,
        Self::Pochi,
        Self::Warp,
        Self::CodeBuddy,
        Self::AiderDesk,
        Self::Trae,
        Self::OpenHands,
        Self::Cagent,
        Self::QwenCode,
        Self::AutohandCode,
        Self::KiroCli,
        Self::IflowCli,
        Self::Jazz,
        Self::ForgeCode,
        Self::DeepAgents,
        Self::MistralVibe,
        Self::Mux,
        Self::Reasonix,
        Self::Adal,
        Self::Kode,
        Self::Neovate,
        Self::CommandCode,
        Self::Firebender,
        Self::Terramind,
        Self::RovoDev,
        Self::CortexCode,
        Self::KimiCodeCli,
        Self::Aider,
        Self::ClineRoo,
        Self::ContinueCody,
        Self::Auggie,
        Self::Devin,
        Self::Eve,
        Self::Junie,
        Self::Kilo,
        Self::SweAgent,
        Self::Amp,
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
    pub proof: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSupportEntry {
    pub id: ProviderId,
    #[serde(alias = "name", default)]
    pub display_name: String,
    #[serde(default)]
    pub priority: ProviderMatrixPriority,
    #[serde(default)]
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
    pub blockers: Vec<String>,
    #[serde(default)]
    pub public_docs: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixture_paths: Vec<String>,
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
            ProviderId::Amp,
            ProviderId::AstrBot,
            ProviderId::AutohandCode,
            ProviderId::Auggie,
            ProviderId::Bob,
            ProviderId::ClaudeCode,
            ProviderId::Cline,
            ProviderId::Codex,
            ProviderId::CodeBuddy,
            ProviderId::AiderDesk,
            ProviderId::Trae,
            ProviderId::Continue,
            ProviderId::Crush,
            ProviderId::Cursor,
            ProviderId::Windsurf,
            ProviderId::CopilotCli,
            ProviderId::Devin,
            ProviderId::Dexto,
            ProviderId::FactoryAiDroid,
            ProviderId::Firebender,
            ProviderId::ForgeCode,
            ProviderId::DeepAgents,
            ProviderId::MistralVibe,
            ProviderId::Mux,
            ProviderId::Reasonix,
            ProviderId::Adal,
            ProviderId::Terramind,
            ProviderId::GeminiCli,
            ProviderId::Goose,
            ProviderId::Hermes,
            ProviderId::Kilo,
            ProviderId::KiroCli,
            ProviderId::Jazz,
            ProviderId::Kode,
            ProviderId::KimiCodeCli,
            ProviderId::Lingma,
            ProviderId::Qoder,
            ProviderId::Pochi,
            ProviderId::Warp,
            ProviderId::Auggie,
            ProviderId::Eve,
            ProviderId::NanoClaw,
            ProviderId::Neovate,
            ProviderId::CommandCode,
            ProviderId::RovoDev,
            ProviderId::CortexCode,
            ProviderId::OpenCode,
            ProviderId::OpenLoaf,
            ProviderId::OpenClaw,
            ProviderId::OpenHands,
            ProviderId::Pi,
            ProviderId::QwenCode,
            ProviderId::RooCode,
            ProviderId::IflowCli,
            ProviderId::Shelley,
            ProviderId::Zed,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(parsed.schema_version, 1);
        assert_eq!(ids, expected);
    }

    #[test]
    fn provider_support_matrix_records_local_import_statuses() {
        let matrix = fs::read_to_string(workspace_file("docs/provider-support-matrix.json"))
            .expect("provider support matrix scaffold should exist");
        let parsed: ProviderSupportMatrixDocument =
            serde_json::from_str(&matrix).expect("matrix scaffold should parse");

        for (id, status, env_name) in [
            (
                ProviderId::Codex,
                ProviderSupportStatus::LocalImport,
                "Codex",
            ),
            (
                ProviderId::Pi,
                ProviderSupportStatus::LocalImportWhenSupported,
                "Pi",
            ),
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
