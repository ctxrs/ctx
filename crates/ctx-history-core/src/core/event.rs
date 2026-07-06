#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum EventType {
        Message => "message",
        ToolCall => "tool_call",
        ToolOutput => "tool_output",
        CommandStarted => "command_started",
        CommandOutput => "command_output",
        CommandFinished => "command_finished",
        FileTouched => "file_touched",
        VcsChange => "vcs_change",
        Artifact => "artifact",
        Summary => "summary",
        Notice => "notice",
    }
    default Notice
}

text_enum! {
    pub enum EventRole {
        User => "user",
        Assistant => "assistant",
        System => "system",
        Tool => "tool",
        Unknown => "unknown",
    }
    default Unknown
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub seq: u64,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_source_id: Option<Uuid>,
    #[serde(default = "default_metadata")]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_blob_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub redaction_state: RedactionState,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEnvelope {
    pub schema_version: u32,
    pub capture_event_id: Uuid,
    pub dedupe_key: String,
    pub source: CaptureSourceDescriptor,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_metadata")]
    pub env_session_hints: serde_json::Value,
    #[serde(default = "default_metadata")]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<String>,
    #[serde(default)]
    pub fidelity: Fidelity,
}
