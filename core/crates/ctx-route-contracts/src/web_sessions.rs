use chrono::{DateTime, Utc};
use ctx_core::ids::{SessionId, WorktreeId};
use serde::{Deserialize, Serialize};

pub const DEFAULT_WEB_SESSION_ACTION_TIMEOUT_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSessionStatus {
    Running,
    Closed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionViewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionInfo {
    pub id: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub worktree_id: Option<String>,
    pub status: WebSessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub url: String,
    pub viewport: WebSessionViewport,
    pub fps: u32,
    pub viewers: u32,
    pub stream_path: String,
    pub stream_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionRunRequest {
    pub code: Option<String>,
    pub script_path: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionRunResponse {
    pub ok: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSessionRouteErrorKind {
    BadRequest,
    Forbidden,
    NotFound,
    Internal,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WebSessionRouteError {
    kind: WebSessionRouteErrorKind,
    message: String,
}

impl WebSessionRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: WebSessionRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            kind: WebSessionRouteErrorKind::Forbidden,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: WebSessionRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: WebSessionRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> WebSessionRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Deserialize)]
pub struct WebSessionCreateRouteRequest {
    pub session_id: Option<String>,
    pub worktree_id: Option<String>,
    pub url: String,
    pub viewport: Option<WebSessionViewport>,
    pub fps: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct WebSessionCreateRouteSpec {
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub url: String,
    pub viewport: Option<WebSessionViewport>,
    pub fps: Option<u32>,
}

impl WebSessionCreateRouteRequest {
    pub fn validate(self) -> Result<WebSessionCreateRouteSpec, WebSessionRouteError> {
        if self.url.trim().is_empty() {
            return Err(WebSessionRouteError::bad_request("url is required"));
        }

        let session_id = parse_optional_session_id(self.session_id.as_deref())?;
        let worktree_id = parse_optional_worktree_id(self.worktree_id.as_deref())?;

        Ok(WebSessionCreateRouteSpec {
            session_id,
            worktree_id,
            url: self.url,
            viewport: self.viewport,
            fps: self.fps,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct WebSessionListRouteQuery {
    pub session_id: Option<String>,
}

impl WebSessionListRouteQuery {
    pub fn validated_session_id(&self) -> Result<Option<&str>, WebSessionRouteError> {
        let Some(session_id) = self.session_id.as_deref() else {
            return Ok(None);
        };
        uuid::Uuid::parse_str(session_id)
            .map_err(|_| WebSessionRouteError::bad_request("invalid session id"))?;
        Ok(Some(session_id))
    }
}

#[derive(Debug, Deserialize)]
pub struct WebSessionActionRouteRequest {
    pub code: Option<String>,
    pub script_path: Option<String>,
    pub timeout_ms: Option<u64>,
}

impl WebSessionActionRouteRequest {
    pub fn into_run_request(self) -> WebSessionRunRequest {
        WebSessionRunRequest {
            code: self.code,
            script_path: self.script_path,
            timeout_ms: Some(
                self.timeout_ms
                    .unwrap_or(DEFAULT_WEB_SESSION_ACTION_TIMEOUT_MS),
            ),
        }
    }
}

fn parse_optional_session_id(raw: Option<&str>) -> Result<Option<SessionId>, WebSessionRouteError> {
    raw.map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| WebSessionRouteError::bad_request("invalid session id"))
        .map(|id| id.map(SessionId))
}

fn parse_optional_worktree_id(
    raw: Option<&str>,
) -> Result<Option<WorktreeId>, WebSessionRouteError> {
    raw.map(uuid::Uuid::parse_str)
        .transpose()
        .map_err(|_| WebSessionRouteError::bad_request("invalid worktree id"))
        .map(|id| id.map(WorktreeId))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_route_error(request: WebSessionCreateRouteRequest) -> WebSessionRouteError {
        match request.validate() {
            Ok(_) => panic!("expected route request to fail"),
            Err(error) => error,
        }
    }

    #[test]
    fn create_route_request_preserves_empty_url_precedence() {
        let request = WebSessionCreateRouteRequest {
            session_id: Some("not-a-uuid".to_string()),
            worktree_id: Some("also-not-a-uuid".to_string()),
            url: " ".to_string(),
            viewport: None,
            fps: None,
        };

        let error = create_route_error(request);
        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "url is required");
    }

    #[test]
    fn create_route_request_rejects_invalid_session_id() {
        let request = WebSessionCreateRouteRequest {
            session_id: Some("not-a-uuid".to_string()),
            worktree_id: None,
            url: "https://example.test".to_string(),
            viewport: None,
            fps: None,
        };

        let error = create_route_error(request);
        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
    }

    #[test]
    fn create_route_request_rejects_invalid_worktree_id() {
        let request = WebSessionCreateRouteRequest {
            session_id: None,
            worktree_id: Some("not-a-uuid".to_string()),
            url: "https://example.test".to_string(),
            viewport: None,
            fps: None,
        };

        let error = create_route_error(request);
        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid worktree id");
    }

    #[test]
    fn list_route_query_validates_but_preserves_raw_session_filter() {
        let raw = uuid::Uuid::new_v4().to_string();
        let query = WebSessionListRouteQuery {
            session_id: Some(raw.clone()),
        };

        assert_eq!(query.validated_session_id().unwrap(), Some(raw.as_str()));

        let error = WebSessionListRouteQuery {
            session_id: Some("not-a-uuid".to_string()),
        }
        .validated_session_id()
        .unwrap_err();
        assert_eq!(error.kind(), WebSessionRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session id");
    }

    #[test]
    fn action_route_request_defaults_timeout_only_when_missing() {
        let defaulted = WebSessionActionRouteRequest {
            code: Some("1 + 1".to_string()),
            script_path: None,
            timeout_ms: None,
        }
        .into_run_request();
        assert_eq!(
            defaulted.timeout_ms,
            Some(DEFAULT_WEB_SESSION_ACTION_TIMEOUT_MS)
        );

        let explicit = WebSessionActionRouteRequest {
            code: None,
            script_path: Some("script.js".to_string()),
            timeout_ms: Some(42),
        }
        .into_run_request();
        assert_eq!(explicit.timeout_ms, Some(42));
    }
}
