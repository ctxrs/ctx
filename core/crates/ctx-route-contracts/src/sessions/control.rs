use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthenticateSessionRouteRequest {
    #[serde(default)]
    method_id: Option<String>,
}

impl AuthenticateSessionRouteRequest {
    pub fn into_method_id(self) -> Option<String> {
        self.method_id
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitAskUserQuestionRouteRequest {
    tool_call_id: String,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    answers: Option<HashMap<String, String>>,
}

impl SubmitAskUserQuestionRouteRequest {
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    pub fn outcome(&self) -> Option<&str> {
        self.outcome.as_deref()
    }

    pub fn into_parts(self) -> (String, Option<String>, Option<HashMap<String, String>>) {
        (self.tool_call_id, self.outcome, self.answers)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmitAskUserQuestionRouteResponse {
    ok: bool,
}

impl SubmitAskUserQuestionRouteResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionFileCompletionsRouteQuery {
    query: Option<String>,
    limit: Option<u32>,
}

impl SessionFileCompletionsRouteQuery {
    pub fn into_parts(self) -> (Option<String>, Option<u32>) {
        (self.query, self.limit)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionFileCompletionsRouteResponse(Vec<String>);

impl From<Vec<String>> for SessionFileCompletionsRouteResponse {
    fn from(completions: Vec<String>) -> Self {
        Self(completions)
    }
}

impl SessionFileCompletionsRouteResponse {
    pub fn new(completions: Vec<String>) -> Self {
        Self(completions)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionControlRouteErrorKind {
    BadRequest,
    NotFound,
    Forbidden,
    Conflict,
    InsufficientStorage,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionControlRouteError {
    kind: SessionControlRouteErrorKind,
    message: String,
}

impl SessionControlRouteError {
    pub fn new(kind: SessionControlRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::NotFound, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::Forbidden, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::Conflict, message)
    }

    pub fn insufficient_storage(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::InsufficientStorage, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(SessionControlRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> SessionControlRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
