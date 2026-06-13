use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use ctx_execution_runtime::{
    route_contract::{StartExecutionLaunchError, StartExecutionLaunchRequest},
    ExecutionLaunchSnapshot,
};

use ctx_daemon::daemon::ExecutionLaunchHandle;

use super::errors::ApiErrorResp;

mod launch_stream;
mod linux_sandbox;

pub(super) use launch_stream::{launch_status, launch_stream_ws};
pub(super) use linux_sandbox::{
    linux_sandbox_runtime_prepare, linux_sandbox_runtime_stage, linux_sandbox_runtime_status_api,
};

pub(super) async fn launch_start(
    State(execution): State<ExecutionLaunchHandle>,
    Json(req): Json<StartExecutionLaunchRequest>,
) -> Result<Json<ExecutionLaunchSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    let snapshot = execution
        .start_execution_launch_for_request(req)
        .await
        .map_err(map_start_execution_launch_error)?;
    Ok(Json(snapshot))
}

fn map_start_execution_launch_error(
    error: StartExecutionLaunchError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let (status, message) = match error {
        StartExecutionLaunchError::MissingWorkspaceId => (
            StatusCode::BAD_REQUEST,
            "workspace_id is required for workspace_launch".to_string(),
        ),
        StartExecutionLaunchError::InvalidWorkspaceId => {
            (StatusCode::BAD_REQUEST, "invalid workspace id".to_string())
        }
        StartExecutionLaunchError::WorkspaceNotFound => {
            (StatusCode::NOT_FOUND, "workspace not found".to_string())
        }
        StartExecutionLaunchError::MaintenanceActive { message } => (StatusCode::CONFLICT, message),
        StartExecutionLaunchError::InvalidWorkspaceExecutionSettings {
            message,
            policy_denial,
        } => (
            if policy_denial {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            },
            message,
        ),
        StartExecutionLaunchError::Internal { message } => {
            (StatusCode::INTERNAL_SERVER_ERROR, message)
        }
    };
    (status, Json(ApiErrorResp { error: message }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_workspace_launch_contract_errors() {
        let (status, body) =
            map_start_execution_launch_error(StartExecutionLaunchError::MissingWorkspaceId);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body.0.error,
            "workspace_id is required for workspace_launch"
        );

        let (status, body) =
            map_start_execution_launch_error(StartExecutionLaunchError::InvalidWorkspaceId);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.0.error, "invalid workspace id");

        let (status, body) =
            map_start_execution_launch_error(StartExecutionLaunchError::WorkspaceNotFound);
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.0.error, "workspace not found");
    }

    #[test]
    fn maps_execution_settings_contract_errors() {
        let (status, body) = map_start_execution_launch_error(
            StartExecutionLaunchError::InvalidWorkspaceExecutionSettings {
                message: "bad settings".to_string(),
                policy_denial: false,
            },
        );
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.0.error, "bad settings");

        let (status, body) = map_start_execution_launch_error(
            StartExecutionLaunchError::InvalidWorkspaceExecutionSettings {
                message: "policy".to_string(),
                policy_denial: true,
            },
        );
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.0.error, "policy");
    }

    #[test]
    fn maps_maintenance_and_internal_contract_errors() {
        let (status, body) =
            map_start_execution_launch_error(StartExecutionLaunchError::MaintenanceActive {
                message: "maintenance".to_string(),
            });
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.0.error, "maintenance");

        let (status, body) =
            map_start_execution_launch_error(StartExecutionLaunchError::Internal {
                message: "internal".to_string(),
            });
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.error, "internal");
    }
}
