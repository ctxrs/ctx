use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{broadcast, Mutex};

use crate::{
    ExecutionMode, ExecutionSettings, HarnessSetupDownloadStatus, HarnessSetupLogLevel,
    HarnessSetupObserver, HarnessSetupPhase, HarnessSetupProgressUpdate, NoopRuntimeEventSink,
    NoopRuntimeMetricsSink, RuntimeEventSink, RuntimeMetricsSink, SharedExecutionHarness,
};
use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;

mod launch_jobs;
mod launch_state;
mod progress;
mod startup_prewarm;
#[cfg(test)]
mod tests;
mod types;
mod warmup_coordination;

use launch_state::{
    seed_runtime_prewarm_initial_state, seed_workspace_launch_initial_state, CoordinatorState,
    LaunchJob, LaunchTerminalMutation, PrewarmGate, StartupPrewarmMetadata,
};
use progress::{
    bundled_image_fingerprint, format_error_chain, format_ts, needs_prewarm,
    normalize_container_engine_ready_for_gate, read_prewarm_metadata, write_prewarm_metadata,
    LaunchObserver,
};
use types::runtime_prewarm_ready_phase_message;
#[cfg(test)]
use types::sandbox_cli_env_test_lock;
pub use types::{
    ExecutionLaunchLogLine, ExecutionLaunchPhaseStatus, ExecutionLaunchSnapshot,
    ExecutionLaunchState, ExecutionLaunchStreamEvent, ExecutionSetupJobKind, RuntimePrewarmScope,
    StartupPrewarmSnapshot, StartupPrewarmState,
};
pub use warmup_coordination::SharedWarmupOperations;
use warmup_coordination::{
    LaunchPrewarmCoordinator, PrewarmJobRegistry, RequestedPrewarmScope, SharedPrewarmLaunchJob,
};

const JOB_LOG_CAP: usize = 400;
const JOB_HISTORY_CAP: usize = 128;
const LAUNCH_EVENT_CHANNEL_CAP: usize = 256;

fn lock_or_recover<'a, T>(mutex: &'a StdMutex<T>, name: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(mutex = name, "mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

pub struct ExecutionSetupCoordinator {
    data_root: PathBuf,
    harness: SharedExecutionHarness,
    metrics: Arc<dyn RuntimeMetricsSink>,
    events: Arc<dyn RuntimeEventSink>,
    inner: Mutex<CoordinatorState>,
    prewarm: LaunchPrewarmCoordinator,
}

impl ExecutionSetupCoordinator {
    pub fn new_with_operations(
        data_root: PathBuf,
        harness: SharedExecutionHarness,
        events: Arc<dyn RuntimeEventSink>,
        metrics: Arc<dyn RuntimeMetricsSink>,
        operations: Arc<dyn SharedWarmupOperations>,
    ) -> Self {
        Self {
            prewarm: LaunchPrewarmCoordinator::new(operations),
            data_root,
            harness,
            metrics,
            events,
            inner: Mutex::new(CoordinatorState::default()),
        }
    }

    pub fn new_for_tests(
        data_root: PathBuf,
        harness: SharedExecutionHarness,
        operations: Arc<dyn SharedWarmupOperations>,
    ) -> Self {
        Self::new_with_operations(
            data_root,
            harness,
            Arc::new(NoopRuntimeEventSink),
            Arc::new(NoopRuntimeMetricsSink),
            operations,
        )
    }

    pub fn spawn_startup_prewarm(self: &Arc<Self>, execution: ExecutionSettings) {
        let coordinator = Arc::clone(self);
        tokio::spawn(async move {
            #[cfg(test)]
            let _sandbox_cli_env_test_lock = sandbox_cli_env_test_lock().lock().await;
            coordinator.run_startup_prewarm(execution).await;
        });
    }

    #[cfg_attr(test, allow(dead_code))]
    pub async fn record_startup_prewarm_error(&self, message: String) {
        let attempted_at = format_ts(Utc::now());
        let snapshot = StartupPrewarmSnapshot {
            state: StartupPrewarmState::Error,
            target_image: String::new(),
            needs_prewarm: false,
            machine_ready: false,
            image_present: false,
            image_ref_changed: false,
            bundled_image_digest_changed: false,
            last_attempt_at: Some(attempted_at),
            last_success_at: None,
            error: Some(message.clone()),
        };
        self.set_startup_snapshot(snapshot).await;
        self.events.emit_event(
            "warn",
            "execution.startup_prewarm_error",
            Some(json!({ "error": message })),
        );
    }

    pub async fn start_workspace_launch(
        self: &Arc<Self>,
        workspace: Workspace,
        settings: ExecutionSettings,
        daemon_url: String,
    ) -> ExecutionLaunchSnapshot {
        let (job, snapshot) = {
            let mut inner = self.inner.lock().await;

            if let Some(existing_job_id) = inner
                .running_launch_by_workspace
                .get(&workspace.id)
                .cloned()
            {
                if let Some(existing) = inner.launch_jobs.get(&existing_job_id) {
                    return existing.snapshot();
                }
                // Stale pointer: drop it so a new launch can start cleanly.
                inner.running_launch_by_workspace.remove(&workspace.id);
            }

            let job_id = uuid::Uuid::new_v4().to_string();
            let job = Arc::new(LaunchJob::new(job_id.clone(), workspace.id));
            if matches!(settings.mode, ExecutionMode::Host) {
                seed_workspace_launch_initial_state(job.as_ref(), &settings);
            } else {
                let _ = job.transition_phase(
                    HarnessSetupPhase::MachineCheck,
                    "checking container runtime",
                );
            }
            let snapshot = job.snapshot();

            inner
                .running_launch_by_workspace
                .insert(workspace.id, job_id.clone());
            inner.remember_launch_job(Arc::clone(&job));

            (job, snapshot)
        };

        let _ = job.tx.send(ExecutionLaunchStreamEvent::LaunchSnapshot {
            snapshot: snapshot.clone(),
        });

        let coordinator = Arc::clone(self);
        tokio::spawn(async move {
            coordinator
                .run_workspace_launch(job, workspace, settings, daemon_url)
                .await;
        });

        snapshot
    }

    pub async fn start_runtime_prewarm(
        self: &Arc<Self>,
        settings: ExecutionSettings,
        scope: RuntimePrewarmScope,
    ) -> ExecutionLaunchSnapshot {
        let (shared_job, snapshot) = {
            let mut inner = self.inner.lock().await;
            if let Some(existing) = inner.prewarm_jobs.find_compatible(&settings, scope) {
                if existing.request_scope(scope) {
                    return existing.snapshot();
                }
                inner
                    .prewarm_jobs
                    .remove_if_current(existing.key(), &existing);
            }

            let job = Arc::new(SharedPrewarmLaunchJob::new(
                uuid::Uuid::new_v4().to_string(),
                &settings,
                scope,
            ));
            let snapshot = job.snapshot();
            inner.prewarm_jobs.insert(Arc::clone(&job));
            inner.remember_launch_job(job.job());
            (job, snapshot)
        };

        let launch_job = shared_job.job();
        let _ = launch_job
            .tx
            .send(ExecutionLaunchStreamEvent::LaunchSnapshot {
                snapshot: snapshot.clone(),
            });

        let coordinator = Arc::clone(self);
        tokio::spawn(async move {
            coordinator.run_runtime_prewarm(shared_job, settings).await;
        });

        snapshot
    }

    pub async fn launch_status(&self, job_id: &str) -> Option<ExecutionLaunchSnapshot> {
        let job = {
            let inner = self.inner.lock().await;
            inner.launch_jobs.get(job_id).cloned()
        }?;
        Some(job.snapshot())
    }

    pub async fn subscribe_launch(
        &self,
        job_id: &str,
    ) -> Option<(
        ExecutionLaunchSnapshot,
        broadcast::Receiver<ExecutionLaunchStreamEvent>,
    )> {
        let job = {
            let inner = self.inner.lock().await;
            inner.launch_jobs.get(job_id).cloned()
        }?;
        let rx = job.tx.subscribe();
        let snapshot = job.snapshot();
        Some((snapshot, rx))
    }

    pub async fn startup_status(&self) -> StartupPrewarmSnapshot {
        let inner = self.inner.lock().await;
        inner.startup.clone()
    }
}
