use ctx_core::models::Session;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateSessionTitleRouteRequest {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    force: Option<bool>,
}

impl GenerateSessionTitleRouteRequest {
    pub fn into_parts(self) -> (Option<String>, Option<bool>) {
        (self.prompt, self.force)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct GenerateSessionTitleRouteResponse(Session);

impl From<Session> for GenerateSessionTitleRouteResponse {
    fn from(session: Session) -> Self {
        Self(session)
    }
}

impl GenerateSessionTitleRouteResponse {
    pub fn new(session: Session) -> Self {
        Self(session)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionModelRouteRequest {
    model_id: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

impl SetSessionModelRouteRequest {
    pub fn into_parts(self) -> (String, Option<String>) {
        (self.model_id, self.reasoning_effort)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SetSessionModelRouteResponse(Session);

impl From<Session> for SetSessionModelRouteResponse {
    fn from(session: Session) -> Self {
        Self(session)
    }
}

impl SetSessionModelRouteResponse {
    pub fn new(session: Session) -> Self {
        Self(session)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetSessionModeRouteRequest {
    mode_id: String,
}

impl SetSessionModeRouteRequest {
    pub fn into_mode_id(self) -> String {
        self.mode_id
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionTitleModelModeRouteErrorKind {
    BadRequest,
    NotFound,
    Forbidden,
    InsufficientStorage,
    ProviderUnavailable,
    LiveSwitchRejected,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionTitleModelModeRouteError {
    kind: SessionTitleModelModeRouteErrorKind,
    message: String,
}

impl SessionTitleModelModeRouteError {
    pub fn new(kind: SessionTitleModelModeRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SessionTitleModelModeRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SessionTitleModelModeRouteErrorKind::NotFound, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(SessionTitleModelModeRouteErrorKind::Forbidden, message)
    }

    pub fn insufficient_storage(message: impl Into<String>) -> Self {
        Self::new(
            SessionTitleModelModeRouteErrorKind::InsufficientStorage,
            message,
        )
    }

    pub fn provider_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            SessionTitleModelModeRouteErrorKind::ProviderUnavailable,
            message,
        )
    }

    pub fn live_switch_rejected(message: impl Into<String>) -> Self {
        Self::new(
            SessionTitleModelModeRouteErrorKind::LiveSwitchRejected,
            message,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(SessionTitleModelModeRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> SessionTitleModelModeRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
