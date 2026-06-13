use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::sync::Notify;

use super::*;
use crate::{ExecutionMode, ExecutionSettings, HarnessSetupDownloadStatus};

mod coordinator;
mod registry;

#[derive(Default)]
struct FakeWarmupOperations {
    runtime_runs: AtomicUsize,
    launch_ready_runs: AtomicUsize,
    builder_runs: AtomicUsize,
    runtime_release: Notify,
    launch_ready_release: Notify,
    builder_release: Notify,
    runtime_block: bool,
    launch_ready_block: bool,
    builder_block: bool,
}

impl FakeWarmupOperations {
    fn blocking_runtime() -> Self {
        Self {
            runtime_block: true,
            ..Self::default()
        }
    }

    fn blocking_launch_ready() -> Self {
        Self {
            launch_ready_block: true,
            ..Self::default()
        }
    }

    fn blocking_runtime_and_builder() -> Self {
        Self {
            launch_ready_block: true,
            builder_block: true,
            ..Self::default()
        }
    }

    async fn wait_for_runtime_runs(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.runtime_runs.load(Ordering::SeqCst) >= expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for runtime runs");
    }

    async fn expect_runtime_runs_below(&self, unexpected: usize) {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                self.wait_for_runtime_runs(unexpected)
            )
            .await
            .is_err(),
            "unexpectedly observed runtime run {unexpected}",
        );
    }

    async fn wait_for_builder_runs(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.builder_runs.load(Ordering::SeqCst) >= expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for builder runs");
    }

    async fn wait_for_launch_ready_runs(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.launch_ready_runs.load(Ordering::SeqCst) >= expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for launch-ready runs");
    }

    fn release_runtime(&self) {
        self.runtime_release.notify_waiters();
    }

    fn release_launch_ready(&self) {
        self.launch_ready_release.notify_waiters();
    }

    fn release_builder(&self) {
        self.builder_release.notify_waiters();
    }
}

#[async_trait]
impl SharedWarmupOperations for FakeWarmupOperations {
    async fn warm_runtime(
        &self,
        _settings: ExecutionSettings,
        observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        self.runtime_runs.fetch_add(1, Ordering::SeqCst);
        observer.on_phase(HarnessSetupPhase::MachineCheck, "warming runtime");
        observer.on_progress(HarnessSetupProgressUpdate {
            phase: HarnessSetupPhase::ArtifactDownload,
            active_download: Some(HarnessSetupDownloadStatus {
                artifact: "Required artifacts".to_string(),
                downloaded_bytes: 512,
                total_bytes: Some(1024),
                bytes_per_sec: Some(128),
            }),
        });
        if self.runtime_block {
            self.runtime_release.notified().await;
        }
        Ok(())
    }

    async fn warm_runtime_launch_ready(
        &self,
        _settings: ExecutionSettings,
        observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        self.launch_ready_runs.fetch_add(1, Ordering::SeqCst);
        observer.on_phase(
            HarnessSetupPhase::MachineStartOrInit,
            "warming launch-ready runtime",
        );
        if self.launch_ready_block {
            self.launch_ready_release.notified().await;
        }
        Ok(())
    }

    async fn warm_builder(&self, observer: Arc<dyn HarnessSetupObserver>) -> Result<()> {
        self.builder_runs.fetch_add(1, Ordering::SeqCst);
        observer.on_phase(HarnessSetupPhase::ImageLoad, "warming builder");
        if self.builder_block {
            self.builder_release.notified().await;
        }
        Ok(())
    }
}

fn container_settings(image: &str) -> ExecutionSettings {
    let mut settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        ..ExecutionSettings::default()
    };
    settings.container.runtime = crate::ContainerRuntimeKind::NativeContainer;
    settings.container.image = Some(image.to_string());
    settings
}

#[derive(Default)]
struct RecordingObserver {
    phases: StdMutex<Vec<(HarnessSetupPhase, String)>>,
    progress: StdMutex<Vec<HarnessSetupProgressUpdate>>,
}

impl HarnessSetupObserver for RecordingObserver {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str) {
        self.phases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((phase, message.to_string()));
    }

    fn on_log(&self, _phase: HarnessSetupPhase, _level: HarnessSetupLogLevel, _message: &str) {}

    fn on_progress(&self, progress: HarnessSetupProgressUpdate) {
        self.progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(progress);
    }
}
