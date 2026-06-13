use std::sync::Arc;
use std::time::Duration;

use anyhow::Error;
use ctx_store::{Store, StoreManager};
use ctx_transport_runtime::terminals::TerminalManager;
use ctx_update_service::route_contract::{
    BeginUpdateDrainRouteRequest, BeginUpdateDrainRouteResult, MaintenanceRouteError,
    ReleaseUpdateDrainRouteRequest, ReleaseUpdateDrainRouteResult, ShutdownDaemonRouteRequest,
    ShutdownDaemonRouteResult,
};
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::activity::{
    daemon_sandbox_work_activity_summary_parts, daemon_turn_activity_summary_parts,
};
use crate::daemon::{
    spawn_deferred_daemon_shutdown, DaemonSandboxWorkActivitySummary, DaemonShutdownHandle,
    DaemonShutdownHost, DaemonState, DaemonTurnActivitySummary, LinuxSandboxRuntimeHandle,
    UpdateDrainHandle,
};

pub struct MaintenanceDrainPermit {
    update_drain: Arc<UpdateDrainCoordinator>,
    released: bool,
}

impl MaintenanceDrainPermit {
    pub async fn release(mut self) -> bool {
        if self.released {
            return false;
        }
        let released = self.update_drain.release().await;
        self.released = true;
        released
    }
}

impl Drop for MaintenanceDrainPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let update_drain = Arc::clone(&self.update_drain);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = update_drain.release().await;
                });
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "maintenance drain permit dropped without a tokio runtime; drain may remain active"
                );
            }
        }
    }
}

#[derive(Debug)]
pub enum BeginUpdateDrainError {
    AlreadyActive,
    ActivityUnavailable(Error),
    Busy,
}

#[derive(Debug)]
pub enum MaintenanceDrainError {
    AlreadyActive,
    ActivityUnavailable(Error),
    SandboxWorkActive,
}

#[derive(Debug)]
pub enum DaemonShutdownError {
    ActivityUnavailable(Error),
    Reconcile(Error),
}

pub async fn begin_update_drain(
    state: &Arc<DaemonState>,
    reason: String,
    owner: String,
) -> Result<DaemonTurnActivitySummary, BeginUpdateDrainError> {
    begin_update_drain_parts(
        state.global_store(),
        &state.core.stores,
        state.core.update_drain.as_ref(),
        reason,
        owner,
    )
    .await
}

pub(in crate::daemon) async fn begin_update_drain_parts(
    global_store: &Store,
    stores: &StoreManager,
    update_drain: &UpdateDrainCoordinator,
    reason: String,
    owner: String,
) -> Result<DaemonTurnActivitySummary, BeginUpdateDrainError> {
    if update_drain.acquire(reason, owner).await.is_none() {
        return Err(BeginUpdateDrainError::AlreadyActive);
    }

    let activity =
        match daemon_turn_activity_summary_parts(global_store, stores, update_drain).await {
            Ok(activity) => activity,
            Err(error) => {
                let _ = update_drain.release().await;
                return Err(BeginUpdateDrainError::ActivityUnavailable(error));
            }
        };
    if turn_activity_blocks_update_drain(&activity) {
        let _ = update_drain.release().await;
        return Err(BeginUpdateDrainError::Busy);
    }
    Ok(activity)
}

pub async fn release_update_drain(state: &DaemonState) -> bool {
    release_update_drain_parts(state.core.update_drain.as_ref()).await
}

pub(in crate::daemon) async fn release_update_drain_parts(
    update_drain: &UpdateDrainCoordinator,
) -> bool {
    update_drain.release().await
}

pub async fn reject_new_execution_during_maintenance(state: &DaemonState) -> Result<(), Error> {
    reject_new_execution_during_maintenance_parts(state.core.update_drain.as_ref()).await
}

pub(in crate::daemon) async fn reject_new_execution_during_maintenance_parts(
    update_drain: &UpdateDrainCoordinator,
) -> Result<(), Error> {
    update_drain.reject_if_draining().await
}

pub async fn post_message_update_drain_reason(state: &DaemonState) -> Option<String> {
    state
        .core
        .update_drain
        .snapshot()
        .await
        .map(|drain| drain.reason)
}

pub async fn acquire_linux_sandbox_prepare_drain(
    state: &Arc<DaemonState>,
) -> Result<MaintenanceDrainPermit, MaintenanceDrainError> {
    acquire_linux_sandbox_prepare_drain_parts(
        Arc::clone(&state.core.update_drain),
        state.global_store(),
        &state.core.stores,
        state.transport.terminals.as_ref(),
        state.execution.harness.as_ref(),
    )
    .await
}

pub(in crate::daemon) async fn acquire_linux_sandbox_prepare_drain_parts(
    update_drain: Arc<UpdateDrainCoordinator>,
    global_store: &Store,
    stores: &StoreManager,
    terminals: &TerminalManager,
    harness: &HarnessRuntimeManager,
) -> Result<MaintenanceDrainPermit, MaintenanceDrainError> {
    if update_drain
        .acquire("linux_sandbox_runtime_prepare", "execution_api")
        .await
        .is_none()
    {
        return Err(MaintenanceDrainError::AlreadyActive);
    }
    let permit = MaintenanceDrainPermit {
        update_drain,
        released: false,
    };

    let activity =
        match daemon_sandbox_work_activity_summary_parts(global_store, stores, terminals, harness)
            .await
        {
            Ok(activity) => activity,
            Err(error) => {
                let _ = permit.release().await;
                return Err(MaintenanceDrainError::ActivityUnavailable(error));
            }
        };
    if sandbox_work_is_active(&activity) {
        let _ = permit.release().await;
        return Err(MaintenanceDrainError::SandboxWorkActive);
    }

    Ok(permit)
}

pub(in crate::daemon) async fn request_daemon_shutdown(
    host: &DaemonShutdownHost,
    reason: String,
) -> Result<DaemonTurnActivitySummary, DaemonShutdownError> {
    let acquired_drain = host.acquire_shutdown_drain(&reason).await;

    host.interrupt_running_sessions().await;

    for _ in 0..10 {
        let activity = match host.turn_activity_summary().await {
            Ok(activity) => activity,
            Err(error) => {
                release_shutdown_drain_on_error(host, acquired_drain).await;
                return Err(DaemonShutdownError::ActivityUnavailable(error));
            }
        };
        if activity.running_turn_count == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if let Err(error) = host.reconcile_running_turns_with_reason(&reason).await {
        host.release_shutdown_drain_if_owned(acquired_drain).await;
        return Err(DaemonShutdownError::Reconcile(error));
    }
    let activity = match host.turn_activity_summary().await {
        Ok(activity) => activity,
        Err(error) => {
            release_shutdown_drain_on_error(host, acquired_drain).await;
            return Err(DaemonShutdownError::ActivityUnavailable(error));
        }
    };

    spawn_deferred_daemon_shutdown(host.clone(), reason, Duration::from_millis(100));
    Ok(activity)
}

fn sandbox_work_is_active(activity: &DaemonSandboxWorkActivitySummary) -> bool {
    activity.active
}

fn turn_activity_blocks_update_drain(activity: &DaemonTurnActivitySummary) -> bool {
    activity.queued_turn_count > 0 || activity.running_turn_count > 0
}

async fn release_shutdown_drain_on_error(host: &DaemonShutdownHost, acquired_drain: bool) {
    host.release_shutdown_drain_if_owned(acquired_drain).await;
}

impl UpdateDrainHandle {
    pub async fn begin_update_drain_for_route(
        &self,
        req: BeginUpdateDrainRouteRequest,
    ) -> Result<BeginUpdateDrainRouteResult, MaintenanceRouteError> {
        if !req.confirm() {
            return Err(MaintenanceRouteError::bad_request("confirm required"));
        }
        let (reason, owner) = req.into_reason_owner();
        let reason = reason
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "daemon_update".to_string());
        let owner = owner
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let update_drain = self.update_drain();
        let activity = begin_update_drain_parts(
            self.global_store(),
            self.stores(),
            update_drain.as_ref(),
            reason,
            owner,
        )
        .await
        .map_err(begin_update_drain_route_error)?;
        Ok(BeginUpdateDrainRouteResult {
            acquired: true,
            activity,
        })
    }

    pub async fn release_update_drain_for_route(
        &self,
        req: ReleaseUpdateDrainRouteRequest,
    ) -> Result<ReleaseUpdateDrainRouteResult, MaintenanceRouteError> {
        if !req.confirm() {
            return Err(MaintenanceRouteError::bad_request("confirm required"));
        }
        let update_drain = self.update_drain();
        Ok(ReleaseUpdateDrainRouteResult {
            released: release_update_drain_parts(update_drain.as_ref()).await,
        })
    }

    pub async fn begin_update_drain(
        &self,
        reason: String,
        owner: String,
    ) -> Result<DaemonTurnActivitySummary, BeginUpdateDrainError> {
        let update_drain = self.update_drain();
        begin_update_drain_parts(
            self.global_store(),
            self.stores(),
            update_drain.as_ref(),
            reason,
            owner,
        )
        .await
    }

    pub async fn release_update_drain(&self) -> bool {
        let update_drain = self.update_drain();
        release_update_drain_parts(update_drain.as_ref()).await
    }

    pub async fn reject_new_execution_during_maintenance(&self) -> Result<(), Error> {
        let update_drain = self.update_drain();
        reject_new_execution_during_maintenance_parts(update_drain.as_ref()).await
    }

    pub async fn daemon_turn_activity_summary(&self) -> Result<DaemonTurnActivitySummary, Error> {
        let update_drain = self.update_drain();
        daemon_turn_activity_summary_parts(
            self.global_store(),
            self.stores(),
            update_drain.as_ref(),
        )
        .await
    }
}

impl LinuxSandboxRuntimeHandle {
    pub async fn acquire_linux_sandbox_prepare_drain(
        &self,
    ) -> Result<MaintenanceDrainPermit, MaintenanceDrainError> {
        acquire_linux_sandbox_prepare_drain_parts(
            self.update_drain(),
            self.global_store(),
            self.stores(),
            self.terminals(),
            self.harness(),
        )
        .await
    }
}

impl DaemonShutdownHandle {
    pub async fn request_daemon_shutdown_for_route(
        &self,
        req: ShutdownDaemonRouteRequest,
    ) -> Result<ShutdownDaemonRouteResult, MaintenanceRouteError> {
        if !req.confirm() {
            return Err(MaintenanceRouteError::bad_request("confirm required"));
        }
        if !self.local_shutdown_token_authorized(req.supplied_shutdown_token()) {
            return Err(MaintenanceRouteError::forbidden(
                "local desktop shutdown token required",
            ));
        }
        let reason = req
            .reason()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "desktop_quit".to_string());
        let activity = self
            .request_shutdown(reason)
            .await
            .map_err(daemon_shutdown_route_error)?;
        Ok(ShutdownDaemonRouteResult {
            accepted: true,
            activity,
        })
    }
}

fn begin_update_drain_route_error(error: BeginUpdateDrainError) -> MaintenanceRouteError {
    match error {
        BeginUpdateDrainError::AlreadyActive => {
            MaintenanceRouteError::conflict("daemon update drain already active")
        }
        BeginUpdateDrainError::ActivityUnavailable(error) => MaintenanceRouteError::internal(error),
        BeginUpdateDrainError::Busy => MaintenanceRouteError::conflict(
            "daemon has queued or running turns; update drain was not acquired",
        ),
    }
}

fn daemon_shutdown_route_error(error: DaemonShutdownError) -> MaintenanceRouteError {
    match error {
        DaemonShutdownError::ActivityUnavailable(error) | DaemonShutdownError::Reconcile(error) => {
            MaintenanceRouteError::internal(error)
        }
    }
}

#[cfg(test)]
mod tests;
