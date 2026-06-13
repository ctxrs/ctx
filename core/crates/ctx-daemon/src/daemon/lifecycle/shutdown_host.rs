use std::sync::Arc;

use anyhow::{anyhow, Result};
use ctx_core::ids::SessionId;
use ctx_core::models::{SessionEvent, SessionTurnStatus};
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext;
use ctx_store::{Store, StoreManager};
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_runtime::HarnessRuntimeManager;
use tokio::sync::broadcast;

use crate::daemon::activity::{
    collect_turns_by_statuses_parts, daemon_turn_activity_summary_parts,
};
use crate::daemon::scheduler::{
    reconcile_turn_terminal_state_with_host, SchedulerCommand, TerminalStateReconcileHost,
};
use crate::daemon::state::{SessionStoreLookup, StoreLookup};
use crate::daemon::task_session_effects::SessionPublicationEffects;
use crate::daemon::DaemonTurnActivitySummary;

#[derive(Clone)]
pub(in crate::daemon) struct DaemonShutdownHost {
    global_store: Store,
    stores: StoreManager,
    session_stores: SessionStoreLookup,
    session_lifecycle: Arc<SessionRuntime<SchedulerCommand>>,
    session_publication: SessionPublicationEffects,
    provider_lifecycle: Arc<ProviderRuntime>,
    update_drain: Arc<UpdateDrainCoordinator>,
    substrate_lifecycle: Arc<HarnessRuntimeManager>,
    shutdown_signal: broadcast::Sender<()>,
}

pub(in crate::daemon) struct DaemonShutdownHostParts {
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) stores: StoreManager,
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) session_lifecycle: Arc<SessionRuntime<SchedulerCommand>>,
    pub(in crate::daemon) session_publication: SessionPublicationEffects,
    pub(in crate::daemon) provider_lifecycle: Arc<ProviderRuntime>,
    pub(in crate::daemon) update_drain: Arc<UpdateDrainCoordinator>,
    pub(in crate::daemon) substrate_lifecycle: Arc<HarnessRuntimeManager>,
    pub(in crate::daemon) shutdown_signal: broadcast::Sender<()>,
}

impl DaemonShutdownHost {
    pub(in crate::daemon) fn new(parts: DaemonShutdownHostParts) -> Self {
        Self {
            global_store: parts.global_store,
            stores: parts.stores,
            session_stores: parts.session_stores,
            session_lifecycle: parts.session_lifecycle,
            session_publication: parts.session_publication,
            provider_lifecycle: parts.provider_lifecycle,
            update_drain: parts.update_drain,
            substrate_lifecycle: parts.substrate_lifecycle,
            shutdown_signal: parts.shutdown_signal,
        }
    }

    pub(in crate::daemon) async fn acquire_shutdown_drain(&self, reason: &str) -> bool {
        self.update_drain
            .acquire(reason, "daemon_shutdown")
            .await
            .is_some()
    }

    pub(in crate::daemon) async fn release_shutdown_drain_if_owned(&self, acquired: bool) {
        if acquired {
            let _ = self.update_drain.release().await;
        }
    }

    pub(in crate::daemon) async fn turn_activity_summary(
        &self,
    ) -> Result<DaemonTurnActivitySummary> {
        daemon_turn_activity_summary_parts(
            &self.global_store,
            &self.stores,
            self.update_drain.as_ref(),
        )
        .await
    }

    pub(in crate::daemon) async fn interrupt_running_sessions(&self) {
        for session_id in self.session_lifecycle.list_running_sessions().await {
            let Some(tx) = self.session_lifecycle.scheduler_sender(session_id).await else {
                continue;
            };
            let interrupt = InterruptTelemetryContext::new(uuid::Uuid::new_v4().to_string());
            let _ = tx.send(SchedulerCommand::Interrupt(interrupt)).await;
        }
    }

    pub(in crate::daemon) async fn reconcile_running_turns_with_reason(
        &self,
        fallback_reason: &str,
    ) -> Result<()> {
        let (_, running_turns) = collect_turns_by_statuses_parts(
            &self.global_store,
            &self.stores,
            &[SessionTurnStatus::Starting, SessionTurnStatus::Running],
        )
        .await?;

        for (_, turn) in running_turns {
            if let Err(err) = reconcile_turn_terminal_state_with_host(
                self,
                turn.session_id,
                turn.run_id,
                turn.turn_id,
                fallback_reason,
            )
            .await
            {
                tracing::warn!(
                    session_id = %turn.session_id.0,
                    turn_id = %turn.turn_id.0,
                    err = %err,
                    "failed to reconcile running turn after daemon shutdown"
                );
            }
        }

        Ok(())
    }

    pub(in crate::daemon) async fn shutdown_provider_adapters(&self, reason: &str) {
        self.provider_lifecycle
            .shutdown_provider_adapters(reason)
            .await;
    }

    pub(in crate::daemon) async fn save_or_stop_selected_shared_substrate(
        &self,
    ) -> Result<Option<ctx_avf_linux_runtime::SubstrateLifecycleRecord>> {
        self.substrate_lifecycle
            .save_or_stop_selected_shared_substrate()
            .await
    }

    pub(in crate::daemon) fn broadcast_shutdown(&self) {
        let _ = self.shutdown_signal.send(());
    }

    async fn set_session_running(&self, session_id: SessionId, running: bool) {
        if let Some(pinned) = self
            .session_lifecycle
            .set_running(session_id, running)
            .await
        {
            self.provider_lifecycle
                .set_provider_session_pinned(session_id.0.to_string(), pinned)
                .await;
        }
    }
}

#[async_trait::async_trait]
impl TerminalStateReconcileHost for DaemonShutdownHost {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        match self.session_stores.lookup_session_store(session_id).await {
            StoreLookup::Found(store) => Ok(store),
            StoreLookup::Missing | StoreLookup::Deleting => {
                Err(anyhow!("workspace missing for session {}", session_id.0))
            }
            StoreLookup::Unavailable(err) => Err(err),
        }
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.session_publication.publish_event(event).await;
    }

    async fn set_running(&self, session_id: SessionId, running: bool) {
        self.set_session_running(session_id, running).await;
    }
}
