use chrono::{DateTime, Utc};
use ctx_core::ids::{SessionId, TaskId, TerminalId, WorkspaceId, WorktreeId};
use ctx_core::models::{TerminalSession, TerminalStatus};
use serde::{Deserialize, Serialize};

pub const DEFAULT_OUTPUT_TAIL_BYTES: usize = 20 * 1024;

#[derive(Debug)]
pub struct ListWorkspaceTerminalsRouteParams {
    workspace_id: String,
}

impl ListWorkspaceTerminalsRouteParams {
    pub fn new(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: workspace_id.into(),
        }
    }

    pub fn parse_workspace_id(&self) -> Result<WorkspaceId, TerminalRouteError> {
        parse_workspace_id(&self.workspace_id)
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateTerminalRouteRequest {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub worktree_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTerminalRouteSpec {
    pub workspace_id: WorkspaceId,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
}

impl CreateTerminalRouteRequest {
    pub fn parse(
        self,
        raw_workspace_id: &str,
    ) -> Result<CreateTerminalRouteSpec, TerminalRouteError> {
        let workspace_id = parse_workspace_id(raw_workspace_id)?;
        let task_id = parse_optional_id(self.task_id, "invalid task_id")?.map(TaskId);
        let session_id = parse_optional_id(self.session_id, "invalid session_id")?.map(SessionId);
        let worktree_id =
            parse_optional_id(self.worktree_id, "invalid worktree_id")?.map(WorktreeId);

        Ok(CreateTerminalRouteSpec {
            workspace_id,
            task_id,
            session_id,
            worktree_id,
            cwd: self.cwd,
            shell: self.shell,
        })
    }
}

#[derive(Debug)]
pub struct DeleteTerminalRouteParams {
    terminal_id: String,
}

impl DeleteTerminalRouteParams {
    pub fn new(terminal_id: impl Into<String>) -> Self {
        Self {
            terminal_id: terminal_id.into(),
        }
    }

    pub fn parse_terminal_id(&self) -> Result<TerminalId, TerminalRouteError> {
        parse_terminal_id(&self.terminal_id)
    }
}

#[derive(Debug)]
pub struct MintTerminalStreamTokenRouteParams {
    terminal_id: String,
}

impl MintTerminalStreamTokenRouteParams {
    pub fn new(terminal_id: impl Into<String>) -> Self {
        Self {
            terminal_id: terminal_id.into(),
        }
    }

    pub fn parse_terminal_id(&self) -> Result<TerminalId, TerminalRouteError> {
        parse_terminal_id(&self.terminal_id)
    }
}

#[derive(Debug)]
pub struct TerminalStreamRouteParams {
    terminal_id: String,
    token: Option<String>,
    tail: Option<String>,
}

impl TerminalStreamRouteParams {
    pub fn new(
        terminal_id: impl Into<String>,
        token: Option<String>,
        tail: Option<String>,
    ) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            token,
            tail,
        }
    }

    pub fn parse_terminal_id(&self) -> Result<TerminalId, TerminalRouteError> {
        parse_terminal_id(&self.terminal_id)
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn tail_bytes(&self) -> usize {
        parse_terminal_stream_tail_bytes(self.tail.as_deref())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalSessionRouteResponse {
    pub id: TerminalId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<WorktreeId>,
    pub cwd: String,
    pub shell: String,
    pub title: String,
    pub status: TerminalStatusRouteResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stream_path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatusRouteResponse {
    Running,
    Exited,
}

impl From<TerminalSession> for TerminalSessionRouteResponse {
    fn from(session: TerminalSession) -> Self {
        Self {
            id: session.id,
            workspace_id: session.workspace_id,
            task_id: session.task_id,
            session_id: session.session_id,
            worktree_id: session.worktree_id,
            cwd: session.cwd,
            shell: session.shell,
            title: session.title,
            status: session.status.into(),
            exit_code: session.exit_code,
            stream_path: session.stream_path,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

impl From<TerminalStatus> for TerminalStatusRouteResponse {
    fn from(status: TerminalStatus) -> Self {
        match status {
            TerminalStatus::Running => Self::Running,
            TerminalStatus::Exited => Self::Exited,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalStreamConnectRouteResponse {
    pub stream_path: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRouteErrorKind {
    BadRequest,
    Unauthorized,
    NotFound,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRouteError {
    kind: TerminalRouteErrorKind,
    message: String,
}

impl TerminalRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: TerminalRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: TerminalRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            kind: TerminalRouteErrorKind::Unauthorized,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: TerminalRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> TerminalRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub fn parse_workspace_id(value: &str) -> Result<WorkspaceId, TerminalRouteError> {
    uuid::Uuid::parse_str(value)
        .map(WorkspaceId)
        .map_err(|_| TerminalRouteError::bad_request("invalid workspace id"))
}

pub fn parse_terminal_id(value: &str) -> Result<TerminalId, TerminalRouteError> {
    uuid::Uuid::parse_str(value)
        .map(TerminalId)
        .map_err(|_| TerminalRouteError::bad_request("invalid terminal id"))
}

pub fn parse_terminal_stream_tail_bytes(raw_tail: Option<&str>) -> usize {
    raw_tail
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<usize>().ok()
            }
        })
        .unwrap_or(DEFAULT_OUTPUT_TAIL_BYTES)
}

fn parse_optional_id(
    raw: Option<String>,
    error: &'static str,
) -> Result<Option<uuid::Uuid>, TerminalRouteError> {
    raw.map(|value| {
        uuid::Uuid::parse_str(value.trim()).map_err(|_| TerminalRouteError::bad_request(error))
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn terminal_session_with_optional_ids(status: TerminalStatus) -> TerminalSession {
        TerminalSession {
            id: TerminalId::new(),
            workspace_id: WorkspaceId::new(),
            task_id: Some(TaskId::new()),
            session_id: Some(SessionId::new()),
            worktree_id: Some(WorktreeId::new()),
            cwd: "/tmp/work".to_string(),
            shell: "/bin/zsh".to_string(),
            title: "zsh".to_string(),
            status,
            exit_code: Some(7),
            stream_path: "/api/terminals/stream?token=secret".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 17, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 5, 17, 10, 1, 0).unwrap(),
        }
    }

    #[test]
    fn terminal_route_response_matches_raw_session_wire_shape_with_optional_fields() {
        let session = terminal_session_with_optional_ids(TerminalStatus::Running);
        let response = TerminalSessionRouteResponse::from(session.clone());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::to_value(session).unwrap()
        );
    }

    #[test]
    fn terminal_route_response_matches_raw_session_wire_shape_without_optional_fields() {
        let mut session = terminal_session_with_optional_ids(TerminalStatus::Exited);
        session.task_id = None;
        session.session_id = None;
        session.worktree_id = None;
        session.exit_code = None;
        let response = TerminalSessionRouteResponse::from(session.clone());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::to_value(session).unwrap()
        );
    }

    #[test]
    fn stream_connect_route_response_preserves_wire_shape() {
        let expires_at = Utc.with_ymd_and_hms(2026, 5, 17, 11, 0, 0).unwrap();
        let response = TerminalStreamConnectRouteResponse {
            stream_path: "/api/terminals/abc/stream?token=secret".to_string(),
            expires_at,
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "stream_path": "/api/terminals/abc/stream?token=secret",
                "expires_at": expires_at,
            })
        );
    }

    #[test]
    fn create_route_request_preserves_invalid_id_messages() {
        let error = CreateTerminalRouteRequest {
            task_id: None,
            session_id: None,
            worktree_id: None,
            cwd: None,
            shell: None,
        }
        .parse("not-a-workspace")
        .unwrap_err();
        assert_eq!(error.kind(), TerminalRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");

        let workspace_id = WorkspaceId::new().0.to_string();
        for (field, message) in [
            ("task", "invalid task_id"),
            ("session", "invalid session_id"),
            ("worktree", "invalid worktree_id"),
        ] {
            let req = CreateTerminalRouteRequest {
                task_id: (field == "task").then(|| "not-a-task".to_string()),
                session_id: (field == "session").then(|| "not-a-session".to_string()),
                worktree_id: (field == "worktree").then(|| "not-a-worktree".to_string()),
                cwd: None,
                shell: None,
            };
            let error = req.parse(&workspace_id).unwrap_err();
            assert_eq!(error.kind(), TerminalRouteErrorKind::BadRequest);
            assert_eq!(error.message(), message);
        }
    }

    #[test]
    fn create_route_request_trims_optional_ids() {
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let worktree_id = WorktreeId::new();
        let req = CreateTerminalRouteRequest {
            task_id: Some(format!(" {} ", task_id.0)),
            session_id: Some(format!("\n{}\t", session_id.0)),
            worktree_id: Some(format!(" {} ", worktree_id.0)),
            cwd: Some(".".to_string()),
            shell: Some("/bin/sh".to_string()),
        };

        let spec = req
            .parse(&workspace_id.0.to_string())
            .expect("valid ids should parse");

        assert_eq!(spec.workspace_id, workspace_id);
        assert_eq!(spec.task_id, Some(task_id));
        assert_eq!(spec.session_id, Some(session_id));
        assert_eq!(spec.worktree_id, Some(worktree_id));
        assert_eq!(spec.cwd.as_deref(), Some("."));
        assert_eq!(spec.shell.as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn terminal_stream_route_rejects_invalid_terminal_id() {
        let error = parse_terminal_id("not-a-terminal").unwrap_err();
        assert_eq!(error.kind(), TerminalRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid terminal id");
    }

    #[test]
    fn terminal_stream_tail_defaults_and_parses() {
        assert_eq!(
            parse_terminal_stream_tail_bytes(None),
            DEFAULT_OUTPUT_TAIL_BYTES
        );
        assert_eq!(
            parse_terminal_stream_tail_bytes(Some("")),
            DEFAULT_OUTPUT_TAIL_BYTES
        );
        assert_eq!(
            parse_terminal_stream_tail_bytes(Some(" \n\t ")),
            DEFAULT_OUTPUT_TAIL_BYTES
        );
        assert_eq!(
            parse_terminal_stream_tail_bytes(Some("not-a-number")),
            DEFAULT_OUTPUT_TAIL_BYTES
        );
        assert_eq!(parse_terminal_stream_tail_bytes(Some("0")), 0);
        assert_eq!(parse_terminal_stream_tail_bytes(Some("42")), 42);
        assert_eq!(parse_terminal_stream_tail_bytes(Some(" 42 ")), 42);
    }
}
