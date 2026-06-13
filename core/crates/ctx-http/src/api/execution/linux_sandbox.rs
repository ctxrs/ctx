use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use ctx_execution_runtime::route_contract::{
    LinuxSandboxRuntimeError, LinuxSandboxRuntimePrepareRequest, LinuxSandboxRuntimePrepareResult,
    LinuxSandboxRuntimeStatus,
};

use ctx_daemon::daemon::LinuxSandboxRuntimeHandle;

use crate::api::errors::ApiErrorResp;

pub(in crate::api) async fn linux_sandbox_runtime_status_api(
    State(execution): State<LinuxSandboxRuntimeHandle>,
) -> Result<Json<LinuxSandboxRuntimeStatus>, (StatusCode, Json<ApiErrorResp>)> {
    let status = execution
        .linux_sandbox_runtime_status()
        .await
        .map_err(map_linux_sandbox_runtime_error)?;
    Ok(Json(status))
}

pub(in crate::api) async fn linux_sandbox_runtime_stage(
    State(execution): State<LinuxSandboxRuntimeHandle>,
) -> Result<Json<LinuxSandboxRuntimeStatus>, (StatusCode, Json<ApiErrorResp>)> {
    let status = execution
        .stage_linux_sandbox_runtime()
        .await
        .map_err(map_linux_sandbox_runtime_error)?;
    Ok(Json(status))
}

pub(in crate::api) async fn linux_sandbox_runtime_prepare(
    State(execution): State<LinuxSandboxRuntimeHandle>,
    Json(req): Json<LinuxSandboxRuntimePrepareRequest>,
) -> Result<Json<LinuxSandboxRuntimePrepareResult>, (StatusCode, Json<ApiErrorResp>)> {
    let result = execution
        .prepare_linux_sandbox_runtime(req.activation_mode, req.sudo_password.as_deref())
        .await
        .map_err(map_linux_sandbox_runtime_error)?;
    Ok(Json(result))
}

fn map_linux_sandbox_runtime_error(
    error: LinuxSandboxRuntimeError,
) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match &error {
        LinuxSandboxRuntimeError::PrepareAlreadyActive
        | LinuxSandboxRuntimeError::PrepareSandboxWorkActive => StatusCode::CONFLICT,
        LinuxSandboxRuntimeError::Runtime { .. }
        | LinuxSandboxRuntimeError::PrepareActivityUnavailable { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = error.message().to_string();
    (status, Json(ApiErrorResp { error: message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_execution_runtime::route_contract::LinuxSandboxRuntimeOperation;

    #[test]
    fn maps_linux_sandbox_prepare_conflicts() {
        let (status, body) =
            map_linux_sandbox_runtime_error(LinuxSandboxRuntimeError::PrepareAlreadyActive);
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.0.error.contains("already in progress"));

        let (status, body) =
            map_linux_sandbox_runtime_error(LinuxSandboxRuntimeError::PrepareSandboxWorkActive);
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.0.error.contains("sandbox work is active"));
    }

    #[test]
    fn maps_linux_sandbox_runtime_errors() {
        let (status, body) =
            map_linux_sandbox_runtime_error(LinuxSandboxRuntimeError::PrepareActivityUnavailable {
                message: "Preparing Linux sandbox runtime failed".to_string(),
            });
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.error, "Preparing Linux sandbox runtime failed");

        let (status, body) = map_linux_sandbox_runtime_error(LinuxSandboxRuntimeError::Runtime {
            operation: LinuxSandboxRuntimeOperation::Status,
            message: "Linux sandbox runtime status check failed".to_string(),
        });
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.error, "Linux sandbox runtime status check failed");
    }
}
