use std::sync::Arc;
use std::time::Duration;

use ctx_core::ids::{TerminalId, WorkspaceId};
use ctx_core::models::{Session, SessionHeadDelta, Workspace, Worktree, WorktreeAttachmentMount};
use ctx_settings_model::{ExecutionSettings, Settings};

use crate::daemon::{self, workspace_attachments_runtime_from_state};

use super::TestDaemon;

impl TestDaemon {
    pub async fn terminal_output_snapshot(&self, terminal_id: TerminalId) -> Option<Vec<u8>> {
        self.state
            .test_terminal_handle(terminal_id)
            .await
            .map(|handle| handle.output_snapshot())
    }

    pub async fn remember_session_meta(&self, session: &Session) {
        self.state.sessions.remember_session_meta(session).await;
    }

    pub async fn publish_session_head_delta(
        &self,
        session: &Session,
        delta: SessionHeadDelta,
        bump_snapshot: bool,
    ) {
        self.state
            .test_publish_session_head_delta(session, delta, bump_snapshot)
            .await;
    }

    pub async fn set_provider_inactivity_timeout(&self, timeout: Duration) {
        self.state
            .test_set_provider_inactivity_timeout(timeout)
            .await;
    }

    pub async fn apply_provider_monitoring_settings_for_test(
        &self,
        settings: &Settings,
    ) -> anyhow::Result<()> {
        daemon::provider_guard::apply_settings_parts(
            self.state.providers.as_ref(),
            self.state.telemetry.resource_sampler.as_ref(),
            settings,
        )
        .await?;
        daemon::provider_restart::apply_settings_parts(
            self.state.providers.as_ref(),
            self.state.telemetry.resource_sampler.as_ref(),
            settings,
        )
        .await?;
        Ok(())
    }

    pub fn spawn_provider_monitoring_for_test(&self) {
        daemon::resource_telemetry::spawn_resource_telemetry(Arc::clone(&self.state));
        daemon::provider_guard::spawn_provider_guard(Arc::clone(
            &self.state.provider_lifecycle_background,
        ));
        daemon::provider_restart::spawn_provider_restart(Arc::clone(
            &self.state.provider_lifecycle_background,
        ));
    }

    pub async fn prepare_workspace_harness_for_test(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution_settings: &ExecutionSettings,
    ) -> anyhow::Result<()> {
        self.state
            .test_prepare_harness(workspace, worktree, execution_settings)
            .await
            .map(|_| ())
    }

    pub async fn workspace_harness_egress_guard_for_test(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<bool>> {
        Ok(self
            .state
            .test_harness_container_status(workspace_id)
            .await?
            .and_then(|status| status.egress_guard))
    }

    pub async fn materialize_workspace_attachments_for_test(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        configs: impl IntoIterator<Item = ctx_workspace_attachments::AttachmentConfig>,
    ) -> anyhow::Result<Vec<WorktreeAttachmentMount>> {
        let attachment_runtime = workspace_attachments_runtime_from_state(&self.state);
        for config in configs {
            attachment_runtime
                .upsert_workspace_attachment(workspace.id, config)
                .await?;
        }

        let sync = ctx_workspace_attachments::sync_workspace_attachments(
            attachment_runtime.as_ref(),
            workspace,
            false,
        )
        .await?;
        for plan in sync.plans {
            ctx_workspace_attachments::run_attachment_materialization(
                attachment_runtime.as_ref(),
                workspace,
                plan.id,
                plan.refresh,
            )
            .await?;
        }

        attachment_runtime
            .ensure_worktree_attachment_mounts_if_materialized(workspace, worktree)
            .await
    }
}
