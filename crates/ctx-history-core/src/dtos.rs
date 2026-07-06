use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    new_id,
    redaction::RedactionState,
    source::CaptureProvider,
    sync::{default_metadata, EntityTimestamps, SyncMetadata},
    utc_now, CoreError,
};

text_enum! {
    pub enum Confidence {
        Explicit => "explicit",
        High => "high",
        Medium => "medium",
        Low => "low",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum HistoryRecordStatus {
        Open => "open",
        Active => "active",
        Completed => "completed",
        Abandoned => "abandoned",
        Archived => "archived",
    }
    default Open
}

text_enum! {
    pub enum AgentType {
        Primary => "primary",
        Subagent => "subagent",
        AgentTeamMember => "agent_team_member",
        Reviewer => "reviewer",
        Implementer => "implementer",
        Unknown => "unknown",
    }
    default Unknown
}

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

text_enum! {
    pub enum RunType {
        AgentTurn => "agent_turn",
        Command => "command",
        ToolCall => "tool_call",
        Review => "review",
        Import => "import",
        Summary => "summary",
    }
    default Command
}

text_enum! {
    pub enum RunStatus {
        Queued => "queued",
        Running => "running",
        Succeeded => "succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
        Partial => "partial",
    }
    default Queued
}

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

text_enum! {
    pub enum VcsKind {
        Git => "git",
        Jj => "jj",
    }
    default Git
}

text_enum! {
    pub enum VcsHost {
        Github => "github",
        Gitlab => "gitlab",
        Bitbucket => "bitbucket",
        Local => "local",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum VcsChangeKind {
        GitCommit => "git_commit",
        GitBranch => "git_branch",
        GitWorktree => "git_worktree",
        JjChange => "jj_change",
        JjBookmark => "jj_bookmark",
        Patch => "patch",
        WorkingCopy => "working_copy",
    }
    default WorkingCopy
}

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

impl fmt::Display for HistoryRecordLinkTargetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

impl Serialize for HistoryRecordLinkTargetType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HistoryRecordLinkTargetType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryRecordLinkType {
    Produced,
    Touched,
    #[default]
    References,
    LikelyRelated,
}

impl HistoryRecordLinkType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Touched => "touched",
            Self::References => "references",
            Self::LikelyRelated => "likely_related",
        }
    }

    pub fn variants() -> &'static [&'static str] {
        &["produced", "touched", "references", "likely_related"]
    }
}

impl fmt::Display for HistoryRecordLinkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HistoryRecordLinkType {
    type Err = CoreError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "produced" => Ok(Self::Produced),
            "touched" => Ok(Self::Touched),
            "references" => Ok(Self::References),
            "likely_related" => Ok(Self::LikelyRelated),
            _ => Err(CoreError::InvalidEnumValue {
                enum_name: "HistoryRecordLinkType",
                value: value.to_owned(),
            }),
        }
    }
}

impl Serialize for HistoryRecordLinkType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HistoryRecordLinkType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
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
    pub enum SummaryKind {
        ImportedProviderSummary => "imported_provider_summary",
        CtxGenerated => "ctx_generated",
        AgentSupplied => "agent_supplied",
        HumanNote => "human_note",
    }
    default HumanNote
}

text_enum! {
    pub enum FileChangeKind {
        Read => "read",
        Created => "created",
        Modified => "modified",
        Deleted => "deleted",
        Renamed => "renamed",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum TagKind {
        User => "user",
        System => "system",
        Inferred => "inferred",
    }
    default User
}

text_enum! {
    pub enum RecordEdgeType {
        Continues => "continues",
        Duplicates => "duplicates",
        Blocks => "blocks",
        Related => "related",
        Supersedes => "supersedes",
        SplitFrom => "split_from",
    }
    default Related
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub kind: String,
    pub workspace: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HistoryRecord {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
        kind: impl Into<String>,
        workspace: Option<String>,
    ) -> Self {
        let now = utc_now();
        Self {
            id: new_id(),
            title: title.into(),
            body: body.into(),
            tags,
            kind: kind.into(),
            workspace,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordMetadata {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: HistoryRecordStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_vcs_workspace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub run_type: RunType,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_blob_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_blob_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
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
pub struct VcsWorkspace {
    pub id: Uuid,
    pub kind: VcsKind,
    pub root_path: String,
    pub repo_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_remote_url_normalized: Option<String>,
    #[serde(default)]
    pub host: VcsHost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monorepo_subpath: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VcsChange {
    pub id: Uuid,
    pub vcs_workspace_id: Uuid,
    pub kind: VcsChangeKind,
    pub change_id: String,
    #[serde(default)]
    pub parent_change_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_or_bookmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordLink {
    pub id: Uuid,
    #[serde(rename = "history_record_id")]
    pub history_record_id: Uuid,
    pub target_type: HistoryRecordLinkTargetType,
    pub target_id: Uuid,
    pub link_type: HistoryRecordLinkType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitationReference {
    pub target_type: HistoryRecordLinkTargetType,
    pub target_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub kind: SummaryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_or_source: Option<String>,
    pub text: String,
    #[serde(default)]
    pub citations: Vec<CitationReference>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileTouched {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_workspace_id: Option<Uuid>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count_delta: Option<i64>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub kind: TagKind,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecordTag {
    #[serde(rename = "history_record_id")]
    pub history_record_id: Uuid,
    pub tag_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default)]
    pub confidence: Confidence,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordEdge {
    pub id: Uuid,
    pub from_record_id: Uuid,
    pub to_record_id: Uuid,
    pub edge_type: RecordEdgeType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCitation {
    #[serde(rename = "type")]
    pub citation_type: ContextCitationType,
    pub id: Uuid,
    pub label: String,
    pub time: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<CaptureProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextLinks {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPagination {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTruncation {
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub omitted_results: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
