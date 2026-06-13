use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, Session, SessionEventType, Workspace};
use ctx_provider_install::install_state::InstallTarget;
use ctx_providers::adapters::ProviderAdapter;
use ctx_store::Store;
use ctx_worktree_data_plane::{
    apply_data_plane_to_execution_settings,
    resolve_worktree_data_plane_with_host as resolve_worktree_data_plane, WorktreeDataPlaneHost,
};

use super::model_switch;
use crate::daemon::workspaces::ResolvedExistingWorktreeExecution;
use crate::daemon::SessionStoreAccessError;
use crate::daemon::SessionTitleModelModeHandle;

#[derive(Debug)]
pub enum SetSessionModeError {
    NotFound,
    BadRequest,
    Internal,
}

#[derive(Debug)]
pub(crate) enum SessionModelTargetLoadError {
    NotFound(&'static str),
    ExecutionSettings(anyhow::Error),
    Internal(anyhow::Error),
}

#[async_trait]
impl WorktreeDataPlaneHost for SessionTitleModelModeHandle {
    async fn get_workspace(
        handle: &Self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Workspace>> {
        handle.global_store().get_workspace(workspace_id).await
    }

    async fn workspace_store(handle: &Self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        handle.store_for_workspace(workspace_id).await
    }
}

impl SessionTitleModelModeHandle {
    pub async fn set_session_mode_for_request(
        &self,
        session_id: SessionId,
        mode_id: String,
    ) -> Result<(), SetSessionModeError> {
        let store = self
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_mode_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(|_| SetSessionModeError::Internal)?
            .ok_or(SetSessionModeError::NotFound)?;
        let worktree = store
            .get_worktree(session.worktree_id)
            .await
            .map_err(|_| SetSessionModeError::Internal)?
            .ok_or(SetSessionModeError::NotFound)?;
        let workspace = self
            .get_workspace(session.workspace_id)
            .await
            .map_err(|_| SetSessionModeError::Internal)?
            .ok_or(SetSessionModeError::NotFound)?;
        let resolved_worktree = self
            .resolve_existing_worktree_execution(&store, &workspace, worktree.id)
            .await
            .map_err(|_| SetSessionModeError::Internal)?;
        let execution_environment = resolved_worktree.execution_environment();
        if session.execution_environment != execution_environment {
            tracing::warn!(
                session_id = %session.id.0,
                stored = session.execution_environment.as_str(),
                resolved = execution_environment.as_str(),
                "session mode update resolved a different execution_environment than persisted metadata"
            );
        }
        let install_target = self
            .effective_install_target_for_environment(worktree.workspace_id, execution_environment)
            .await
            .map_err(|err| {
                tracing::warn!(
                    workspace_id = %worktree.workspace_id.0,
                    "set_session_mode failed to load execution settings: {err:#}",
                );
                SetSessionModeError::Internal
            })?;

        let adapter = self
            .ensure_provider_adapter_for_target(&session.provider_id, install_target)
            .await
            .map_err(|_| SetSessionModeError::Internal)?;

        adapter
            .set_session_mode(session.id.0.to_string(), mode_id.clone())
            .await
            .map_err(|_| SetSessionModeError::BadRequest)?;

        let event = store
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Init,
                serde_json::json!({"set_mode": mode_id}),
            )
            .await
            .map_err(|_| SetSessionModeError::Internal)?;
        self.publish_event(event).await;

        Ok(())
    }

    pub async fn set_session_model_for_request(
        &self,
        session_id: SessionId,
        request: model_switch::SetSessionModelRequest,
    ) -> Result<Session, model_switch::SetSessionModelError> {
        model_switch::set_session_model_for_request(self, session_id, request).await
    }

    pub(crate) async fn load_session_model_target_parts(
        &self,
        session_id: SessionId,
    ) -> Result<
        (Session, Workspace, ExecutionEnvironment, InstallTarget),
        SessionModelTargetLoadError,
    > {
        let store = self
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_model_target_error)?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(SessionModelTargetLoadError::Internal)?
            .ok_or(SessionModelTargetLoadError::NotFound("session"))?;
        let workspace = self
            .get_workspace(session.workspace_id)
            .await
            .map_err(SessionModelTargetLoadError::Internal)?
            .ok_or(SessionModelTargetLoadError::NotFound("workspace"))?;
        let worktree = store
            .get_worktree(session.worktree_id)
            .await
            .map_err(SessionModelTargetLoadError::Internal)?
            .ok_or(SessionModelTargetLoadError::NotFound("worktree"))?;
        let resolved_worktree = self
            .resolve_existing_worktree_execution(&store, &workspace, worktree.id)
            .await
            .map_err(SessionModelTargetLoadError::Internal)?;
        let execution_environment = resolved_worktree.execution_environment();
        if session.execution_environment != execution_environment {
            tracing::warn!(
                session_id = %session.id.0,
                stored = session.execution_environment.as_str(),
                resolved = execution_environment.as_str(),
                "session model update resolved a different execution_environment than persisted metadata"
            );
        }
        let install_target = self
            .effective_install_target_for_environment(worktree.workspace_id, execution_environment)
            .await
            .map_err(|err| {
                tracing::warn!(
                    workspace_id = %worktree.workspace_id.0,
                    "set_session_model failed to load execution settings: {err:#}",
                );
                SessionModelTargetLoadError::ExecutionSettings(err)
            })?;

        Ok((session, workspace, execution_environment, install_target))
    }

    pub async fn resolve_existing_worktree_execution(
        &self,
        store: &Store,
        _workspace: &Workspace,
        worktree_id: ctx_core::ids::WorktreeId,
    ) -> anyhow::Result<ResolvedExistingWorktreeExecution> {
        let worktree = store
            .get_worktree(worktree_id)
            .await?
            .ok_or_else(|| anyhow!("worktree not found"))?;
        let base_effective =
            ctx_settings_service::effective_execution_settings(self.global_store(), store)
                .await
                .context("loading workspace execution settings")?;
        let data_plane = resolve_worktree_data_plane(self, &worktree)
            .await
            .context("resolving worktree data plane")?;
        let effective = apply_data_plane_to_execution_settings(&base_effective, &data_plane)
            .context("applying worktree data plane to execution settings")?;
        Ok(ResolvedExistingWorktreeExecution {
            worktree,
            effective,
        })
    }

    pub(crate) async fn persist_session_model_update_for_request(
        &self,
        session_id: SessionId,
        model_id: String,
        reasoning_effort: Option<String>,
        full_model_id: String,
    ) -> Result<Option<Session>> {
        let Some(store) = self.session_store_for_write_or_none(session_id).await? else {
            return Ok(None);
        };
        store
            .update_session_model_config(session_id, model_id, reasoning_effort.clone())
            .await?;

        let Some(updated) = store.get_session(session_id).await? else {
            return Ok(None);
        };
        self.remember_session_meta(&updated).await;

        let event = store
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Init,
                serde_json::json!({
                    "current_model_id": full_model_id,
                    "reasoning_effort": reasoning_effort,
                }),
            )
            .await?;
        self.publish_event(event).await;

        if let Err(error) = self
            .update_workspace_provider_preferred_model_id(
                updated.workspace_id,
                &updated.provider_id,
                Some(full_model_id),
            )
            .await
        {
            tracing::warn!(
                session_id = %updated.id.0,
                workspace_id = %updated.workspace_id.0,
                provider_id = updated.provider_id.as_str(),
                "failed to persist workspace provider model preference after session model update: {error:#}"
            );
        }

        if let Err(error) = self.emit_workspace_task_upsert(updated.task_id).await {
            tracing::warn!(
                task_id = %updated.task_id.0,
                "workspace active snapshot refresh failed after session model update: {error:?}"
            );
        }

        Ok(Some(updated))
    }

    pub async fn effective_install_target_for_environment(
        &self,
        workspace_id: WorkspaceId,
        execution_environment: ExecutionEnvironment,
    ) -> anyhow::Result<InstallTarget> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_settings_service::effective_install_target_for_environment(
            self.global_store(),
            &store,
            execution_environment,
        )
        .await
    }

    pub(crate) async fn ensure_provider_adapter_for_target(
        &self,
        provider_id: &str,
        install_target: InstallTarget,
    ) -> anyhow::Result<Arc<dyn ProviderAdapter>> {
        ctx_provider_runtime::provider_launch::resolver::ensure_provider_adapter_for_target(
            self,
            provider_id,
            install_target,
        )
        .await
    }

    pub(crate) async fn update_workspace_provider_preferred_model_id(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        preferred_model_id: Option<String>,
    ) -> anyhow::Result<()> {
        let store = self.store_for_workspace(workspace_id).await?;
        ctx_workspace_config::update_preferred_new_session_model_id(
            &store,
            provider_id,
            preferred_model_id,
        )
        .await?;
        ctx_provider_runtime::provider_cache::invalidate_workspace_provider_options_cache(
            self.providers(),
            workspace_id,
            provider_id,
        )
        .await;
        Ok(())
    }
}

fn session_store_access_mode_error(error: SessionStoreAccessError) -> SetSessionModeError {
    match error {
        SessionStoreAccessError::NotFound => SetSessionModeError::NotFound,
        SessionStoreAccessError::LookupUnavailable(_)
        | SessionStoreAccessError::StoreUnavailable => SetSessionModeError::Internal,
    }
}

fn session_store_access_model_target_error(
    error: SessionStoreAccessError,
) -> SessionModelTargetLoadError {
    match error {
        SessionStoreAccessError::NotFound => SessionModelTargetLoadError::NotFound("session"),
        SessionStoreAccessError::LookupUnavailable(error) => {
            SessionModelTargetLoadError::Internal(error)
        }
        SessionStoreAccessError::StoreUnavailable => {
            SessionModelTargetLoadError::Internal(anyhow!("workspace store unavailable"))
        }
    }
}
