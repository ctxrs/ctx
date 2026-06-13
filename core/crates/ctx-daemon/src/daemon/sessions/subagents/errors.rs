use ctx_observability::logs;
use ctx_storage_admission::is_storage_exhaustion_error;

pub(in crate::daemon) type SubagentResult<T> = Result<T, SubagentError>;
pub(in crate::daemon) type ApiResult<T> = SubagentResult<T>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubagentErrorKind {
    BadRequest,
    NotFound,
    Forbidden,
    InsufficientStorage,
    Internal,
}

#[derive(Debug, Eq, PartialEq)]
pub struct SubagentError {
    kind: SubagentErrorKind,
    message: String,
}

impl SubagentError {
    pub fn kind(&self) -> SubagentErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(kind: SubagentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub(super) fn subagent_error(kind: SubagentErrorKind, error: impl Into<String>) -> SubagentError {
    SubagentError {
        kind,
        message: error.into(),
    }
}

pub(in crate::daemon) fn api_error(
    kind: SubagentErrorKind,
    error: impl Into<String>,
) -> SubagentError {
    subagent_error(kind, error)
}

pub(in crate::daemon) fn not_found(error: impl Into<String>) -> SubagentError {
    subagent_error(SubagentErrorKind::NotFound, error)
}

pub(super) fn internal_subagent_error(error: impl ToString) -> SubagentError {
    subagent_error(
        SubagentErrorKind::Internal,
        logs::redact_sensitive(&error.to_string()),
    )
}

pub(in crate::daemon) fn internal_api_error(error: impl ToString) -> SubagentError {
    internal_subagent_error(error)
}

pub(super) fn internal_request_or_policy_error(error: anyhow::Error) -> SubagentError {
    let kind = if ctx_settings_service::is_execution_policy_denial(&error) {
        SubagentErrorKind::Forbidden
    } else if error
        .chain()
        .any(|cause| is_storage_exhaustion_error(&cause.to_string()))
    {
        SubagentErrorKind::InsufficientStorage
    } else {
        SubagentErrorKind::Internal
    };
    subagent_error(kind, logs::redact_sensitive(&format!("{error:#}")))
}
