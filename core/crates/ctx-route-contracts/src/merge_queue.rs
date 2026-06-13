use chrono::{DateTime, Utc};
use ctx_core::ids::{MergeQueueEntryId, SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SubmitMergeQueueEntryRouteRequest {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    worktree_id: Option<String>,
    #[serde(default)]
    worktree_root: Option<String>,
    #[serde(default)]
    target_branch: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

pub type SubmitMergeQueueEntryRouteParts = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl SubmitMergeQueueEntryRouteRequest {
    pub fn new(
        session_id: Option<String>,
        worktree_id: Option<String>,
        worktree_root: Option<String>,
        target_branch: Option<String>,
        message: Option<String>,
    ) -> Self {
        Self {
            session_id,
            worktree_id,
            worktree_root,
            target_branch,
            message,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn worktree_id(&self) -> Option<&str> {
        self.worktree_id.as_deref()
    }

    pub fn into_parts(self) -> SubmitMergeQueueEntryRouteParts {
        (
            self.session_id,
            self.worktree_id,
            self.worktree_root,
            self.target_branch,
            self.message,
        )
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MergeQueueSubmitRouteErrorKind {
    BadRequest,
    Unauthorized,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MergeQueueSubmitRouteError {
    kind: MergeQueueSubmitRouteErrorKind,
    message: String,
}

impl MergeQueueSubmitRouteError {
    fn new(kind: MergeQueueSubmitRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(MergeQueueSubmitRouteErrorKind::BadRequest, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(MergeQueueSubmitRouteErrorKind::Unauthorized, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(MergeQueueSubmitRouteErrorKind::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(MergeQueueSubmitRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> MergeQueueSubmitRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Deserialize)]
pub struct ListMergeQueueEntriesRouteRequest {
    workspace_id: String,
    #[serde(default)]
    limit: Option<i64>,
}

impl ListMergeQueueEntriesRouteRequest {
    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, MergeQueueEntryRouteError> {
        parse_workspace_id(&self.workspace_id)
    }

    pub fn limit(&self) -> Option<i64> {
        self.limit
    }
}

#[derive(Debug, Deserialize)]
pub struct MergeQueueEntryRouteParams {
    workspace_id: String,
    id: String,
}

impl MergeQueueEntryRouteParams {
    pub fn new(workspace_id: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            id: id.into(),
        }
    }

    pub fn parse(&self) -> Result<(WorkspaceId, MergeQueueEntryId), MergeQueueEntryRouteError> {
        Ok((
            parse_workspace_id(&self.workspace_id)?,
            parse_entry_id(&self.id)?,
        ))
    }

    pub fn parse_for_log_download(
        &self,
    ) -> Result<(WorkspaceId, MergeQueueEntryId), MergeQueueLogDownloadRouteError> {
        self.parse()
            .map_err(MergeQueueLogDownloadRouteError::from_entry_route_error)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeQueueEntryRouteResponse {
    pub id: MergeQueueEntryId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<WorktreeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub target_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub patch_source: MergeQueuePatchSourceRouteResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit_sha: Option<String>,
    pub patch_path: String,
    pub patch_size: i64,
    pub status: MergeQueueEntryStatusRouteResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueuePatchSourceRouteResponse {
    Generated,
    Provided,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueueEntryStatusRouteResponse {
    Queued,
    Running,
    Passed,
    Failed,
    Conflict,
    Cancelled,
}

impl From<MergeQueueEntry> for MergeQueueEntryRouteResponse {
    fn from(entry: MergeQueueEntry) -> Self {
        Self {
            id: entry.id,
            workspace_id: entry.workspace_id,
            worktree_id: entry.worktree_id,
            session_id: entry.session_id,
            target_branch: entry.target_branch,
            message: entry.message,
            patch_source: entry.patch_source.into(),
            base_commit_sha: entry.base_commit_sha,
            head_commit_sha: entry.head_commit_sha,
            patch_path: entry.patch_path,
            patch_size: entry.patch_size,
            status: entry.status.into(),
            result_commit_sha: entry.result_commit_sha,
            error_message: entry.error_message,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
        }
    }
}

impl From<MergeQueuePatchSource> for MergeQueuePatchSourceRouteResponse {
    fn from(source: MergeQueuePatchSource) -> Self {
        match source {
            MergeQueuePatchSource::Generated => Self::Generated,
            MergeQueuePatchSource::Provided => Self::Provided,
        }
    }
}

impl From<MergeQueueEntryStatus> for MergeQueueEntryStatusRouteResponse {
    fn from(status: MergeQueueEntryStatus) -> Self {
        match status {
            MergeQueueEntryStatus::Queued => Self::Queued,
            MergeQueueEntryStatus::Running => Self::Running,
            MergeQueueEntryStatus::Passed => Self::Passed,
            MergeQueueEntryStatus::Failed => Self::Failed,
            MergeQueueEntryStatus::Conflict => Self::Conflict,
            MergeQueueEntryStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MergeQueueEntryRouteErrorKind {
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MergeQueueEntryRouteError {
    kind: MergeQueueEntryRouteErrorKind,
    message: String,
}

impl MergeQueueEntryRouteError {
    fn new(kind: MergeQueueEntryRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(MergeQueueEntryRouteErrorKind::BadRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(MergeQueueEntryRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> MergeQueueEntryRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MergeQueueLogDownloadRouteErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MergeQueueLogDownloadRouteError {
    kind: MergeQueueLogDownloadRouteErrorKind,
}

impl MergeQueueLogDownloadRouteError {
    pub fn bad_request() -> Self {
        Self {
            kind: MergeQueueLogDownloadRouteErrorKind::BadRequest,
        }
    }

    pub fn not_found() -> Self {
        Self {
            kind: MergeQueueLogDownloadRouteErrorKind::NotFound,
        }
    }

    pub fn internal() -> Self {
        Self {
            kind: MergeQueueLogDownloadRouteErrorKind::Internal,
        }
    }

    pub fn from_entry_route_error(error: MergeQueueEntryRouteError) -> Self {
        match error.kind() {
            MergeQueueEntryRouteErrorKind::BadRequest => Self::bad_request(),
            MergeQueueEntryRouteErrorKind::Internal => Self::internal(),
        }
    }

    pub fn kind(&self) -> MergeQueueLogDownloadRouteErrorKind {
        self.kind
    }
}

fn parse_workspace_id(value: &str) -> Result<WorkspaceId, MergeQueueEntryRouteError> {
    uuid::Uuid::parse_str(value)
        .map(WorkspaceId)
        .map_err(|_| MergeQueueEntryRouteError::bad_request("invalid workspace id"))
}

fn parse_entry_id(value: &str) -> Result<MergeQueueEntryId, MergeQueueEntryRouteError> {
    uuid::Uuid::parse_str(value)
        .map(MergeQueueEntryId)
        .map_err(|_| MergeQueueEntryRouteError::bad_request("invalid entry id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn full_entry() -> MergeQueueEntry {
        MergeQueueEntry {
            id: MergeQueueEntryId::new(),
            workspace_id: WorkspaceId::new(),
            worktree_id: Some(WorktreeId::new()),
            session_id: Some(SessionId::new()),
            target_branch: "dev".to_string(),
            message: Some("ship it".to_string()),
            patch_source: MergeQueuePatchSource::Provided,
            base_commit_sha: Some("base".to_string()),
            head_commit_sha: Some("head".to_string()),
            patch_path: "/tmp/entry.patch".to_string(),
            patch_size: 42,
            status: MergeQueueEntryStatus::Passed,
            result_commit_sha: Some("result".to_string()),
            error_message: Some("kept for wire-shape coverage".to_string()),
            created_at: Utc.with_ymd_and_hms(2026, 5, 17, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 5, 17, 10, 1, 0).unwrap(),
        }
    }

    #[test]
    fn entry_route_response_matches_raw_entry_wire_shape_with_optional_fields() {
        let entry = full_entry();
        let response = MergeQueueEntryRouteResponse::from(entry.clone());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::to_value(entry).unwrap()
        );
    }

    #[test]
    fn entry_route_response_matches_raw_entry_wire_shape_without_optional_fields() {
        let mut entry = full_entry();
        entry.worktree_id = None;
        entry.session_id = None;
        entry.message = None;
        entry.base_commit_sha = None;
        entry.head_commit_sha = None;
        entry.result_commit_sha = None;
        entry.error_message = None;
        entry.patch_source = MergeQueuePatchSource::Generated;
        entry.status = MergeQueueEntryStatus::Queued;

        let response = MergeQueueEntryRouteResponse::from(entry.clone());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::to_value(entry).unwrap()
        );
    }

    #[test]
    fn list_route_request_preserves_limit_pass_through() {
        let req = ListMergeQueueEntriesRouteRequest {
            workspace_id: WorkspaceId::new().0.to_string(),
            limit: Some(-1),
        };

        assert!(req.parse_workspace_id().is_ok());
        assert_eq!(req.limit(), Some(-1));
    }

    #[test]
    fn entry_route_params_preserve_invalid_id_messages() {
        let req = ListMergeQueueEntriesRouteRequest {
            workspace_id: "not-a-workspace".to_string(),
            limit: None,
        };
        let error = req.parse_workspace_id().unwrap_err();
        assert_eq!(error.kind(), MergeQueueEntryRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");

        let error =
            MergeQueueEntryRouteParams::new(WorkspaceId::new().0.to_string(), "not-an-entry")
                .parse()
                .unwrap_err();
        assert_eq!(error.kind(), MergeQueueEntryRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid entry id");
    }

    #[test]
    fn log_download_route_param_errors_are_transport_safe() {
        let params_error = MergeQueueEntryRouteParams::new(
            "not-a-workspace",
            MergeQueueEntryId::new().0.to_string(),
        )
        .parse_for_log_download()
        .unwrap_err();
        assert_eq!(
            params_error.kind(),
            MergeQueueLogDownloadRouteErrorKind::BadRequest
        );

        let params_error =
            MergeQueueEntryRouteParams::new(WorkspaceId::new().0.to_string(), "not-an-entry")
                .parse_for_log_download()
                .unwrap_err();
        assert_eq!(
            params_error.kind(),
            MergeQueueLogDownloadRouteErrorKind::BadRequest
        );
    }

    #[test]
    fn submit_route_request_defaults_missing_fields_to_none() {
        let request: SubmitMergeQueueEntryRouteRequest =
            serde_json::from_value(serde_json::json!({})).expect("deserialize submit request");

        let (session_id, worktree_id, worktree_root, target_branch, message) = request.into_parts();
        assert_eq!(session_id, None);
        assert_eq!(worktree_id, None);
        assert_eq!(worktree_root, None);
        assert_eq!(target_branch, None);
        assert_eq!(message, None);
    }

    #[test]
    fn submit_route_request_preserves_raw_optional_strings() {
        let request: SubmitMergeQueueEntryRouteRequest =
            serde_json::from_value(serde_json::json!({
                "session_id": "session",
                "worktree_id": "worktree",
                "worktree_root": "  /tmp/worktree  ",
                "target_branch": "main",
                "message": "ship it"
            }))
            .expect("deserialize submit request");

        assert_eq!(request.session_id(), Some("session"));
        assert_eq!(request.worktree_id(), Some("worktree"));
        let parts = request.into_parts();
        assert_eq!(parts.2.as_deref(), Some("  /tmp/worktree  "));
        assert_eq!(parts.3.as_deref(), Some("main"));
        assert_eq!(parts.4.as_deref(), Some("ship it"));
    }

    #[test]
    fn submit_route_errors_keep_kind_and_message_private_with_accessors() {
        let error = MergeQueueSubmitRouteError::unauthorized("scope denied");

        assert_eq!(error.kind(), MergeQueueSubmitRouteErrorKind::Unauthorized);
        assert_eq!(error.message(), "scope denied");
    }
}
