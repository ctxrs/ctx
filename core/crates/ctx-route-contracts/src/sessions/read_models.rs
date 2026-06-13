use ctx_core::ids::{SessionId, TurnId};
use ctx_core::models::{
    SessionEventsPage, SessionHeadSnapshot, SessionHistoryPage, SessionSnapshot, SessionState,
    SessionTurnTool,
};
use serde::{Deserialize, Serialize};

use super::common::parse_session_route_id;

pub const SESSION_EVENTS_DEFAULT_LIMIT: u32 = 200;
pub const SESSION_EVENTS_MAX_LIMIT: u32 = 1000;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionSnapshotRouteQuery {
    pub limit: Option<u32>,
    pub include_events: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionHeadRouteQuery {
    pub limit: Option<u32>,
    pub include_events: Option<String>,
    pub min_event_seq: Option<i64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub struct SessionHistoryRouteQuery {
    pub before_seq: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SessionEventsRouteQuery {
    pub after_seq: Option<i64>,
    pub limit: Option<u32>,
    pub tail: Option<u32>,
    pub include_transient: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionSnapshotRouteResponse(SessionSnapshot);

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionHeadRouteResponse(SessionHeadSnapshot);

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionHistoryRouteResponse(SessionHistoryPage);

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionEventsRouteResponse(SessionEventsPage);

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionStateRouteResponse(SessionState);

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SessionTurnToolsRouteResponse(Vec<SessionTurnTool>);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionReadModelRouteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionReadModelRouteError {
    kind: SessionReadModelRouteErrorKind,
    message: String,
}

impl SessionReadModelRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: SessionReadModelRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: SessionReadModelRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            kind: SessionReadModelRouteErrorKind::Conflict,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: SessionReadModelRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> SessionReadModelRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<SessionSnapshot> for SessionSnapshotRouteResponse {
    fn from(snapshot: SessionSnapshot) -> Self {
        Self(snapshot)
    }
}

impl From<SessionHeadSnapshot> for SessionHeadRouteResponse {
    fn from(head: SessionHeadSnapshot) -> Self {
        Self(head)
    }
}

impl From<SessionHistoryPage> for SessionHistoryRouteResponse {
    fn from(page: SessionHistoryPage) -> Self {
        Self(page)
    }
}

impl From<SessionEventsPage> for SessionEventsRouteResponse {
    fn from(page: SessionEventsPage) -> Self {
        Self(page)
    }
}

impl From<SessionState> for SessionStateRouteResponse {
    fn from(state: SessionState) -> Self {
        Self(state)
    }
}

impl From<Vec<SessionTurnTool>> for SessionTurnToolsRouteResponse {
    fn from(tools: Vec<SessionTurnTool>) -> Self {
        Self(tools)
    }
}

pub fn parse_session_id(value: &str) -> Result<SessionId, SessionReadModelRouteError> {
    parse_session_route_id(value)
        .map_err(|_| SessionReadModelRouteError::bad_request("invalid session id"))
}

pub fn parse_turn_id(value: &str) -> Result<TurnId, SessionReadModelRouteError> {
    uuid::Uuid::parse_str(value)
        .map(TurnId)
        .map_err(|_| SessionReadModelRouteError::bad_request("invalid turn id"))
}

pub fn parse_boolish_flag(
    raw: Option<&str>,
    label: &str,
) -> Result<bool, SessionReadModelRouteError> {
    match raw {
        Some(value) => ctx_core::boolish::parse_boolish(value).ok_or_else(|| {
            SessionReadModelRouteError::bad_request(format!(
                "{label} must be one of: 1/true/yes/on or 0/false/no/off"
            ))
        }),
        None => Ok(false),
    }
}
