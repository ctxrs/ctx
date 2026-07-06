#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum SessionStatus {
        Started => "started",
        Active => "active",
        Idle => "idle",
        Completed => "completed",
        Failed => "failed",
        Interrupted => "interrupted",
        Imported => "imported",
    }
    default Started
}

text_enum! {
    pub enum SessionEdgeType {
        ParentChild => "parent_child",
        Delegated => "delegated",
        Reviewed => "reviewed",
        Spawned => "spawned",
        ResumedFrom => "resumed_from",
        ImportedRelated => "imported_related",
    }
    default ImportedRelated
}

impl HistoryRecordLinkTargetType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Run => "run",
            Self::Event => "event",
            Self::VcsWorkspace => "vcs_workspace",
            Self::VcsChange => "vcs_change",
            Self::Artifact => "artifact",
        }
    }

    pub fn variants() -> &'static [&'static str] {
        &[
            "session",
            "run",
            "event",
            "vcs_workspace",
            "vcs_change",
            "artifact",
        ]
    }
}

impl FromStr for HistoryRecordLinkTargetType {
    type Err = CoreError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "session" => Ok(Self::Session),
            "run" => Ok(Self::Run),
            "event" => Ok(Self::Event),
            "vcs_workspace" => Ok(Self::VcsWorkspace),
            "vcs_change" => Ok(Self::VcsChange),
            "artifact" => Ok(Self::Artifact),
            _ => Err(CoreError::InvalidEnumValue {
                enum_name: "HistoryRecordLinkTargetType",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_source_id: Option<Uuid>,
    pub provider: CaptureProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_agent_id: Option<String>,
    pub agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_hint: Option<String>,
    #[serde(default)]
    pub is_primary: bool,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_blob_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEdge {
    pub id: Uuid,
    pub from_session_id: Uuid,
    pub to_session_id: Uuid,
    pub edge_type: SessionEdgeType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}
