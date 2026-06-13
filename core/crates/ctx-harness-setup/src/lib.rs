use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSetupPhase {
    ArtifactDownload,
    MachineCheck,
    MachineStartOrInit,
    ImageCheck,
    ImageLoad,
    ContainerCheck,
    ContainerStartOrCreate,
    RuntimeNetworkSetup,
    Ready,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSetupLogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessSetupDownloadStatus {
    pub artifact: String,
    pub downloaded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessSetupProgressUpdate {
    pub phase: HarnessSetupPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_download: Option<HarnessSetupDownloadStatus>,
}

pub trait HarnessSetupObserver: Send + Sync {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str);
    fn on_log(&self, phase: HarnessSetupPhase, level: HarnessSetupLogLevel, message: &str);
    fn on_progress(&self, _progress: HarnessSetupProgressUpdate) {}
}

pub fn observe_phase(
    observer: Option<&dyn HarnessSetupObserver>,
    phase: HarnessSetupPhase,
    message: &str,
) {
    if let Some(observer) = observer {
        observer.on_phase(phase, message);
    }
}

pub fn observe_log(
    observer: Option<&dyn HarnessSetupObserver>,
    phase: HarnessSetupPhase,
    level: HarnessSetupLogLevel,
    message: &str,
) {
    if let Some(observer) = observer {
        observer.on_log(phase, level, message);
    }
}

pub fn observe_progress(
    observer: Option<&dyn HarnessSetupObserver>,
    progress: HarnessSetupProgressUpdate,
) {
    if let Some(observer) = observer {
        observer.on_progress(progress);
    }
}

#[derive(Debug, Default)]
struct ManagedDownloadAggregateState {
    downloads: BTreeMap<String, ManagedDownloadArtifactState>,
}

#[derive(Debug, Clone, Default)]
pub struct ManagedDownloadAggregate {
    inner: Arc<StdMutex<ManagedDownloadAggregateState>>,
}

#[derive(Debug, Clone, Default)]
struct ManagedDownloadArtifactState {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    bytes_per_sec: Option<u64>,
    finished: bool,
}

impl ManagedDownloadAggregate {
    pub fn update(
        &self,
        artifact: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        bytes_per_sec: Option<u64>,
        finished: bool,
    ) -> Option<HarnessSetupDownloadStatus> {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = inner.downloads.entry(artifact.to_string()).or_default();
        entry.downloaded_bytes = downloaded_bytes;
        entry.total_bytes = total_bytes;
        entry.bytes_per_sec = bytes_per_sec;
        entry.finished = finished;

        let all_finished = inner.downloads.values().all(|download| download.finished);
        if all_finished {
            return None;
        }

        let downloaded_total = inner
            .downloads
            .values()
            .map(|download| download.downloaded_bytes)
            .sum();
        let total_bytes = inner.downloads.values().try_fold(0u64, |acc, download| {
            download.total_bytes.map(|value| acc.saturating_add(value))
        });
        let bytes_per_sec = inner
            .downloads
            .values()
            .filter_map(|download| download.bytes_per_sec)
            .fold(None, |acc: Option<u64>, value| {
                Some(acc.unwrap_or(0u64).saturating_add(value))
            });
        Some(HarnessSetupDownloadStatus {
            artifact: "Required artifacts".to_string(),
            downloaded_bytes: downloaded_total,
            total_bytes,
            bytes_per_sec,
        })
    }
}

#[derive(Clone)]
pub struct ManagedArtifactDownloadReporter<'a> {
    pub observer: Option<&'a dyn HarnessSetupObserver>,
    pub aggregate: Option<ManagedDownloadAggregate>,
    pub phase: HarnessSetupPhase,
    pub artifact: String,
}

impl<'a> ManagedArtifactDownloadReporter<'a> {
    pub fn new(
        observer: Option<&'a dyn HarnessSetupObserver>,
        aggregate: Option<ManagedDownloadAggregate>,
        phase: HarnessSetupPhase,
        artifact: impl Into<String>,
    ) -> Self {
        Self {
            observer,
            aggregate,
            phase,
            artifact: artifact.into(),
        }
    }

    pub fn emit_progress(
        &self,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        bytes_per_sec: Option<u64>,
        finished: bool,
    ) {
        let active_download = if let Some(aggregate) = &self.aggregate {
            aggregate.update(
                &self.artifact,
                downloaded_bytes,
                total_bytes,
                bytes_per_sec,
                finished,
            )
        } else if finished {
            None
        } else {
            Some(HarnessSetupDownloadStatus {
                artifact: self.artifact.clone(),
                downloaded_bytes,
                total_bytes,
                bytes_per_sec,
            })
        };
        observe_progress(
            self.observer,
            HarnessSetupProgressUpdate {
                phase: self.phase,
                active_download,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_download_aggregate_combines_parallel_artifact_progress() {
        let aggregate = ManagedDownloadAggregate::default();

        let first = aggregate
            .update("Sandbox CLI runtime", 10, Some(40), Some(3), false)
            .expect("first aggregate snapshot");
        assert_eq!(first.artifact, "Required artifacts");
        assert_eq!(first.downloaded_bytes, 10);
        assert_eq!(first.total_bytes, Some(40));
        assert_eq!(first.bytes_per_sec, Some(3));

        let combined = aggregate
            .update("Harness image", 5, Some(20), Some(2), false)
            .expect("combined aggregate snapshot");
        assert_eq!(combined.downloaded_bytes, 15);
        assert_eq!(combined.total_bytes, Some(60));
        assert_eq!(combined.bytes_per_sec, Some(5));

        let still_running = aggregate
            .update("Sandbox CLI runtime", 40, Some(40), Some(4), true)
            .expect("remaining artifact should keep aggregate active");
        assert_eq!(still_running.downloaded_bytes, 45);
        assert_eq!(still_running.total_bytes, Some(60));
        assert_eq!(still_running.bytes_per_sec, Some(6));

        let finished = aggregate.update("Harness image", 20, Some(20), Some(2), true);
        assert!(
            finished.is_none(),
            "aggregate should clear once all downloads finish"
        );
    }
}
