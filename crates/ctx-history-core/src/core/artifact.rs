#[allow(unused_imports)]
use super::*;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryRecordLinkTargetType {
    Session,
    Run,
    #[default]
    Event,
    VcsWorkspace,
    VcsChange,
    Artifact,
}

text_enum! {
    pub enum ArtifactKind {
        Transcript => "transcript",
        Stdout => "stdout",
        Stderr => "stderr",
        Screenshot => "screenshot",
        Report => "report",
        Diff => "diff",
        FileSnapshot => "file_snapshot",
        Json => "json",
        Markdown => "markdown",
        Binary => "binary",
    }
    default Binary
}

text_enum! {
    pub enum ContextCitationType {
        HistoryRecord => "history_record",
        Session => "session",
        Run => "run",
        Event => "event",
        VcsChange => "vcs_change",
        Artifact => "artifact",
        Summary => "summary",
        File => "file",
    }
    default Event
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Uuid,
    pub kind: ArtifactKind,
    pub blob_hash: String,
    pub blob_path: String,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    #[serde(default)]
    pub redaction_state: RedactionState,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}
