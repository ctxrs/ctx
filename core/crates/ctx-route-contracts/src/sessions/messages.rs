use ctx_core::models::{Message, MessageAttachment, MessageDelivery};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct PostSessionMessageRouteRequest {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    content: String,
    delivery: Option<MessageDelivery>,
    #[serde(default)]
    attachments: Vec<MessageAttachment>,
}

impl PostSessionMessageRouteRequest {
    pub fn into_parts(
        self,
    ) -> (
        Option<String>,
        Option<String>,
        String,
        Option<MessageDelivery>,
        Vec<MessageAttachment>,
    ) {
        (
            self.id,
            self.turn_id,
            self.content,
            self.delivery,
            self.attachments,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct PostSessionMessageRouteResponse(Message);

impl From<Message> for PostSessionMessageRouteResponse {
    fn from(message: Message) -> Self {
        Self(message)
    }
}

impl PostSessionMessageRouteResponse {
    pub fn new(message: Message) -> Self {
        Self(message)
    }
}

#[derive(Debug, Clone)]
pub struct DeleteSessionMessageRouteParams {
    session_id: String,
    message_id: String,
}

impl DeleteSessionMessageRouteParams {
    pub fn new(session_id: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            message_id: message_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn message_id(&self) -> &str {
        &self.message_id
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SessionMessageRouteErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    PayloadTooLarge,
    UnsupportedMediaType,
    ServiceUnavailable,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionMessageRouteError {
    kind: SessionMessageRouteErrorKind,
    message: String,
}

impl SessionMessageRouteError {
    pub fn new(kind: SessionMessageRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::Conflict, message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::PayloadTooLarge, message)
    }

    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::UnsupportedMediaType, message)
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::ServiceUnavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(SessionMessageRouteErrorKind::Internal, message)
    }

    pub fn kind(&self) -> SessionMessageRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
