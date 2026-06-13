pub(super) use ctx_route_contracts::tasks::{TaskRouteError, TaskRouteErrorKind, TaskRouteParams};

use ctx_observability::logs;

use crate::daemon::WorkspaceStoreAccessError;

use super::super::{TaskCreateError, TaskLifecycleError, TaskSessionCreateError};

pub(super) fn task_route_error_from_workspace_store(
    error: WorkspaceStoreAccessError,
) -> TaskRouteError {
    match error {
        WorkspaceStoreAccessError::NotFound => TaskRouteError::not_found("workspace not found"),
        WorkspaceStoreAccessError::Unavailable(error) => {
            tracing::warn!("workspace store unavailable for task route: {error:#}");
            TaskRouteError::internal("workspace store unavailable")
        }
    }
}

pub(super) fn task_route_error_from_task_create(error: TaskCreateError) -> TaskRouteError {
    match error {
        TaskCreateError::BadRequest(error) => TaskRouteError::bad_request(error),
        TaskCreateError::NotFound(error) => TaskRouteError::not_found(error),
        TaskCreateError::Conflict(error) => TaskRouteError::conflict(error),
        TaskCreateError::Internal(error) => {
            let message = logs::redact_sensitive(&error.to_string());
            classified_internal_route_error(&error, message)
        }
        TaskCreateError::DefaultSessionFailed(error) => {
            let kind = task_route_error_from_task_session_create(error).kind();
            match kind {
                TaskRouteErrorKind::BadRequest => {
                    TaskRouteError::bad_request("failed to create default session")
                }
                TaskRouteErrorKind::NotFound => {
                    TaskRouteError::not_found("failed to create default session")
                }
                TaskRouteErrorKind::Conflict => {
                    TaskRouteError::conflict("failed to create default session")
                }
                TaskRouteErrorKind::Forbidden => {
                    TaskRouteError::forbidden("failed to create default session")
                }
                TaskRouteErrorKind::InsufficientStorage => {
                    TaskRouteError::insufficient_storage("failed to create default session")
                }
                TaskRouteErrorKind::Internal => {
                    TaskRouteError::internal("failed to create default session")
                }
            }
        }
        TaskCreateError::DefaultSessionConflict => {
            TaskRouteError::conflict("task id already exists with a different default session")
        }
    }
}

pub(super) fn task_route_error_from_task_session_create(
    error: TaskSessionCreateError,
) -> TaskRouteError {
    match error {
        TaskSessionCreateError::BadRequest => TaskRouteError::bad_request("bad request"),
        TaskSessionCreateError::NotFound => TaskRouteError::not_found("task not found"),
        TaskSessionCreateError::Conflict => TaskRouteError::conflict("session conflict"),
        TaskSessionCreateError::Internal(error) => {
            tracing::warn!("task session creation failed: {error:#}");
            let message = logs::redact_sensitive(&error.to_string());
            classified_internal_route_error(&error, message)
        }
    }
}

pub(super) fn task_route_error_from_task_lifecycle(error: TaskLifecycleError) -> TaskRouteError {
    match error {
        TaskLifecycleError::NotFound => TaskRouteError::not_found("task not found"),
        TaskLifecycleError::Internal(error) => {
            tracing::warn!("task lifecycle operation failed: {error:#}");
            classified_internal_route_error(&error, "task lifecycle operation failed")
        }
    }
}

pub(super) fn classified_internal_route_error(
    error: &anyhow::Error,
    message: impl Into<String>,
) -> TaskRouteError {
    match route_error_kind_for_internal_error(error) {
        TaskRouteErrorKind::Forbidden => TaskRouteError::forbidden(message),
        TaskRouteErrorKind::InsufficientStorage => TaskRouteError::insufficient_storage(message),
        _ => TaskRouteError::internal(message),
    }
}

pub(super) fn route_error_kind_for_internal_error(error: &anyhow::Error) -> TaskRouteErrorKind {
    if ctx_settings_service::is_execution_policy_denial(error) {
        TaskRouteErrorKind::Forbidden
    } else if error
        .chain()
        .any(|cause| ctx_storage_admission::is_storage_exhaustion_error(&cause.to_string()))
    {
        TaskRouteErrorKind::InsufficientStorage
    } else {
        TaskRouteErrorKind::Internal
    }
}
