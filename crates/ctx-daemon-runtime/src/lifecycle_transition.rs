use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    create_private_dir_all, daemon_lifecycle_control_lock_path,
    daemon_lifecycle_transition_lock_path, daemon_root_path, daemon_upgrade_handoff_path,
    daemon_upgrade_restart_request_root, handoff_marker_state_at, open_or_create_pid_lock_file,
    remove_restart_requests_at, secure_private_file_permissions, write_handoff_marker_at,
    write_restart_request_at, DurableHandoffFence, HandoffMarkerState,
};

/// Serializes one complete user-visible daemon policy transaction for a data
/// root. This is distinct from [`DaemonLifecycleTransitionLock`]: a control
/// command may wait for readiness, whose publication takes the transition
/// lock.
pub struct DaemonLifecycleControlLock {
    file: Option<fs::File>,
    path: PathBuf,
}

impl DaemonLifecycleControlLock {
    pub fn acquire(data_root: &Path) -> Result<Self> {
        ctx_history_platform::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let path = daemon_lifecycle_control_lock_path(data_root);
        let (file, _) = open_or_create_pid_lock_file(&path).with_context(|| {
            format!("open ctx daemon lifecycle control lock {}", path.display())
        })?;
        secure_private_file_permissions(&path)?;
        fs2::FileExt::lock_exclusive(&file).with_context(|| {
            format!(
                "acquire ctx daemon lifecycle control lock {}",
                path.display()
            )
        })?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    /// Remove the persistent guard only after an external quiescence fence has
    /// excluded every current and future control participant.
    pub fn remove_after_quiescence(mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            fs2::FileExt::unlock(&file).with_context(|| {
                format!(
                    "release ctx daemon lifecycle control lock {}",
                    self.path.display()
                )
            })?;
            drop(file);
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "remove quiesced ctx daemon lifecycle control lock {}",
                    self.path.display()
                )
            }),
        }
    }
}

impl Drop for DaemonLifecycleControlLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.as_ref() {
            let _ = fs2::FileExt::unlock(file);
        }
    }
}

/// Serializes durable handoff fencing with daemon readiness publication for one
/// data root. The guard file is persistent so concurrent openers always contend
/// on the same filesystem object.
pub struct DaemonLifecycleTransitionLock {
    file: Option<fs::File>,
    path: std::path::PathBuf,
}

impl DaemonLifecycleTransitionLock {
    pub fn acquire(data_root: &Path) -> Result<Self> {
        ctx_history_platform::platform_security::establish_private_data_root(data_root)?;
        create_private_dir_all(&daemon_root_path(data_root))?;
        let path = daemon_lifecycle_transition_lock_path(data_root);
        let (file, _) = open_or_create_pid_lock_file(&path).with_context(|| {
            format!(
                "open ctx daemon lifecycle transition lock {}",
                path.display()
            )
        })?;
        secure_private_file_permissions(&path)?;
        fs2::FileExt::lock_exclusive(&file).with_context(|| {
            format!(
                "acquire ctx daemon lifecycle transition lock {}",
                path.display()
            )
        })?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    /// Remove the persistent guard only after an external quiescence fence has
    /// excluded every current and future lifecycle-transition participant.
    pub fn remove_after_quiescence(mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            fs2::FileExt::unlock(&file).with_context(|| {
                format!(
                    "release ctx daemon lifecycle transition lock {}",
                    self.path.display()
                )
            })?;
            drop(file);
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "remove quiesced ctx daemon lifecycle transition lock {}",
                    self.path.display()
                )
            }),
        }
    }
}

impl Drop for DaemonLifecycleTransitionLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.as_ref() {
            let _ = fs2::FileExt::unlock(file);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonHandoffRestartDeferral {
    RestartRequest(PathBuf),
    ReplacementPending,
}

#[derive(Debug)]
pub struct DaemonLifecycleState(AtomicU8);

#[must_use]
pub struct DaemonLifecycleStoppingGuard<'a>(&'a DaemonLifecycleState);

impl Drop for DaemonLifecycleStoppingGuard<'_> {
    fn drop(&mut self) {
        self.0.mark_stopping();
    }
}

impl DaemonLifecycleState {
    pub fn starting() -> Self {
        Self(AtomicU8::new(0))
    }

    pub fn mark_ready(&self) -> bool {
        matches!(
            self.0
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst),
            Ok(_) | Err(1)
        )
    }

    pub fn mark_stopping(&self) {
        self.0.store(2, Ordering::SeqCst);
    }

    pub fn stopping_guard(&self) -> DaemonLifecycleStoppingGuard<'_> {
        DaemonLifecycleStoppingGuard(self)
    }

    pub fn is_stopping(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 2
    }

    pub fn readiness(&self) -> &'static str {
        match self.0.load(Ordering::SeqCst) {
            0 => "starting",
            1 => "ready",
            _ => "stopping",
        }
    }
}

pub fn block_daemon_main_after_ready_for_test(data_root: &Path) -> Result<()> {
    block_daemon_main_for_test(data_root, "after-ready")
}

/// Deterministically pauses an identity-owned daemon after its lifecycle IPC
/// endpoint is responsive but before it publishes Ready. Debug builds only.
pub fn block_daemon_main_before_ready_for_test(data_root: &Path) -> Result<()> {
    block_daemon_main_for_test(data_root, "before-ready")
}

fn block_daemon_main_for_test(data_root: &Path, phase: &str) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let block = data_root.join(format!(".block-daemon-main-{phase}-for-test"));
    if !block.exists() {
        return Ok(());
    }
    let blocked = data_root.join(format!(".daemon-main-blocked-{phase}-for-test"));
    fs::write(&blocked, b"blocked\n")
        .with_context(|| format!("publish daemon test block marker {}", blocked.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while block.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if block.exists() {
        anyhow::bail!("timed out in daemon {phase} test block");
    }
    match fs::remove_file(&blocked) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("remove daemon test block marker {}", blocked.display())),
    }
}

/// Deterministically pauses a daemon policy command after its requested mode
/// is durable but before daemon or supervisor side effects. Debug builds only.
pub fn block_daemon_enabled_after_config_for_test(data_root: &Path, enabled: bool) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let mode = if enabled { "automatic" } else { "manual" };
    let block = data_root.join(format!(
        ".block-daemon-{mode}-indexing-after-config-for-test"
    ));
    if !block.exists() {
        return Ok(());
    }
    let blocked = data_root.join(format!(
        ".daemon-{mode}-indexing-blocked-after-config-for-test"
    ));
    fs::write(&blocked, format!("{}\n", std::process::id()))
        .with_context(|| format!("publish daemon policy test marker {}", blocked.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while block.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if block.exists() {
        anyhow::bail!("timed out at daemon policy after-config test gate");
    }
    match fs::remove_file(&blocked) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("remove daemon policy test marker {}", blocked.display())),
    }
}

pub fn defer_restart_for_active_daemon_handoff(
    data_root: &Path,
    persisted_restart_label: &str,
    request_id: &str,
    stale_after: Duration,
) -> Result<Option<DaemonHandoffRestartDeferral>> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    let handoff_path = daemon_upgrade_handoff_path(data_root);
    if matches!(
        handoff_marker_state_at(&handoff_path, stale_after),
        HandoffMarkerState::Absent | HandoffMarkerState::Terminal
    ) {
        return Ok(None);
    }
    if crate::read_handoff_marker_at(&handoff_path)
        .is_some_and(|handoff| handoff["phase"] == "finalizing")
    {
        return Ok(Some(DaemonHandoffRestartDeferral::ReplacementPending));
    }
    write_restart_request_at(
        &daemon_upgrade_restart_request_root(data_root),
        persisted_restart_label,
        request_id,
    )
    .map(DaemonHandoffRestartDeferral::RestartRequest)
    .map(Some)
}

pub fn write_daemon_restart_request_if_intake_open(
    data_root: &Path,
    persisted_restart_label: &str,
    request_id: &str,
) -> Result<PathBuf> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    if crate::read_handoff_marker_at(&daemon_upgrade_handoff_path(data_root))
        .is_some_and(|handoff| handoff["phase"] == "finalizing")
    {
        anyhow::bail!("daemon restart handoff intake is closed");
    }
    write_restart_request_at(
        &daemon_upgrade_restart_request_root(data_root),
        persisted_restart_label,
        request_id,
    )
}

pub fn close_daemon_handoff_restart_intake(
    data_root: &Path,
    handoff_id: &str,
    captured_restart_label: Option<&str>,
) -> Result<Option<String>> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    let path = daemon_upgrade_handoff_path(data_root);
    let handoff = crate::read_handoff_marker_at(&path)
        .ok_or_else(|| anyhow::anyhow!("replacement helper has no daemon handoff"))?;
    if handoff["handoff_id"].as_str() != Some(handoff_id) {
        anyhow::bail!("replacement helper daemon handoff identity does not match");
    }
    let restart_label = captured_restart_label.map(str::to_owned).or_else(|| {
        crate::read_restart_requests_at(&daemon_upgrade_restart_request_root(data_root))
            .into_iter()
            .next()
            .map(|(_, label)| label)
    });
    if handoff["phase"] == "finalizing" {
        return Ok(restart_label);
    }
    if handoff["phase"] == "completed" {
        return Ok(restart_label);
    }
    if handoff["phase"] != "scheduled" {
        anyhow::bail!("replacement helper daemon handoff is not scheduled");
    }
    let Some(restart_label) = restart_label else {
        write_handoff_marker_at(&path, handoff_id, "completed", None)?;
        return Ok(None);
    };
    let helper_pid = handoff["helper_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| anyhow::anyhow!("replacement helper handoff has no live helper PID"))?;
    write_handoff_marker_at(&path, handoff_id, "finalizing", Some(helper_pid))?;
    Ok(Some(restart_label))
}

pub fn write_finalizing_daemon_restart_request(
    data_root: &Path,
    handoff_id: &str,
    persisted_restart_label: &str,
) -> Result<PathBuf> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    let handoff = crate::read_handoff_marker_at(&daemon_upgrade_handoff_path(data_root))
        .ok_or_else(|| anyhow::anyhow!("replacement helper has no daemon handoff"))?;
    if handoff["handoff_id"].as_str() != Some(handoff_id) || handoff["phase"] != "finalizing" {
        anyhow::bail!("replacement helper does not own finalizing daemon handoff");
    }
    write_restart_request_at(
        &daemon_upgrade_restart_request_root(data_root),
        persisted_restart_label,
        handoff_id,
    )
}

pub fn finish_replacement_daemon_handoff(data_root: &Path, handoff_id: &str) -> Result<()> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    let path = daemon_upgrade_handoff_path(data_root);
    let Some(handoff) = crate::read_handoff_marker_at(&path) else {
        return Ok(());
    };
    if handoff["handoff_id"].as_str() != Some(handoff_id) {
        return Ok(());
    }
    if !crate::read_restart_requests_at(&daemon_upgrade_restart_request_root(data_root)).is_empty()
    {
        anyhow::bail!("replacement daemon handoff still has unacknowledged restart intent");
    }
    write_handoff_marker_at(&path, handoff_id, "completed", None)
}

pub fn terminalize_daemon_handoff_for_restart(data_root: &Path, handoff_id: &str) -> Result<()> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    let path = daemon_upgrade_handoff_path(data_root);
    if crate::read_handoff_marker_at(&path)
        .as_ref()
        .and_then(|handoff| handoff["handoff_id"].as_str())
        != Some(handoff_id)
    {
        return Ok(());
    }
    write_handoff_marker_at(&path, handoff_id, "completed", None)
}

pub fn complete_daemon_handoff_and_acknowledge(
    data_root: &Path,
    fence: &mut DurableHandoffFence,
) -> Result<()> {
    let _transition = DaemonLifecycleTransitionLock::acquire(data_root)?;
    remove_restart_requests_at(&daemon_upgrade_restart_request_root(data_root));
    fence.complete()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_unwind_revokes_readiness_before_service_teardown() {
        struct ProbeOnDrop<'a> {
            lifecycle: &'a DaemonLifecycleState,
            observed: &'a std::cell::Cell<Option<&'static str>>,
        }

        impl Drop for ProbeOnDrop<'_> {
            fn drop(&mut self) {
                self.observed.set(Some(self.lifecycle.readiness()));
            }
        }

        let lifecycle = DaemonLifecycleState::starting();
        assert!(lifecycle.mark_ready());
        let observed = std::cell::Cell::new(None);
        let result = (|| -> Result<()> {
            let _service = ProbeOnDrop {
                lifecycle: &lifecycle,
                observed: &observed,
            };
            let _stopping = lifecycle.stopping_guard();
            anyhow::bail!("injected fatal daemon error after readiness")
        })();

        assert!(result.is_err());
        assert_eq!(observed.get(), Some("stopping"));
    }

    #[test]
    fn transition_lock_serializes_independent_openers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let owner = DaemonLifecycleTransitionLock::acquire(temp.path())?;
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(daemon_lifecycle_transition_lock_path(temp.path()))?;

        assert!(fs2::FileExt::try_lock_exclusive(&contender).is_err());
        drop(owner);
        fs2::FileExt::try_lock_exclusive(&contender)?;
        fs2::FileExt::unlock(&contender)?;
        Ok(())
    }

    #[test]
    fn control_lock_serializes_independent_openers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let owner = DaemonLifecycleControlLock::acquire(temp.path())?;
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(daemon_lifecycle_control_lock_path(temp.path()))?;

        assert!(fs2::FileExt::try_lock_exclusive(&contender).is_err());
        drop(owner);
        fs2::FileExt::try_lock_exclusive(&contender)?;
        fs2::FileExt::unlock(&contender)?;
        Ok(())
    }

    #[test]
    fn deferred_restart_and_terminal_acknowledgement_are_linearized() -> Result<()> {
        for writer_first in [true, false] {
            let temp = tempfile::tempdir()?;
            let handoff_id = if writer_first {
                "writer-first"
            } else {
                "terminal-first"
            };
            let handoff_path = daemon_upgrade_handoff_path(temp.path());
            crate::write_handoff_marker_at(&handoff_path, handoff_id, "ready", None)?;
            let mut fence = DurableHandoffFence::armed(handoff_path, handoff_id.to_owned());
            if writer_first {
                assert!(defer_restart_for_active_daemon_handoff(
                    temp.path(),
                    "search",
                    "request",
                    Duration::from_secs(60),
                )?
                .is_some());
            }
            complete_daemon_handoff_and_acknowledge(temp.path(), &mut fence)?;
            assert!(defer_restart_for_active_daemon_handoff(
                temp.path(),
                "search",
                "late-request",
                Duration::from_secs(60),
            )?
            .is_none());
            assert!(
                crate::read_restart_requests_at(&daemon_upgrade_restart_request_root(temp.path()))
                    .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn handoff_and_restart_records_preserve_v1_identity_fields() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_id = "handoff-v1";
        let handoff_path = daemon_upgrade_handoff_path(temp.path());
        crate::write_handoff_marker_at(&handoff_path, handoff_id, "scheduled", Some(42))?;
        let handoff = crate::read_handoff_marker_at(&handoff_path).expect("handoff record");
        assert_eq!(handoff["schema_version"], 1);
        assert_eq!(handoff["handoff_id"], handoff_id);
        assert_eq!(handoff["phase"], "scheduled");
        assert_eq!(handoff["helper_pid"], 42);
        assert_eq!(handoff.as_object().map(serde_json::Map::len), Some(6));

        let path = write_restart_request_at(
            &daemon_upgrade_restart_request_root(temp.path()),
            "setup",
            handoff_id,
        )?;
        let restart: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(restart["schema_version"], 1);
        assert_eq!(restart["request_id"], handoff_id);
        assert_eq!(restart["trigger_command"], "setup");
        assert_eq!(restart.as_object().map(serde_json::Map::len), Some(5));
        Ok(())
    }

    #[test]
    fn finalizing_helper_closes_restart_intake_before_terminalization() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_id = "helper-finalizing";
        let handoff_path = daemon_upgrade_handoff_path(temp.path());
        write_handoff_marker_at(
            &handoff_path,
            handoff_id,
            "scheduled",
            Some(std::process::id()),
        )?;

        assert_eq!(
            close_daemon_handoff_restart_intake(temp.path(), handoff_id, Some("search"))?,
            Some("search".to_owned())
        );
        assert_eq!(
            defer_restart_for_active_daemon_handoff(
                temp.path(),
                "search",
                "late-request",
                Duration::from_secs(60),
            )?,
            Some(DaemonHandoffRestartDeferral::ReplacementPending)
        );
        assert!(write_daemon_restart_request_if_intake_open(
            temp.path(),
            "search",
            "late-installation"
        )
        .is_err());
        write_finalizing_daemon_restart_request(temp.path(), handoff_id, "search")?;
        assert!(finish_replacement_daemon_handoff(temp.path(), handoff_id).is_err());
        remove_restart_requests_at(&daemon_upgrade_restart_request_root(temp.path()));
        finish_replacement_daemon_handoff(temp.path(), handoff_id)?;
        assert_eq!(
            crate::read_handoff_marker_at(&handoff_path)
                .and_then(|handoff| handoff["phase"].as_str().map(str::to_owned))
                .as_deref(),
            Some("completed")
        );
        Ok(())
    }

    #[test]
    fn helper_without_restart_demand_terminalizes_when_intake_closes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_id = "helper-without-restart";
        let handoff_path = daemon_upgrade_handoff_path(temp.path());
        write_handoff_marker_at(
            &handoff_path,
            handoff_id,
            "scheduled",
            Some(std::process::id()),
        )?;

        assert_eq!(
            close_daemon_handoff_restart_intake(temp.path(), handoff_id, None)?,
            None
        );
        assert_eq!(
            handoff_marker_state_at(&handoff_path, Duration::from_secs(60)),
            HandoffMarkerState::Terminal
        );
        Ok(())
    }
}
