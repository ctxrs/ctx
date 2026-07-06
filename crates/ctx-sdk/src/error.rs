use ctx_protocol::{AgentHistoryErrorBody, AgentHistoryErrorCode};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{body:?}")]
pub struct AgentHistoryError {
    pub body: AgentHistoryErrorBody,
}

impl AgentHistoryError {
    pub(crate) fn new(
        code: AgentHistoryErrorCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            body: AgentHistoryErrorBody::new(code, message, retryable),
        }
    }

    pub(crate) fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.body.cause = Some(cause.into());
        self
    }
}
