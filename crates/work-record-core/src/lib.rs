use std::{env, fmt, path::PathBuf, str::FromStr};

use chrono::{DateTime, Utc};
use directories::BaseDirs;
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
        Shim => "shim",
        DirectCli => "direct_cli",
        Dashboard => "dashboard",
        HostedSync => "hosted_sync",
        Manual => "manual",
    }
    default Manual
}

text_enum! {
    pub enum CaptureProvider {
        Codex => "codex",
        Claude => "claude",
        Pi => "pi",
        Cursor => "cursor",
        Shell => "shell",
        Git => "git",
        Jj => "jj",
        Gh => "gh",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum WorkRecordStatus {
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
        Evidence => "evidence",
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
        PrLink => "pr_link",
        Evidence => "evidence",
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

text_enum! {
    pub enum PullRequestProvider {
        Github => "github",
        Gitlab => "gitlab",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum PullRequestLinkSource {
        Explicit => "explicit",
        GhShim => "gh_shim",
        CapturedUrl => "captured_url",
        InferredBranch => "inferred_branch",
        InferredCommit => "inferred_commit",
        Manual => "manual",
    }
    default Manual
}

text_enum! {
    pub enum WorkRecordLinkTargetType {
        Session => "session",
        Run => "run",
        Event => "event",
        VcsWorkspace => "vcs_workspace",
        VcsChange => "vcs_change",
        PullRequest => "pull_request",
        Artifact => "artifact",
        Evidence => "evidence",
    }
    default Event
}

text_enum! {
    pub enum WorkRecordLinkType {
        Produced => "produced",
        Touched => "touched",
        References => "references",
        EvidenceFor => "evidence_for",
        PublishedTo => "published_to",
        LikelyRelated => "likely_related",
    }
    default References
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
    pub enum EvidenceKind {
        Test => "test",
        Lint => "lint",
        Build => "build",
        Typecheck => "typecheck",
        Screenshot => "screenshot",
        Review => "review",
        Ci => "ci",
        Manual => "manual",
    }
    default Manual
}

text_enum! {
    pub enum EvidenceStatus {
        Passed => "passed",
        Failed => "failed",
        Skipped => "skipped",
        Stale => "stale",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum EvidenceFreshness {
        Fresh => "fresh",
        ProbablyFresh => "probably_fresh",
        Stale => "stale",
        Unbound => "unbound",
        Inferred => "inferred",
    }
    default Unbound
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
        Hosted => "hosted",
    }
    default System
}

text_enum! {
    pub enum ContextCitationType {
        WorkRecord => "work_record",
        Session => "session",
        Run => "run",
        Event => "event",
        VcsChange => "vcs_change",
        PullRequest => "pull_request",
        Artifact => "artifact",
        Evidence => "evidence",
        Summary => "summary",
        File => "file",
    }
    default Event
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRecord {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub kind: String,
    pub workspace: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkRecord {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
        kind: impl Into<String>,
        workspace: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            body: body.into(),
            tags,
            kind: kind.into(),
            workspace,
            pr_url: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: Uuid,
    pub record_id: Option<Uuid>,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
}

impl Evidence {
    pub fn new(
        record_id: Option<Uuid>,
        command: impl Into<String>,
        exit_code: i32,
        stdout: String,
        stderr: String,
        started_at: DateTime<Utc>,
        duration_ms: i64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            record_id,
            command: command.into(),
            exit_code,
            stdout,
            stderr,
            started_at,
            duration_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRecordArchive {
    pub version: u32,
    pub records: Vec<WorkRecord>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkContext {
    pub query: Option<String>,
    pub records: Vec<WorkRecord>,
    pub evidence: Vec<Evidence>,
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
pub struct WorkRecordMetadata {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: WorkRecordStatus,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_record_id: Option<Uuid>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_record_id: Option<Uuid>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_record_id: Option<Uuid>,
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
pub struct PullRequest {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_workspace_id: Option<Uuid>,
    pub provider: PullRequestProvider,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub confidence: Confidence,
    pub link_source: PullRequestLinkSource,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkRecordLink {
    pub id: Uuid,
    pub work_record_id: Uuid,
    pub target_type: WorkRecordLinkTargetType,
    pub target_id: Uuid,
    pub link_type: WorkRecordLinkType,
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
pub struct EvidenceMetadata {
    pub id: Uuid,
    pub work_record_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_change_id: Option<Uuid>,
    pub kind: EvidenceKind,
    pub status: EvidenceStatus,
    #[serde(default)]
    pub freshness: EvidenceFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_tree_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitationReference {
    pub target_type: WorkRecordLinkTargetType,
    pub target_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_record_id: Option<Uuid>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_record_id: Option<Uuid>,
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
pub struct WorkRecordTag {
    pub work_record_id: Uuid,
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
pub struct ContextBudget {
    pub max_tokens: u32,
    pub estimated_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCitation {
    #[serde(rename = "type")]
    pub citation_type: ContextCitationType,
    pub id: Uuid,
    pub label: String,
    pub time: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEvidence {
    pub id: Uuid,
    pub kind: EvidenceKind,
    pub status: EvidenceStatus,
    #[serde(default)]
    pub freshness: EvidenceFreshness,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextLinks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextResult {
    pub record_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub rank: f32,
    #[serde(default)]
    pub why_matched: Vec<String>,
    #[serde(default)]
    pub citations: Vec<ContextCitation>,
    #[serde(default)]
    pub evidence: Vec<ContextEvidence>,
    #[serde(default)]
    pub links: ContextLinks,
    #[serde(default)]
    pub visibility: Visibility,
}

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
    #[serde(default)]
    pub omitted_evidence: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentContextPacket {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub budget: ContextBudget,
    #[serde(default)]
    pub results: Vec<ContextResult>,
    #[serde(default)]
    pub pagination: ContextPagination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<ContextTruncation>,
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_pending_sync_state() -> SyncState {
    SyncState::Pending
}

pub fn default_data_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DATA_ROOT") {
        return Ok(PathBuf::from(value));
    }

    let base = BaseDirs::new().ok_or(CoreError::MissingHome)?;
    Ok(base.home_dir().join(".ctx"))
}

pub fn work_record_dir(root: PathBuf) -> PathBuf {
    root.join("work-record")
}

pub fn database_path(root: PathBuf) -> PathBuf {
    work_record_dir(root).join("work.sqlite")
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
        assert_eq!(EvidenceFreshness::default(), EvidenceFreshness::Unbound);

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
    fn agent_context_packet_serializes_as_contract_json() {
        let generated_at = DateTime::parse_from_rfc3339("2026-06-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let record_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000001").unwrap();
        let event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000002").unwrap();
        let evidence_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000003").unwrap();

        let packet = AgentContextPacket {
            schema_version: 1,
            query: Some("checkout retry".to_owned()),
            generated_at,
            budget: ContextBudget {
                max_tokens: 12_000,
                estimated_tokens: 4_312,
            },
            results: vec![ContextResult {
                record_id,
                title: "Fix checkout retry".to_owned(),
                summary: Some("short redacted summary".to_owned()),
                rank: 0.93,
                why_matched: vec![
                    "title".to_owned(),
                    "primary_user_message".to_owned(),
                    "failed_command".to_owned(),
                ],
                citations: vec![ContextCitation {
                    citation_type: ContextCitationType::Event,
                    id: event_id,
                    label: "primary user prompt".to_owned(),
                    time: generated_at,
                }],
                evidence: vec![ContextEvidence {
                    id: evidence_id,
                    kind: EvidenceKind::Test,
                    status: EvidenceStatus::Passed,
                    freshness: EvidenceFreshness::Fresh,
                }],
                links: ContextLinks {
                    dashboard: Some(format!("http://127.0.0.1:3000/records/{record_id}")),
                    pr: Some("https://github.com/org/repo/pull/123".to_owned()),
                },
                visibility: Visibility::Reportable,
            }],
            pagination: ContextPagination {
                cursor: Some("opaque".to_owned()),
                has_more: false,
            },
            truncation: Some(ContextTruncation::default()),
        };

        let value = serde_json::to_value(&packet).unwrap();
        assert_eq!(value["schema_version"], json!(1));
        assert_eq!(value["query"], json!("checkout retry"));
        assert_eq!(value["generated_at"], json!("2026-06-22T00:00:00Z"));
        assert_eq!(value["budget"]["max_tokens"], json!(12000));
        assert_eq!(
            value["results"][0]["record_id"],
            json!(record_id.to_string())
        );
        assert_eq!(value["results"][0]["citations"][0]["type"], json!("event"));
        assert_eq!(
            value["results"][0]["evidence"][0]["freshness"],
            json!("fresh")
        );
        assert_eq!(value["results"][0]["visibility"], json!("reportable"));
        assert_eq!(value["pagination"]["cursor"], json!("opaque"));

        let decoded: AgentContextPacket = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.results[0].record_id, record_id);
        assert_eq!(
            decoded.results[0].evidence[0].status,
            EvidenceStatus::Passed
        );
        assert_eq!(decoded.results[0].visibility, Visibility::Reportable);
    }
}
