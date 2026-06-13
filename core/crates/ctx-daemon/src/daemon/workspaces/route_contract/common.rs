use ctx_observability::logs;
use ctx_settings_service::EffectiveExecutionSettingsError;

use crate::daemon::RouteFileDownloadError;

use super::super::{
    FileCompletionsError, FileCompletionsErrorKind, WorkspaceDeleteError,
    WorkspaceHarnessContainerError, WorkspaceHydrationError, WorkspaceRouteError,
};

pub(super) fn workspace_hydration_route_error(
    error: WorkspaceHydrationError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceHydrationError::NotFound => WorkspaceRouteError::not_found("workspace not found"),
        WorkspaceHydrationError::Load(error) => {
            WorkspaceRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

pub(super) fn workspace_delete_route_error(error: WorkspaceDeleteError) -> WorkspaceRouteError {
    match error {
        WorkspaceDeleteError::NotFound => WorkspaceRouteError::not_found("workspace not found"),
        WorkspaceDeleteError::Internal => {
            WorkspaceRouteError::internal("failed to delete workspace")
        }
    }
}

pub(super) fn route_file_download_error(error: RouteFileDownloadError) -> WorkspaceRouteError {
    match error {
        RouteFileDownloadError::NotFound => WorkspaceRouteError::not_found("file not found"),
        RouteFileDownloadError::Internal => {
            WorkspaceRouteError::internal("failed to read route file")
        }
    }
}

pub(in crate::daemon::workspaces) fn workspace_harness_container_status_error(
    error: WorkspaceHarnessContainerError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceHarnessContainerError::NotFound => {
            WorkspaceRouteError::not_found("workspace not found")
        }
        WorkspaceHarnessContainerError::Internal(_)
        | WorkspaceHarnessContainerError::ExecutionSettings(_)
        | WorkspaceHarnessContainerError::Ensure(_) => {
            WorkspaceRouteError::internal("workspace harness container request failed")
        }
    }
}

pub(super) fn effective_execution_settings_route_error(
    error: EffectiveExecutionSettingsError,
) -> WorkspaceRouteError {
    match error {
        EffectiveExecutionSettingsError::InvalidWorkspaceOverride(error) => {
            let message = logs::redact_sensitive(&error.to_string());
            if ctx_settings_service::is_execution_policy_denial(&error) {
                WorkspaceRouteError::forbidden(message)
            } else {
                WorkspaceRouteError::bad_request(message)
            }
        }
        EffectiveExecutionSettingsError::Internal(error) => {
            WorkspaceRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

pub(in crate::daemon::workspaces) fn workspace_harness_container_ensure_error(
    error: WorkspaceHarnessContainerError,
) -> WorkspaceRouteError {
    match error {
        WorkspaceHarnessContainerError::NotFound => {
            WorkspaceRouteError::not_found("workspace not found")
        }
        WorkspaceHarnessContainerError::Internal(error) => {
            WorkspaceRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
        WorkspaceHarnessContainerError::ExecutionSettings(error) => {
            effective_execution_settings_route_error(error)
        }
        WorkspaceHarnessContainerError::Ensure(error) => {
            WorkspaceRouteError::bad_request(logs::redact_sensitive(&error.to_string()))
        }
    }
}

pub(in crate::daemon::workspaces) fn file_completions_route_error(
    error: FileCompletionsError,
) -> WorkspaceRouteError {
    match error.kind() {
        FileCompletionsErrorKind::NotFound => WorkspaceRouteError::not_found(error.message()),
        FileCompletionsErrorKind::Forbidden => WorkspaceRouteError::forbidden(error.message()),
        FileCompletionsErrorKind::InsufficientStorage => {
            WorkspaceRouteError::insufficient_storage(error.message())
        }
        FileCompletionsErrorKind::Internal => {
            tracing::warn!(error = error.message(), "file completions request failed");
            WorkspaceRouteError::internal(error.message())
        }
    }
}
