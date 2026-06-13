use ctx_core::ids::SessionId;

#[derive(Debug, Clone)]
pub struct SessionRouteParams {
    session_id: String,
}

impl SessionRouteParams {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Debug, Clone)]
pub struct SessionTurnToolsRouteParams {
    session_id: String,
    turn_id: String,
}

impl SessionTurnToolsRouteParams {
    pub fn new(session_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SessionRouteIdParseError;

impl std::fmt::Display for SessionRouteIdParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid session id")
    }
}

impl std::error::Error for SessionRouteIdParseError {}

pub fn parse_session_route_id(value: &str) -> Result<SessionId, SessionRouteIdParseError> {
    uuid::Uuid::parse_str(value)
        .map(SessionId)
        .map_err(|_| SessionRouteIdParseError)
}
