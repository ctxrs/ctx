use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Session, SessionEvent, SessionSummary, Task, VcsKind, Workspace, Worktree,
};
use ctx_provider_install::install_state::{
    InstallId, InstallInfo, InstallProgressEvent, InstallTarget,
};
use ctx_provider_runtime::{provider_usage, CachedProviderOptions, CachedProviderVerify};
use ctx_providers::adapters::ProviderStatus;
use ctx_storage_admission::StorageGuardStatus;

use crate::daemon;

use super::{TestDaemon, TestMobileAccessForTest};

impl TestDaemon {
    pub async fn replace_provider_statuses(&self, statuses: HashMap<String, ProviderStatus>) {
        self.state
            .providers
            .replace_provider_statuses(statuses)
            .await;
    }

    pub async fn refresh_provider_statuses(&self) -> anyhow::Result<()> {
        ctx_managed_installs::refresh_provider_statuses(&self.provider_status_handle_for_test())
            .await
    }

    pub async fn upsert_provider_status(&self, provider_id: String, status: ProviderStatus) {
        self.state
            .providers
            .upsert_provider_status(provider_id, status)
            .await;
    }

    pub async fn seed_pending_codex_login_for_test(
        &self,
        account_id: &str,
        expected_callback_url: Option<&str>,
    ) -> String {
        daemon::providers::start_codex_login_session(
            &self.state.providers,
            account_id.to_string(),
            "https://chat.openai.com/oauth/authorize".to_string(),
            expected_callback_url.map(str::to_string),
        )
        .await
        .completion_token
    }

    pub async fn codex_login_completion_token_state_for_test(
        &self,
        account_id: &str,
    ) -> Option<Option<String>> {
        daemon::providers::codex_login_status(&self.state.providers, account_id)
            .await
            .map(|status| status.completion_token)
    }

    pub async fn persist_successful_codex_login_for_test(
        &self,
        account_id: &str,
        label: String,
        email: Option<String>,
        plan_type: Option<String>,
    ) -> anyhow::Result<()> {
        daemon::providers::persist_successful_codex_login(
            &self.state.core.data_root,
            &self.state.providers,
            account_id,
            label,
            email,
            plan_type,
        )
        .await
    }

    pub fn publish_storage_guard(&self, status: StorageGuardStatus) {
        self.state.test_publish_storage_guard(status);
    }

    pub async fn stop_mobile_tunnel(&self) {
        self.state.test_stop_mobile_tunnel().await;
    }

    pub fn mobile_access_for_test(&self) -> TestMobileAccessForTest<'_> {
        TestMobileAccessForTest { state: &self.state }
    }

    pub async fn issue_provider_session_mcp_token(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> String {
        daemon::issue_provider_session_mcp_token(&self.state, session_id, workspace_id, worktree_id)
            .await
    }

    pub async fn issue_provider_session_mcp_token_with_capabilities(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        capabilities: ctx_mcp_auth::McpAuthCapabilities,
    ) -> String {
        daemon::issue_provider_session_mcp_token_with_capabilities(
            &self.state,
            session_id,
            workspace_id,
            worktree_id,
            capabilities,
        )
        .await
    }

    pub async fn revoke_provider_session_mcp_token(&self, token: &str) -> bool {
        daemon::revoke_provider_session_mcp_token(&self.state, token).await
    }

    pub async fn test_with_provider_usage_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, provider_usage::ProviderUsageSnapshot>) -> R,
    ) -> R {
        self.state.test_with_provider_usage_cache(f).await
    }

    pub async fn test_with_provider_options_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, CachedProviderOptions>) -> R,
    ) -> R {
        self.state.test_with_provider_options_cache(f).await
    }

    pub async fn test_with_provider_verify_cache<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, CachedProviderVerify>) -> R,
    ) -> R {
        self.state.test_with_provider_verify_cache(f).await
    }

    pub async fn seed_provider_options_probe_cache_for_test(
        &self,
        key: &str,
        provider_id: &str,
        probe_ok: bool,
    ) {
        self.state
            .test_with_provider_options_cache(|cache| {
                cache.insert(
                    key.to_string(),
                    CachedProviderOptions {
                        cached_at: std::time::Instant::now(),
                        value: serde_json::json!({
                            "provider_id": provider_id,
                            "probe_ok": probe_ok,
                        }),
                    },
                );
            })
            .await;
    }

    pub async fn seed_host_session_model_catalog_cache_for_test(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        current_model_id: impl Into<String>,
        models: Vec<(String, String)>,
    ) {
        let current_model_id = current_model_id.into();
        let model_entries: Vec<serde_json::Value> = models
            .into_iter()
            .map(|(id, name)| {
                serde_json::json!({
                    "id": id,
                    "name": name,
                })
            })
            .collect();
        self.state
            .test_with_provider_options_cache(|cache| {
                cache.insert(
                    format!("{}/host/{provider_id}", workspace_id.0),
                    CachedProviderOptions {
                        cached_at: std::time::Instant::now(),
                        value: serde_json::json!({
                            "models": {
                                "models": model_entries,
                                "current_model_id": current_model_id,
                                "meta": {
                                    "source_kind": "subscription",
                                    "refresh_pending": false,
                                },
                            },
                        }),
                    },
                );
            })
            .await;
    }

    pub async fn provider_options_probe_cache_contains_for_test(&self, key: &str) -> bool {
        self.state
            .test_with_provider_options_cache(|cache| cache.contains_key(key))
            .await
    }

    pub async fn seed_provider_verify_cache_status_for_test(&self, key: &str, status: &str) {
        self.state
            .test_with_provider_verify_cache(|cache| {
                cache.insert(
                    key.to_string(),
                    CachedProviderVerify {
                        cached_at: std::time::Instant::now(),
                        value: serde_json::json!({ "status": status }),
                    },
                );
            })
            .await;
    }

    pub async fn provider_verify_cache_contains_for_test(&self, key: &str) -> bool {
        self.state
            .test_with_provider_verify_cache(|cache| cache.contains_key(key))
            .await
    }

    pub async fn seed_provider_usage_success_for_test(
        &self,
        provider_id: &str,
        source: &str,
        payload: serde_json::Value,
    ) {
        self.state
            .test_with_provider_usage_cache(|cache| {
                cache.insert(
                    provider_id.to_string(),
                    provider_usage::ProviderUsageSnapshot {
                        provider_id: provider_id.to_string(),
                        source: source.to_string(),
                        fetched_at: chrono::Utc::now(),
                        payload: Some(payload),
                        error: None,
                    },
                );
            })
            .await;
    }

    pub async fn seed_mcp_parent_session_for_test(
        &self,
        repo_path: &Path,
        base_commit: String,
        provider_id: &str,
        model_id: &str,
    ) -> anyhow::Result<Session> {
        let workspace: Workspace = self
            .state
            .global_store()
            .create_workspace(
                "test".into(),
                repo_path.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await?;
        let store = self.state.store_for_workspace(workspace.id).await?;
        let worktree: Worktree = store
            .create_worktree(
                workspace.id,
                repo_path.to_string_lossy().to_string(),
                base_commit,
                None,
            )
            .await?;
        let task: Task = store.create_task(workspace.id, "task".into(), None).await?;
        let session = store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Host,
                provider_id.into(),
                model_id.into(),
                "assistant".into(),
                None,
                None,
                None,
            )
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(session.id, workspace.id)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_worktree_index(worktree.id, workspace.id)
            .await?;
        self.state
            .global_store()
            .upsert_workspace_task_index(task.id, workspace.id)
            .await?;
        Ok(session)
    }

    pub async fn mcp_parent_session_events_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        self.state
            .store_for_session(session_id)
            .await?
            .list_session_events(session_id)
            .await
    }

    pub async fn mcp_subagent_sessions_for_test(
        &self,
        parent_session_id: SessionId,
    ) -> anyhow::Result<Vec<SessionSummary>> {
        self.state
            .store_for_session(parent_session_id)
            .await?
            .list_subagent_sessions(parent_session_id)
            .await
    }

    pub async fn start_install(
        &self,
        provider_id: String,
        target: Option<InstallTarget>,
    ) -> (InstallId, bool) {
        self.state.start_install(provider_id, target).await
    }

    pub async fn provider_target_start_tracked_install_for_test(
        &self,
        provider_id: String,
        target: Option<InstallTarget>,
    ) -> (InstallId, bool) {
        self.state.start_install(provider_id, target).await
    }

    pub async fn find_running_install(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Option<InstallId> {
        self.state.find_running_install(provider_id, target).await
    }

    pub async fn install_provider_with_progress(
        &self,
        install_id: InstallId,
        provider_id: String,
        target: InstallTarget,
    ) -> anyhow::Result<()> {
        let host: Arc<ctx_managed_installs::ManagedInstallHostObject> =
            Arc::new(self.provider_install_handle_for_test());
        ctx_managed_installs::install_provider_with_progress(host, install_id, provider_id, target)
            .await
    }

    pub async fn provider_target_install_with_progress_for_test(
        &self,
        install_id: InstallId,
        provider_id: String,
        target: InstallTarget,
    ) -> anyhow::Result<()> {
        let host: Arc<ctx_managed_installs::ManagedInstallHostObject> =
            Arc::new(self.provider_install_handle_for_test());
        ctx_managed_installs::install_provider_with_progress(host, install_id, provider_id, target)
            .await
    }

    pub async fn install_title_generation_local_with_progress(
        &self,
        install_id: InstallId,
    ) -> anyhow::Result<()> {
        let host: Arc<dyn ctx_managed_installs::title_generation::TitleGenerationLocalInstallHost> =
            Arc::new(self.provider_install_handle_for_test());
        ctx_managed_installs::install_title_generation_local_with_progress(host, install_id).await
    }

    pub async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent) {
        self.state.emit_install_event(install_id, event).await;
    }

    pub async fn get_install_info(&self, install_id: InstallId) -> Option<InstallInfo> {
        self.state.get_install_info(install_id).await
    }

    pub async fn provider_target_install_info_for_test(
        &self,
        install_id: InstallId,
    ) -> Option<InstallInfo> {
        self.state.get_install_info(install_id).await
    }

    pub async fn wait_for_provider_target_install_completion_for_test(
        &self,
        install_id: InstallId,
        timeout: Duration,
    ) -> anyhow::Result<InstallInfo> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let info = self
                .state
                .get_install_info(install_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("missing install info for {install_id}"))?;
            if !matches!(
                info.state,
                ctx_provider_install::install_state::InstallStateKind::Running
            ) {
                return Ok(info);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for install {install_id}: {info:#?}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn wait_for_provider_target_running_install_progress_for_test(
        &self,
        install_id: InstallId,
        timeout: Duration,
    ) -> anyhow::Result<InstallInfo> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let info = self
                .state
                .get_install_info(install_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("missing install info for {install_id}"))?;
            if matches!(
                info.state,
                ctx_provider_install::install_state::InstallStateKind::Running
            ) && info.last_event.is_some()
            {
                return Ok(info);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for running install {install_id} to expose real progress: {info:#?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn wait_for_provider_target_running_install_id_for_test(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
        timeout: Duration,
    ) -> anyhow::Result<InstallId> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(install_id) = self.state.find_running_install(provider_id, target).await {
                return Ok(install_id);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for running install {provider_id} with target {target:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn wait_for_provider_target_tracked_install_id_for_test(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
        timeout: Duration,
    ) -> anyhow::Result<InstallId> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(install_id) = self
                .state
                .test_tracked_install_ids(provider_id, target)
                .await
                .into_iter()
                .next()
            {
                return Ok(install_id);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for tracked install {provider_id} with target {target:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn tracked_install_ids(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Vec<InstallId> {
        self.state
            .test_tracked_install_ids(provider_id, target)
            .await
    }

    pub async fn provider_target_tracked_install_ids_for_test(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Vec<InstallId> {
        self.state
            .test_tracked_install_ids(provider_id, target)
            .await
    }

    pub async fn has_target_provider_adapter(&self, cache_key: &str) -> bool {
        self.state.test_has_target_provider_adapter(cache_key).await
    }

    pub async fn provider_target_has_adapter_cache_entry_for_test(&self, cache_key: &str) -> bool {
        self.state.test_has_target_provider_adapter(cache_key).await
    }

    pub async fn target_provider_adapter_cache_keys(&self) -> Vec<String> {
        self.state
            .test_target_provider_adapter_entries()
            .await
            .into_iter()
            .map(|(cache_key, _)| cache_key)
            .collect()
    }

    pub async fn provider_target_adapter_cache_keys_for_test(&self) -> Vec<String> {
        self.state
            .test_target_provider_adapter_entries()
            .await
            .into_iter()
            .map(|(cache_key, _)| cache_key)
            .collect()
    }

    pub async fn get_install_polling_info(&self, install_id: InstallId) -> Option<InstallInfo> {
        self.state.get_install_polling_info(install_id).await
    }

    pub async fn get_install_events(
        &self,
        install_id: InstallId,
    ) -> Option<Vec<InstallProgressEvent>> {
        self.state.get_install_events(install_id).await
    }

    pub async fn provider_login_session_caches_empty(&self) -> bool {
        let gemini = self
            .state
            .test_with_gemini_login_sessions(|map| map.is_empty())
            .await;
        let qwen = self
            .state
            .test_with_qwen_login_sessions(|map| map.is_empty())
            .await;
        let amp = self
            .state
            .test_with_amp_login_sessions(|map| map.is_empty())
            .await;
        let mistral = self
            .state
            .test_with_mistral_login_sessions(|map| map.is_empty())
            .await;
        let kimi = self
            .state
            .test_with_kimi_login_sessions(|map| map.is_empty())
            .await;
        let claude = self
            .state
            .test_with_claude_login_sessions(|map| map.is_empty())
            .await;
        let codex = self
            .state
            .test_with_codex_login_sessions(|map| map.is_empty())
            .await;
        let cursor = self
            .state
            .test_with_cursor_login_sessions(|map| map.is_empty())
            .await;
        gemini && qwen && amp && mistral && kimi && claude && codex && cursor
    }
}
