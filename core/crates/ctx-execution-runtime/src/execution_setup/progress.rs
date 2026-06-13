use super::launch_state::{LaunchJob, LaunchJobInner, StartupPrewarmMetadata};
use super::*;

#[derive(Clone)]
pub(super) struct LaunchObserver {
    pub(super) coordinator: Arc<ExecutionSetupCoordinator>,
    pub(super) job: Arc<LaunchJob>,
}

impl HarnessSetupObserver for LaunchObserver {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str) {
        self.coordinator.emit_phase(&self.job, phase, message);
    }

    fn on_log(&self, phase: HarnessSetupPhase, level: HarnessSetupLogLevel, message: &str) {
        self.coordinator.emit_log(&self.job, phase, level, message);
    }

    fn on_progress(&self, progress: HarnessSetupProgressUpdate) {
        self.coordinator.emit_progress(&self.job, progress);
    }
}

fn phase_budget_ms(kind: ExecutionSetupJobKind, phase: HarnessSetupPhase) -> u64 {
    match phase {
        HarnessSetupPhase::ArtifactDownload => 150_000,
        HarnessSetupPhase::MachineCheck => 2_000,
        HarnessSetupPhase::MachineStartOrInit => 25_000,
        HarnessSetupPhase::ImageCheck => 1_000,
        HarnessSetupPhase::ImageLoad => 5_000,
        HarnessSetupPhase::ContainerCheck => {
            if kind == ExecutionSetupJobKind::WorkspaceLaunch {
                500
            } else {
                0
            }
        }
        HarnessSetupPhase::ContainerStartOrCreate => {
            if kind == ExecutionSetupJobKind::WorkspaceLaunch {
                800
            } else {
                0
            }
        }
        HarnessSetupPhase::RuntimeNetworkSetup => {
            if kind == ExecutionSetupJobKind::WorkspaceLaunch {
                1_000
            } else {
                0
            }
        }
        HarnessSetupPhase::Ready => 0,
    }
}

fn remaining_future_phase_budget_ms(kind: ExecutionSetupJobKind, phase: HarnessSetupPhase) -> u64 {
    match kind {
        ExecutionSetupJobKind::StartupPrewarm => match phase {
            HarnessSetupPhase::ArtifactDownload | HarnessSetupPhase::MachineCheck => {
                phase_budget_ms(kind, HarnessSetupPhase::MachineStartOrInit)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageLoad)
            }
            HarnessSetupPhase::MachineStartOrInit => {
                phase_budget_ms(kind, HarnessSetupPhase::ImageCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageLoad)
            }
            HarnessSetupPhase::ImageCheck => phase_budget_ms(kind, HarnessSetupPhase::ImageLoad),
            HarnessSetupPhase::ImageLoad
            | HarnessSetupPhase::ContainerCheck
            | HarnessSetupPhase::ContainerStartOrCreate
            | HarnessSetupPhase::RuntimeNetworkSetup
            | HarnessSetupPhase::Ready => 0,
        },
        ExecutionSetupJobKind::WorkspaceLaunch => match phase {
            HarnessSetupPhase::ArtifactDownload | HarnessSetupPhase::MachineCheck => {
                phase_budget_ms(kind, HarnessSetupPhase::MachineStartOrInit)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageLoad)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerStartOrCreate)
                    + phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::MachineStartOrInit => {
                phase_budget_ms(kind, HarnessSetupPhase::ImageCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ImageLoad)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerStartOrCreate)
                    + phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::ImageCheck => {
                phase_budget_ms(kind, HarnessSetupPhase::ImageLoad)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerStartOrCreate)
                    + phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::ImageLoad => {
                phase_budget_ms(kind, HarnessSetupPhase::ContainerCheck)
                    + phase_budget_ms(kind, HarnessSetupPhase::ContainerStartOrCreate)
                    + phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::ContainerCheck => {
                phase_budget_ms(kind, HarnessSetupPhase::ContainerStartOrCreate)
                    + phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::ContainerStartOrCreate => {
                phase_budget_ms(kind, HarnessSetupPhase::RuntimeNetworkSetup)
            }
            HarnessSetupPhase::RuntimeNetworkSetup | HarnessSetupPhase::Ready => 0,
        },
    }
}

fn running_phase_elapsed_ms(inner: &LaunchJobInner, now: DateTime<Utc>) -> u64 {
    let Some(current_phase) = inner.current_phase else {
        return 0;
    };
    let started_at = inner
        .phases
        .iter()
        .rev()
        .find(|phase| phase.phase == current_phase && phase.finished_at.is_none())
        .map(|phase| phase.started_at)
        .unwrap_or(inner.started_at);
    now.signed_duration_since(started_at)
        .num_milliseconds()
        .max(0) as u64
}

pub(super) fn estimate_remaining_ms(inner: &LaunchJobInner, now: DateTime<Utc>) -> Option<u64> {
    match inner.state {
        ExecutionLaunchState::Ready | ExecutionLaunchState::Error => return Some(0),
        ExecutionLaunchState::Running => {}
    }
    let current_phase = inner.current_phase?;
    let elapsed_ms = running_phase_elapsed_ms(inner, now);
    let current_remaining_ms =
        estimate_current_phase_remaining_ms(inner, current_phase, elapsed_ms)?;
    Some(current_remaining_ms + remaining_future_phase_budget_ms(inner.kind, current_phase))
}

fn estimate_current_phase_remaining_ms(
    inner: &LaunchJobInner,
    current_phase: HarnessSetupPhase,
    elapsed_ms: u64,
) -> Option<u64> {
    if current_phase == HarnessSetupPhase::ArtifactDownload {
        if let Some(download) = inner.active_download.as_ref() {
            match (download.total_bytes, download.bytes_per_sec) {
                (Some(total_bytes), Some(bytes_per_sec)) if bytes_per_sec > 0 => {
                    return Some(
                        total_bytes
                            .saturating_sub(download.downloaded_bytes)
                            .saturating_mul(1000)
                            / bytes_per_sec,
                    );
                }
                _ => {}
            }
        }
    }

    let phase_budget = phase_budget_ms(inner.kind, current_phase);
    if phase_budget == 0 {
        return Some(0);
    }
    if elapsed_ms >= phase_budget {
        return None;
    }
    Some(phase_budget - elapsed_ms)
}

pub(super) fn project_progress_pct(
    inner: &LaunchJobInner,
    now: DateTime<Utc>,
    eta_ms: Option<u64>,
) -> Option<u8> {
    match inner.state {
        ExecutionLaunchState::Ready => return Some(100),
        ExecutionLaunchState::Error => return Some(100),
        ExecutionLaunchState::Running => {}
    }
    let eta_ms = eta_ms?;
    let elapsed_ms = now
        .signed_duration_since(inner.started_at)
        .num_milliseconds()
        .max(0) as u64;
    let total = elapsed_ms.saturating_add(eta_ms);
    if total == 0 {
        return Some(0);
    }
    Some(
        ((elapsed_ms as f64 / total as f64) * 100.0)
            .round()
            .clamp(0.0, 99.0) as u8,
    )
}

pub(super) fn format_error_chain(err: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        let raw = cause.to_string();
        let normalized = if raw.trim().is_empty() {
            "<empty error cause>".to_string()
        } else {
            raw.trim().to_string()
        };
        if parts.last() != Some(&normalized) {
            parts.push(normalized);
        }
    }
    if parts.is_empty() {
        return "unknown error".to_string();
    }
    parts.join(": ")
}

pub(super) fn format_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn prewarm_metadata_path(data_root: &Path) -> PathBuf {
    data_root
        .join("execution")
        .join("startup_prewarm_status.json")
}

pub(super) async fn read_prewarm_metadata(
    data_root: &Path,
) -> Result<Option<StartupPrewarmMetadata>> {
    let path = prewarm_metadata_path(data_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let parsed = serde_json::from_str::<StartupPrewarmMetadata>(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(parsed))
}

pub(super) async fn write_prewarm_metadata(
    data_root: &Path,
    metadata: &StartupPrewarmMetadata,
) -> Result<()> {
    let path = prewarm_metadata_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(metadata)?;
    tokio::fs::write(&path, raw)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub(super) async fn bundled_image_fingerprint(image: &str) -> Result<Option<String>> {
    ctx_sandbox_container_runtime::default_container_image_fingerprint(image).await
}

pub(super) fn needs_prewarm(
    machine_ready: bool,
    image_present: bool,
    image_ref_changed: bool,
    bundled_image_digest_changed: bool,
) -> bool {
    !machine_ready || !image_present || image_ref_changed || bundled_image_digest_changed
}

pub(super) fn normalize_container_engine_ready_for_gate(
    result: anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            let lowered = err.to_string().to_ascii_lowercase();
            if lowered.contains("sandbox container cli unavailable")
                || lowered.contains("native sandbox container runtime is unavailable")
            {
                // Thin desktop bundles can start with no local sandbox CLI yet; treat that as
                // "not ready" so startup prewarm can surface the missing runtime cleanly.
                return Ok(false);
            }
            Err(err)
        }
    }
}
