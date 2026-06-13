use ctx_core::ids::SessionId;
use ctx_core::models::{ExecutionEnvironment, Session, Workspace};
use ctx_observability::logs;
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderAdapter;
use ctx_session_tools::model_resolution::{
    compose_model_id, normalize_effort_id, resolve_model_id,
};
use ctx_storage_admission::is_storage_exhaustion_error;

use super::model_target_bridge::SessionModelTargetLoadError;
use crate::daemon::SessionTitleModelModeHandle;

#[derive(Debug, Clone)]
pub struct SetSessionModelRequest {
    pub model_id: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SetSessionModelErrorKind {
    BadRequest,
    NotFound,
    Forbidden,
    InsufficientStorage,
    ProviderUnavailable,
    LiveSwitchRejected,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SetSessionModelError {
    kind: SetSessionModelErrorKind,
    message: String,
}

impl SetSessionModelError {
    pub(in crate::daemon::sessions) fn new(
        kind: SetSessionModelErrorKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(SetSessionModelErrorKind::BadRequest, message)
    }

    fn not_found(resource: &'static str) -> Self {
        Self::new(
            SetSessionModelErrorKind::NotFound,
            format!("{resource} not found"),
        )
    }

    fn provider_unavailable(error: impl std::fmt::Display) -> Self {
        Self::new(
            SetSessionModelErrorKind::ProviderUnavailable,
            logs::redact_sensitive(&format!("{error:#}")),
        )
    }

    fn live_switch_rejected(message: impl Into<String>) -> Self {
        Self::new(SetSessionModelErrorKind::LiveSwitchRejected, message)
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            SetSessionModelErrorKind::Internal,
            logs::redact_sensitive(&format!("{error:#}")),
        )
    }

    pub fn kind(&self) -> SetSessionModelErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

struct SessionModelTarget {
    session: Session,
    workspace: Workspace,
    execution_environment: ExecutionEnvironment,
    install_target: InstallTarget,
}

struct ResolvedSessionModelUpdate {
    model_id: String,
    reasoning_effort: Option<String>,
    full_model_id: String,
}

pub(super) async fn set_session_model_for_request(
    sessions: &SessionTitleModelModeHandle,
    session_id: SessionId,
    request: SetSessionModelRequest,
) -> Result<Session, SetSessionModelError> {
    let target = load_session_model_target(sessions, session_id).await?;
    let adapter = sessions
        .ensure_provider_adapter_for_target(&target.session.provider_id, target.install_target)
        .await
        .map_err(SetSessionModelError::provider_unavailable)?;
    let resolved_model = resolve_session_model_update(
        sessions,
        &target.workspace,
        &target.session,
        target.execution_environment,
        request,
    )
    .await?;
    switch_live_session_model(
        adapter.as_ref(),
        &target.session,
        &resolved_model.full_model_id,
    )
    .await?;

    sessions
        .persist_session_model_update_for_request(
            session_id,
            resolved_model.model_id,
            resolved_model.reasoning_effort,
            resolved_model.full_model_id,
        )
        .await
        .map_err(SetSessionModelError::internal)?
        .ok_or_else(|| SetSessionModelError::not_found("session"))
}

async fn load_session_model_target(
    sessions: &SessionTitleModelModeHandle,
    session_id: SessionId,
) -> Result<SessionModelTarget, SetSessionModelError> {
    let (session, workspace, execution_environment, install_target) = sessions
        .load_session_model_target_parts(session_id)
        .await
        .map_err(session_model_target_load_error)?;

    Ok(SessionModelTarget {
        session,
        workspace,
        execution_environment,
        install_target,
    })
}

fn session_model_target_load_error(error: SessionModelTargetLoadError) -> SetSessionModelError {
    match error {
        SessionModelTargetLoadError::NotFound(resource) => {
            SetSessionModelError::not_found(resource)
        }
        SessionModelTargetLoadError::ExecutionSettings(error) => execution_settings_error(error),
        SessionModelTargetLoadError::Internal(error) => SetSessionModelError::internal(error),
    }
}

fn execution_settings_error(error: anyhow::Error) -> SetSessionModelError {
    let kind = if ctx_settings_service::is_execution_policy_denial(&error) {
        SetSessionModelErrorKind::Forbidden
    } else if error
        .chain()
        .any(|cause| is_storage_exhaustion_error(&cause.to_string()))
    {
        SetSessionModelErrorKind::InsufficientStorage
    } else {
        SetSessionModelErrorKind::Internal
    };
    SetSessionModelError::new(kind, "failed to load execution settings")
}

async fn resolve_session_model_update(
    sessions: &SessionTitleModelModeHandle,
    workspace: &Workspace,
    session: &Session,
    execution_environment: ExecutionEnvironment,
    request: SetSessionModelRequest,
) -> Result<ResolvedSessionModelUpdate, SetSessionModelError> {
    let reasoning_effort = request
        .reasoning_effort
        .as_deref()
        .map(normalize_effort_id)
        .filter(|value| !value.is_empty());
    if let Some(ref effort) = reasoning_effort {
        let allowed = ["none", "minimal", "low", "medium", "high", "xhigh"];
        if !allowed.contains(&effort.as_str()) {
            return Err(SetSessionModelError::bad_request(format!(
                "unsupported reasoning effort '{effort}'"
            )));
        }
    }

    let catalog = sessions
        .load_provider_model_catalog_for_execution_environment(
            workspace,
            &session.provider_id,
            execution_environment,
        )
        .await
        .map_err(|error| SetSessionModelError::internal(anyhow::anyhow!(error)))?;
    let resolved_model = resolve_model_id(
        Some(request.model_id.as_str()),
        reasoning_effort.as_deref(),
        None,
        catalog.as_ref(),
    )
    .map_err(|error| {
        SetSessionModelError::bad_request(logs::redact_sensitive(&error.to_string()))
    })?;
    let full_model_id = compose_model_id(
        &resolved_model.model_id,
        resolved_model.reasoning_effort.as_deref(),
    );

    Ok(ResolvedSessionModelUpdate {
        model_id: resolved_model.model_id,
        reasoning_effort: resolved_model.reasoning_effort,
        full_model_id,
    })
}

async fn switch_live_session_model(
    adapter: &dyn ProviderAdapter,
    session: &Session,
    full_model_id: &str,
) -> Result<(), SetSessionModelError> {
    let session_id = session.id.0.to_string();
    if adapter.has_live_session(&session_id).await {
        adapter
            .set_session_model(session_id, full_model_id.to_string())
            .await
            .map_err(|error| {
                SetSessionModelError::live_switch_rejected(format!(
                    "failed to switch the live {} session to '{}': {}",
                    session.provider_id,
                    full_model_id,
                    logs::redact_sensitive(&error.to_string()),
                ))
            })?;
    }

    Ok(())
}
