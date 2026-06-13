use ctx_core::models::DiffUnavailableReason;
use serde::{Deserialize, Serialize};

fn is_true(v: &bool) -> bool {
    *v
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SessionVcsRouteQuery {
    base_commit_sha: Option<String>,
    target_branch: Option<String>,
}

impl SessionVcsRouteQuery {
    pub fn into_parts(self) -> (Option<String>, Option<String>) {
        (self.base_commit_sha, self.target_branch)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ApplySessionVcsDiffPatchRouteRequest {
    action: String,
    patch: String,
}

impl ApplySessionVcsDiffPatchRouteRequest {
    pub fn action(&self) -> &str {
        &self.action
    }

    pub fn patch(&self) -> &str {
        &self.patch
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionVcsDiffRouteResponse {
    pub diff: String,
    #[serde(skip_serializing_if = "is_true")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionVcsDiffSummaryRouteResponse {
    pub base_commit_sha: String,
    pub head_commit_sha: String,
    pub file_count: i64,
    pub line_additions: i64,
    pub line_deletions: i64,
    #[serde(skip_serializing_if = "is_true")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<DiffUnavailableReason>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionVcsGitStatusRouteResponse {
    pub raw: String,
    pub summary_line: String,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<SessionVcsGitStatusEntryRouteResponse>,
    pub entries_truncated: bool,
    pub entries_total_count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionVcsGitStatusEntryRouteResponse {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionVcsRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionVcsRouteError {
    kind: SessionVcsRouteErrorKind,
    message: String,
}

impl SessionVcsRouteError {
    pub fn new(kind: SessionVcsRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SessionVcsRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SessionVcsRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(SessionVcsRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> SessionVcsRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
