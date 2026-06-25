use std::{env, fmt, path::PathBuf, str::FromStr, sync::OnceLock};

use chrono::{DateTime, Utc};
use directories::BaseDirs;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("could not determine a home directory for the default ctx data root")]
    MissingHome,
    #[error("invalid {enum_name} value: {value}")]
    InvalidEnumValue {
        enum_name: &'static str,
        value: String,
    },
}

pub type Result<T> = std::result::Result<T, CoreError>;

macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
        default $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn variants() -> &'static [&'static str] {
                &[$($value),+]
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(CoreError::InvalidEnumValue {
                        enum_name: stringify!($name),
                        value: value.to_owned(),
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

mod provider;

pub use provider::*;

text_enum! {
    pub enum Visibility {
        LocalOnly => "local_only",
        Reportable => "reportable",
        SyncMetadata => "sync_metadata",
        SyncFull => "sync_full",
        Withheld => "withheld",
    }
    default LocalOnly
}

text_enum! {
    pub enum Fidelity {
        Full => "full",
        Partial => "partial",
        Imported => "imported",
        Inferred => "inferred",
        SummaryOnly => "summary_only",
    }
    default Partial
}

text_enum! {
    pub enum SyncState {
        LocalOnly => "local_only",
        Pending => "pending",
        Synced => "synced",
        Failed => "failed",
        Withheld => "withheld",
    }
    default LocalOnly
}

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
    pub enum RedactionState {
        Raw => "raw",
        Redacted => "redacted",
        SafePreview => "safe_preview",
        Withheld => "withheld",
    }
    default SafePreview
}

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
        Antigravity => "antigravity",
        Gemini => "gemini",
        Cursor => "cursor",
        CopilotCli => "copilot_cli",
        FactoryAiDroid => "factory_ai_droid",
        Shell => "shell",
        Git => "git",
        Jj => "jj",
        Gh => "gh",
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
    pub enum SyncDirection {
        Upload => "upload",
        Download => "download",
    }
    default Upload
}

text_enum! {
    pub enum SyncBatchStatus {
        Pending => "pending",
        Running => "running",
        Succeeded => "succeeded",
        Failed => "failed",
    }
    default Pending
}

text_enum! {
    pub enum SyncOutboxOperation {
        Insert => "insert",
        Update => "update",
        Delete => "delete",
        BlobUpload => "blob_upload",
    }
    default Insert
}

text_enum! {
    pub enum AuditActorKind {
        Human => "human",
        Agent => "agent",
        System => "system",
    }
    default System
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
        let now = Utc::now();
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
pub struct SessionHistoryArchive {
    #[serde(default = "legacy_archive_schema_version")]
    pub schema_version: u32,
    #[serde(default = "legacy_archive_schema_version")]
    pub version: u32,
    pub records: Vec<HistoryRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_sources: Vec<CaptureSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<Run>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Event>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_records: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcs_workspaces: Vec<VcsWorkspace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcs_changes: Vec<VcsChange>,
    #[serde(
        default,
        rename = "history_record_links",
        alias = "work_record_links",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub history_record_links: Vec<HistoryRecordLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<Summary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<FileTouched>,
}

impl Default for SessionHistoryArchive {
    fn default() -> Self {
        Self {
            schema_version: archive_schema_version(),
            version: archive_schema_version(),
            records: Vec::new(),
            capture_sources: Vec::new(),
            sessions: Vec::new(),
            runs: Vec::new(),
            events: Vec::new(),
            artifact_records: Vec::new(),
            vcs_workspaces: Vec::new(),
            vcs_changes: Vec::new(),
            history_record_links: Vec::new(),
            summaries: Vec::new(),
            files_touched: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncMetadata {
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default)]
    pub sync_state: SyncState,
    #[serde(default)]
    pub sync_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

impl Default for SyncMetadata {
    fn default() -> Self {
        Self {
            visibility: Visibility::default(),
            fidelity: Fidelity::default(),
            sync_state: SyncState::default(),
            sync_version: 0,
            deleted_at: None,
            metadata: default_metadata(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTimestamps {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
        alias = "work_record_id",
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
        alias = "work_record_id",
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
        alias = "work_record_id",
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
    #[serde(rename = "history_record_id", alias = "work_record_id")]
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
        alias = "work_record_id",
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
        alias = "work_record_id",
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
    #[serde(rename = "history_record_id", alias = "work_record_id")]
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
pub struct SyncAlias {
    pub id: Uuid,
    pub local_table: String,
    pub local_id: String,
    pub hosted_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCursor {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    pub stream: String,
    pub cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncBatch {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    pub direction: SyncDirection,
    pub status: SyncBatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOutboxItem {
    pub id: Uuid,
    pub local_table: String,
    pub local_id: String,
    pub operation: SyncOutboxOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    #[serde(default = "default_pending_sync_state")]
    pub sync_state: SyncState,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default = "default_metadata")]
    pub payload: serde_json::Value,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub actor_kind: AuditActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_table: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
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

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_pending_sync_state() -> SyncState {
    SyncState::Pending
}

fn archive_schema_version() -> u32 {
    2
}

fn legacy_archive_schema_version() -> u32 {
    1
}

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

pub fn default_data_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DATA_ROOT") {
        return Ok(PathBuf::from(value));
    }

    let base = BaseDirs::new().ok_or(CoreError::MissingHome)?;
    Ok(base.home_dir().join(".ctx"))
}

pub fn history_dir(root: PathBuf) -> PathBuf {
    root
}

pub fn database_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("work.sqlite")
}

pub fn object_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("objects")
}

pub fn blob_dir(root: PathBuf) -> PathBuf {
    object_dir(root)
}

pub fn spool_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("spool")
}

pub fn inbox_dir(root: PathBuf) -> PathBuf {
    spool_dir(root)
}

pub fn config_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("config.toml")
}

pub fn logs_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("logs")
}

pub fn device_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("device.json")
}

pub fn redact_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    for ch in text.chars().take(max_chars) {
        preview.push(ch);
    }
    redact_secret_markers(&preview)
}

pub fn redact_share_safe_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    for ch in text.chars().take(max_chars) {
        preview.push(ch);
    }
    redact_share_safe_markers(&preview)
}

pub fn redact_share_safe_markers(text: &str) -> String {
    redact_local_paths(&redact_secret_markers(text))
}

pub fn redact_secret_markers(text: &str) -> String {
    let mut value = text.to_owned();
    if let Some(regex) = database_url_password_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]@")
            .into_owned();
    }
    if let Some(regex) = credentialed_url_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_CREDENTIAL]@")
            .into_owned();
    }
    if let Some(regex) = email_assignment_regex() {
        value = regex.replace_all(&value, "$1[REDACTED_EMAIL]").into_owned();
    }
    if let Some(regex) = authorization_bearer_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    if let Some(regex) = bearer_token_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    for regex in standalone_secret_regexes() {
        value = regex.replace_all(&value, "[REDACTED_SECRET]").into_owned();
    }
    if let Some(regex) = secret_assignment_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    if let Some(regex) = password_phrase_regex() {
        value = regex
            .replace_all(&value, "$1[REDACTED_SECRET]")
            .into_owned();
    }
    value
}

fn redact_local_paths(text: &str) -> String {
    let mut value = text.to_owned();
    if let Some(regex) = private_path_prefix_regex() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    for regex in local_path_regexes() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    value
}

fn secret_assignment_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:api[_-]?key|access[_-]?key|access[_-]?token|auth[_-]?token|token|secret|password|passwd|pwd)\s*[:=]\s*)([^\s,;"']{3,})"#,
            )
            .ok()
        })
        .as_ref()
}

fn credentialed_url_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b((?:https?|ssh|git)://)[^/\s:@\[]+:[^/\s@\[]+@"#).ok())
        .as_ref()
}

fn database_url_password_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:postgres|postgresql|mysql|mariadb|mongodb|redis)://[^/\s:@]+:)[^/\s@]+@"#,
            )
            .ok()
        })
        .as_ref()
}

fn email_assignment_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:customer[_-]?email|email)\s*[:=]\s*)[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b"#,
            )
            .ok()
        })
        .as_ref()
}

fn bearer_token_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{12,}\b").ok())
        .as_ref()
}

fn authorization_bearer_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(r"(?i)\b(authorization\s*:\s*bearer\s+)[A-Za-z0-9._~+/=-]{3,}\b").ok()
        })
        .as_ref()
}

fn password_phrase_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r#"(?i)\b(password\s+)[^\s,;"']{6,}"#).ok())
        .as_ref()
}

fn private_path_prefix_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)(^|[\s"'(=\[])(/(?:home|Users)/[^\s/,;"'<>)\]]+/(?:src|code|work|repo|repos)/[^\s/,;"'<>)\]]*secret[^\s/,;"'<>)\]]*)"#,
            )
            .ok()
        })
        .as_ref()
}

fn standalone_secret_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{12,}\b",
                r"\bgh[pousr]_[A-Za-z0-9_]{16,}\b",
                r"\bAKIA[0-9A-Z]{16}\b",
            ]
            .into_iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
        })
        .as_slice()
}

fn local_path_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r#"(^|[\s"'(=\[])(/(?:home|Users|tmp|var/tmp|private/tmp|Volumes|mnt|workspace|workspaces|repo|repos|code)(?:/[^\s,;"'<>)\]]*)?)"#,
                r#"(^|[\s"'(=\[])(/(?:[A-Za-z0-9._-]+/)+[^\s,;"'<>)\]]*)"#,
                r#"(?i)(^|[\s"'(=\[])(?:[A-Z]:\\|\\\\)[^\s,;"'<>)\]]+"#,
            ]
            .into_iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enum_string_roundtrips_and_defaults() {
        let visibility: Visibility = serde_json::from_str("\"sync_metadata\"").unwrap();
        assert_eq!(visibility, Visibility::SyncMetadata);
        assert_eq!(visibility.to_string(), "sync_metadata");
        assert_eq!(
            serde_json::to_string(&Visibility::Withheld).unwrap(),
            "\"withheld\""
        );
        assert!("not_valid".parse::<Visibility>().is_err());

        assert_eq!(Visibility::default(), Visibility::LocalOnly);
        assert_eq!(Fidelity::default(), Fidelity::Partial);
        assert_eq!(SyncState::default(), SyncState::LocalOnly);
        assert_eq!(Confidence::default(), Confidence::Unknown);
        assert_eq!(RedactionState::default(), RedactionState::SafePreview);
        assert_eq!(
            serde_json::from_str::<CaptureProvider>("\"copilot_cli\"").unwrap(),
            CaptureProvider::CopilotCli
        );
        assert_eq!(
            serde_json::from_str::<CaptureProvider>("\"factory_ai_droid\"").unwrap(),
            CaptureProvider::FactoryAiDroid
        );

        let sync: SyncMetadata = serde_json::from_value(json!({})).unwrap();
        assert_eq!(sync.visibility, Visibility::LocalOnly);
        assert_eq!(sync.fidelity, Fidelity::Partial);
        assert_eq!(sync.sync_state, SyncState::LocalOnly);
        assert_eq!(sync.sync_version, 0);
        assert_eq!(sync.metadata, json!({}));

        let outbox: SyncOutboxItem = serde_json::from_value(json!({
            "id": "018f45d0-0000-7000-8000-000000000010",
            "local_table": "work_records",
            "local_id": "018f45d0-0000-7000-8000-000000000001",
            "operation": "insert",
            "device_id": "device-1",
            "created_at": "2026-06-22T00:00:00Z",
            "updated_at": "2026-06-22T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(outbox.sync_state, SyncState::Pending);
    }

    #[test]
    fn history_record_json_names_accept_legacy_aliases() {
        let record_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000001").unwrap();
        let session: Session = serde_json::from_value(json!({
            "id": "018f45d0-0000-7000-8000-000000000002",
            "work_record_id": record_id,
            "provider": "codex",
            "agent_type": "primary",
            "status": "imported",
            "started_at": "2026-06-22T00:00:00Z",
            "created_at": "2026-06-22T00:00:00Z",
            "updated_at": "2026-06-22T00:00:00Z"
        }))
        .unwrap();

        assert_eq!(session.history_record_id, Some(record_id));
        let value = serde_json::to_value(&session).unwrap();
        assert_eq!(value["history_record_id"], record_id.to_string());
        assert!(value.get("work_record_id").is_none());
        assert_eq!(
            serde_json::to_string(&ContextCitationType::HistoryRecord).unwrap(),
            "\"history_record\""
        );
    }

    #[test]
    fn redacts_common_secret_markers() {
        let redacted = redact_secret_markers(
            "token=ghp_1234567890abcdef password=hunter2 secret=shhh \
             bearer abcdef1234567890 AKIA1234567890ABCDEF sk-abcdefghijklmnop",
        );

        assert!(redacted.contains("token=[REDACTED_SECRET]"));
        assert!(redacted.contains("password=[REDACTED_SECRET]"));
        assert!(redacted.contains("secret=[REDACTED_SECRET]"));
        assert_eq!(redacted.matches("[REDACTED_SECRET]").count(), 6);
        assert!(!redacted.contains("ghp_123456"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("shhh"));
        assert!(!redacted.contains("AKIA1234567890ABCDEF"));
        assert!(!redacted.contains("sk-abcdefghijklmnop"));
    }

    #[test]
    fn share_safe_redaction_hides_local_paths() {
        let redacted = redact_share_safe_markers(
            "cwd=/home/example/code/project tmp=/tmp/work ci=/var/lib/buildkite-agent/builds/project token=ghp_1234567890abcdef",
        );

        assert!(redacted.contains("cwd=[REDACTED_PATH]"));
        assert!(redacted.contains("tmp=[REDACTED_PATH]"));
        assert!(redacted.contains("ci=[REDACTED_PATH]"));
        assert!(redacted.contains("token=[REDACTED_SECRET]"));
        assert!(!redacted.contains("/home/example/code/project"));
        assert!(!redacted.contains("/tmp/work"));
        assert!(!redacted.contains("/var/lib/buildkite-agent/builds/project"));
        assert!(!redacted.contains("ghp_123456"));
    }

    #[test]
    fn redaction_corpus_matches_share_safe_helpers() {
        let corpus = include_str!("../../../tests/fixtures/redaction/redaction-corpus.jsonl");
        for (index, line) in corpus.lines().enumerate() {
            let case: serde_json::Value = serde_json::from_str(line).unwrap();
            let input = case["input"].as_str().unwrap();
            let expected = case["expected_redacted"].as_str().unwrap();

            assert_eq!(
                redact_share_safe_markers(input),
                expected,
                "redaction corpus line {} ({})",
                index + 1,
                case["id"].as_str().unwrap()
            );
            assert_eq!(
                redact_share_safe_preview(input, input.chars().count()),
                expected,
                "share-safe preview corpus line {} ({})",
                index + 1,
                case["id"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn generated_ids_are_uuid_v7_and_paths_are_centralized() {
        let record = HistoryRecord::new("Task", "body", Vec::new(), "task", None);

        assert_eq!(record.id.get_version_num(), 7);
    }

    #[test]
    fn local_layout_paths_are_flat_under_data_root() {
        let root = PathBuf::from("/tmp/ctx-root");
        assert_eq!(history_dir(root.clone()), PathBuf::from("/tmp/ctx-root"));
        assert_eq!(
            database_path(root.clone()),
            PathBuf::from("/tmp/ctx-root/work.sqlite")
        );
        assert_eq!(
            object_dir(root.clone()),
            PathBuf::from("/tmp/ctx-root/objects")
        );
        assert_eq!(
            blob_dir(root.clone()),
            PathBuf::from("/tmp/ctx-root/objects")
        );
        assert_eq!(
            spool_dir(root.clone()),
            PathBuf::from("/tmp/ctx-root/spool")
        );
        assert_eq!(
            inbox_dir(root.clone()),
            PathBuf::from("/tmp/ctx-root/spool")
        );
        assert_eq!(
            config_path(root.clone()),
            PathBuf::from("/tmp/ctx-root/config.toml")
        );
        assert_eq!(logs_dir(root.clone()), PathBuf::from("/tmp/ctx-root/logs"));
        assert_eq!(
            device_path(root),
            PathBuf::from("/tmp/ctx-root/device.json")
        );
    }

    #[test]
    fn ctx_data_root_env_is_the_ctx_root_itself() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        let _guard = ENV_LOCK.lock().unwrap();
        let previous = env::var_os("CTX_DATA_ROOT");
        env::remove_var("CTX_DATA_ROOT");

        let default_root = default_data_root().unwrap();
        assert!(default_root.ends_with(".ctx"));
        assert!(!default_root.ends_with("work-record"));

        env::set_var("CTX_DATA_ROOT", "/tmp/custom-ctx-root");

        assert_eq!(
            default_data_root().unwrap(),
            PathBuf::from("/tmp/custom-ctx-root")
        );
        assert_eq!(
            database_path(default_data_root().unwrap()),
            PathBuf::from("/tmp/custom-ctx-root/work.sqlite")
        );

        if let Some(previous) = previous {
            env::set_var("CTX_DATA_ROOT", previous);
        } else {
            env::remove_var("CTX_DATA_ROOT");
        }
    }
}
