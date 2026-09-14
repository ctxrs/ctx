use super::*;

pub(super) use ctx_daemon_runtime::terminate_identity_verified_residual_daemon;
#[cfg(windows)]
use ctx_daemon_runtime::wait_for_released_residual_daemon;
use ctx_daemon_runtime::{
    daemon_upgrade_handoff_path, daemon_upgrade_restart_request_root, DaemonLifecycleControlLock,
    DaemonLifecycleTransitionLock,
};

mod uninstall;

pub use uninstall::prepare_daemon_uninstall;
use uninstall::wait_for_daemon_lifecycle_release;

struct CurrentHandoffSupervisorFence<'a> {
    handoff: &'a mut DaemonUpgradeHandoff,
}

impl super::super::daemon_supervisor::DaemonSupervisorUpgradeFence
    for CurrentHandoffSupervisorFence<'_>
{
    fn release(&mut self) -> Result<()> {
        self.handoff.complete_release()
    }
}

struct ReplacementHandoffSupervisorFence<'a> {
    data_root: &'a Path,
    handoff_id: &'a str,
}

impl super::super::daemon_supervisor::DaemonSupervisorUpgradeFence
    for ReplacementHandoffSupervisorFence<'_>
{
    fn release(&mut self) -> Result<()> {
        ctx_daemon_runtime::terminalize_daemon_handoff_for_restart(self.data_root, self.handoff_id)
    }
}

pub(crate) fn terminate_current_executable_daemon(data_root: &Path) -> Result<()> {
    let executable = env::current_exe().context("resolve current ctx executable")?;
    terminate_identity_verified_residual_daemon(data_root, &executable)
}

const DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_ENV: &str =
    "CTX_DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_FOR_TESTS";

pub(super) fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_root_path(data_root).join(DAEMON_QUERY_ENDPOINT_FILE)
}

pub(super) fn read_daemon_upgrade_handoff(data_root: &Path) -> Option<Value> {
    read_daemon_upgrade_handoff_at(&daemon_upgrade_handoff_path(data_root))
}

fn read_daemon_upgrade_handoff_at(path: &Path) -> Option<Value> {
    ctx_daemon_runtime::read_handoff_marker_at(path)
}

use ctx_daemon_runtime::HandoffMarkerState as DaemonUpgradeHandoffState;

fn daemon_upgrade_handoff_state_at(path: &Path) -> DaemonUpgradeHandoffState {
    ctx_daemon_runtime::handoff_marker_state_at(path, DAEMON_UPGRADE_HANDOFF_STALE_AFTER)
}

#[cfg(test)]
pub(super) fn daemon_upgrade_handoff_is_active(data_root: &Path) -> bool {
    let path = daemon_upgrade_handoff_path(data_root);
    daemon_upgrade_handoff_is_active_at(&path)
}

#[cfg(test)]
fn daemon_upgrade_handoff_is_active_at(path: &Path) -> bool {
    daemon_upgrade_handoff_state_at(path) == DaemonUpgradeHandoffState::Active
}

pub(crate) fn daemon_upgrade_handoff_blocks_current_process(data_root: &Path) -> bool {
    match daemon_upgrade_handoff_state_at(&daemon_upgrade_handoff_path(data_root)) {
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => false,
        DaemonUpgradeHandoffState::CorruptOrUnreadable => true,
        DaemonUpgradeHandoffState::Active => {
            !current_process_owns_daemon_upgrade_handoff(data_root)
        }
    }
}

#[cfg(test)]
pub(crate) fn daemon_upgrade_handoff_fences_start(data_root: &Path) -> bool {
    !matches!(
        daemon_upgrade_handoff_state_at(&daemon_upgrade_handoff_path(data_root)),
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal
    )
}

pub(crate) fn current_process_owns_daemon_upgrade_handoff(data_root: &Path) -> bool {
    let token = env::var(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV).ok();
    current_process_owns_daemon_upgrade_handoff_with_token(data_root, token.as_deref())
}

fn current_process_owns_daemon_upgrade_handoff_with_token(
    data_root: &Path,
    handoff_token: Option<&str>,
) -> bool {
    current_process_owns_daemon_upgrade_handoff_at(
        &daemon_upgrade_handoff_path(data_root),
        handoff_token,
    )
}

fn current_process_owns_daemon_upgrade_handoff_at(
    handoff_path: &Path,
    handoff_token: Option<&str>,
) -> bool {
    ctx_daemon_runtime::process_owns_handoff_marker_at(
        handoff_path,
        handoff_token,
        DAEMON_UPGRADE_HANDOFF_STALE_AFTER,
    )
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ExpectedProcessIdentity {
    executable: PathBuf,
}

trait CooperativeStopPort: Send {
    fn request_stop(&mut self);
}

struct LifecycleCooperativeStopPort {
    data_root: PathBuf,
}

impl CooperativeStopPort for LifecycleCooperativeStopPort {
    fn request_stop(&mut self) {
        let _ = ctx_daemon_service::daemon_source_refresh_request(
            &self.data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": "upgrade_handoff",
            })),
            DAEMON_HEALTH_TIMEOUT,
            DAEMON_HEALTH_RESPONSE_MAX_BYTES,
        );
    }
}

struct DaemonUpgradeHandoffInput {
    data_root: PathBuf,
    handoff_id: String,
    expected_process: ExpectedProcessIdentity,
    persisted_restart_label: Option<String>,
    handoff_path: PathBuf,
    restart_request_root: PathBuf,
    cooperative_stop: Box<dyn CooperativeStopPort>,
}

fn normalize_daemon_upgrade_handoff_input(
    data_root: &Path,
    upgrade_attempt_id: &str,
    expected_executable: &Path,
) -> Result<DaemonUpgradeHandoffInput> {
    if !ctx_upgrade_engine::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    Ok(DaemonUpgradeHandoffInput {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        expected_process: ExpectedProcessIdentity {
            executable: expected_executable.to_path_buf(),
        },
        persisted_restart_label: daemon_restart_trigger(data_root)
            .map(|trigger| trigger.as_str().to_owned()),
        handoff_path: daemon_upgrade_handoff_path(data_root),
        restart_request_root: daemon_upgrade_restart_request_root(data_root),
        cooperative_stop: Box::new(LifecycleCooperativeStopPort {
            data_root: data_root.to_path_buf(),
        }),
    })
}

pub struct DaemonUpgradeHandoff {
    data_root: PathBuf,
    fence: ctx_daemon_runtime::DurableHandoffFence,
    installation_executable: PathBuf,
    persisted_restart_label: Option<String>,
    persisted_loop_interval_seconds: Option<u64>,
}

struct UpgradeHandoffRestartAuthority {
    replacement_executable: PathBuf,
}

impl UpgradeHandoffRestartAuthority {
    fn spawn(&self, launch: NormalizedLaunch) -> io::Result<Child> {
        spawn_daemon_child_for_upgrade_handoff(launch, &self.replacement_executable)
    }
}

impl DaemonUpgradeHandoff {
    pub fn wait_for_installation_quiescence(&self) -> Result<()> {
        wait_for_installation_daemon_quiescence_for(
            &self.installation_executable,
            self.fence.handoff_id(),
        )?;
        pause_after_installation_quiescence_for_test()
    }

    /// Capture persistent auto-daemon restart intent in data that can be
    /// embedded in a durable platform replacement helper.
    pub fn replacement_restart(&self) -> Option<(&'static str, Option<u64>)> {
        let trigger = self
            .persisted_restart_label
            .as_deref()
            .and_then(|label| parse_daemon_trigger(Some(label)))
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger))?;
        Some((trigger.as_str(), self.persisted_loop_interval_seconds))
    }

    /// Preserve daemon restart intent while schema-2 recovery re-executes the
    /// identity-validated current-format executable restored at the install
    /// path. The restored process consumes this request while fixing forward.
    pub fn release_for_current_format_reexec(mut self) -> Result<()> {
        let _transition = DaemonLifecycleTransitionLock::acquire(&self.data_root)?;
        if read_daemon_restart_request(&self.data_root).is_none() {
            if let Some(label) = self.persisted_restart_label.as_deref() {
                write_daemon_restart_request_at(
                    &daemon_upgrade_restart_request_root(&self.data_root),
                    label,
                    self.fence.handoff_id(),
                )?;
            }
        }
        self.fence.abort_and_disarm()?;
        Ok(())
    }

    /// Release the upgrade fence and restart the current auto-daemon after a
    /// verified forward publication succeeds.
    pub fn resume_with(mut self, executable: &Path) -> Result<()> {
        let restart_authority = self.authenticated_restart_authority(executable)?;
        let restart_trigger = self
            .persisted_restart_label
            .as_deref()
            .and_then(|label| parse_daemon_trigger(Some(label)))
            .or_else(|| read_daemon_restart_request(&self.data_root).map(|(_, trigger)| trigger));
        if daemon_restart_allowed(&self.data_root)? {
            if let Some(trigger) = restart_trigger {
                let data_root = self.data_root.clone();
                let loop_interval_seconds = self.persisted_loop_interval_seconds;
                let mut upgrade_fence = CurrentHandoffSupervisorFence { handoff: &mut self };
                let supervisor_resume =
                    super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                        &data_root,
                        executable,
                        loop_interval_seconds,
                        &mut upgrade_fence,
                    )?;
                match supervisor_resume {
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                        wait_for_daemon_ready_ack(&self.data_root)?;
                    }
                    super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback
                    | super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::ManagerUnavailable => {
                        let launch = daemon_autostart_command(
                            executable,
                            &self.data_root,
                            trigger,
                            self.persisted_loop_interval_seconds,
                            Some(self.fence.handoff_id()),
                        )?;
                        let mut child = restart_authority
                            .spawn(launch)
                            .context("restart persistent ctx daemon after upgrade")?;
                        wait_for_replacement_daemon(&self.data_root, &mut child)?;
                    }
                }
            }
        }
        restart_acknowledged_installation_daemons_with(
            executable,
            self.fence.handoff_id(),
            Some(&self.data_root),
            |launch| restart_authority.spawn(launch),
        )?;
        if self.fence.is_armed() {
            self.complete_release()?;
        }
        Ok(())
    }

    fn authenticated_restart_authority(
        &self,
        executable: &Path,
    ) -> Result<UpgradeHandoffRestartAuthority> {
        let current = read_daemon_upgrade_handoff(&self.data_root)
            .ok_or_else(|| anyhow!("daemon upgrade handoff disappeared before restart"))?;
        let identity_matches = current.get("handoff_id").and_then(Value::as_str)
            == Some(self.fence.handoff_id())
            && current.get("phase").and_then(Value::as_str) == Some("ready")
            && current
                .get("owner_pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                == Some(process::id());
        if !identity_matches {
            return Err(anyhow!(
                "current process does not own the ready daemon upgrade handoff"
            ));
        }
        Ok(UpgradeHandoffRestartAuthority {
            replacement_executable: executable.to_path_buf(),
        })
    }

    /// Keep the fence owned by a platform replacement helper after apply
    /// returns `Scheduled`. Autostart remains blocked while that helper is live
    /// and becomes eligible only after it exits.
    pub fn transfer_to_replacement_helper(mut self, helper_pid: u32) -> Result<()> {
        let already_transferred =
            read_daemon_upgrade_handoff(&self.data_root).is_some_and(|value| {
                value.get("handoff_id").and_then(Value::as_str) == Some(self.fence.handoff_id())
                    && value.get("phase").and_then(Value::as_str) == Some("scheduled")
                    && value
                        .get("helper_pid")
                        .and_then(Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok())
                        == Some(helper_pid)
            });
        if !already_transferred {
            self.fence.transfer(helper_pid)?;
        } else {
            self.fence.disarm();
        }
        Ok(())
    }

    fn complete_release(&mut self) -> Result<()> {
        ctx_daemon_runtime::complete_daemon_handoff_and_acknowledge(
            &self.data_root,
            &mut self.fence,
        )
    }
}

impl Drop for DaemonUpgradeHandoff {
    fn drop(&mut self) {
        if !self.fence.is_armed() {
            return;
        }
        if let Ok(_transition) = DaemonLifecycleTransitionLock::acquire(&self.data_root) {
            let _ = self.fence.abort_and_disarm();
        }
        self.fence.disarm();
    }
}

/// Fence daemon starts, request a cooperative exit from the current daemon, and
/// wait until its process lock is released before binary replacement begins.
///
/// The actual upgrade owner must already hold the upgrade transaction lock.
/// This handoff deliberately does not schedule or serialize upgrades.
pub fn begin_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
) -> Result<DaemonUpgradeHandoff> {
    let expected_executable = env::current_exe().context("resolve upgrading ctx executable")?;
    let input = normalize_daemon_upgrade_handoff_input(
        data_root,
        upgrade_attempt_id,
        &expected_executable,
    )?;
    begin_daemon_upgrade_handoff_with(input)
}

fn begin_daemon_upgrade_handoff_with(
    input: DaemonUpgradeHandoffInput,
) -> Result<DaemonUpgradeHandoff> {
    let DaemonUpgradeHandoffInput {
        data_root,
        handoff_id,
        expected_process,
        persisted_restart_label,
        handoff_path,
        restart_request_root,
        mut cooperative_stop,
    } = input;
    let lifecycle_transition = DaemonLifecycleTransitionLock::acquire(&data_root)?;
    match daemon_upgrade_handoff_state_at(&handoff_path) {
        DaemonUpgradeHandoffState::Active => {
            return Err(anyhow!(
                "another ctx upgrade owns the daemon lifecycle handoff"
            ));
        }
        DaemonUpgradeHandoffState::CorruptOrUnreadable => {
            return Err(anyhow!(
                "daemon upgrade handoff state is corrupt or unreadable"
            ));
        }
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => {}
    }
    persist_handoff_before_cooperative_stop(
        &handoff_path,
        &restart_request_root,
        &handoff_id,
        persisted_restart_label.as_deref(),
        cooperative_stop.as_mut(),
    )?;
    drop(lifecycle_transition);
    let mut handoff = DaemonUpgradeHandoff {
        data_root: data_root.to_path_buf(),
        fence: ctx_daemon_runtime::DurableHandoffFence::armed(handoff_path.clone(), handoff_id),
        installation_executable: expected_process.executable.clone(),
        persisted_restart_label,
        persisted_loop_interval_seconds: None,
    };
    let deadline = Instant::now() + DAEMON_UPGRADE_STOP_TIMEOUT;
    while daemon_lock_is_active(&data_root) {
        if Instant::now() >= deadline {
            #[cfg(any(unix, windows))]
            {
                terminate_identity_verified_residual_daemon(
                    &data_root,
                    &expected_process.executable,
                )
                .context("stop residual ctx daemon before upgrade")?;
                break;
            }
            #[cfg(not(any(unix, windows)))]
            return Err(anyhow!(
                "timed out waiting for the ctx daemon to stop before upgrade"
            ));
        }
        std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
    }
    wait_for_daemon_lifecycle_release(&data_root)?;
    write_daemon_upgrade_handoff_at(&handoff_path, handoff.fence.handoff_id(), "ready", None)?;
    handoff.wait_for_installation_quiescence()?;
    handoff.persisted_loop_interval_seconds = read_installation_daemon_restarts(
        &handoff.installation_executable,
        handoff.fence.handoff_id(),
    )?
    .into_iter()
    .find(|restart| restart.data_root == handoff.data_root)
    .and_then(|restart| restart.loop_interval_seconds);
    Ok(handoff)
}

fn persist_handoff_before_cooperative_stop(
    handoff_path: &Path,
    restart_request_root: &Path,
    handoff_id: &str,
    persisted_restart_label: Option<&str>,
    cooperative_stop: &mut dyn CooperativeStopPort,
) -> Result<()> {
    ctx_daemon_runtime::persist_handoff_before_stop(
        handoff_path,
        restart_request_root,
        handoff_id,
        persisted_restart_label,
        || cooperative_stop.request_stop(),
    )
}

/// Fence new daemon starts while the daemon that owns `data_root` is still
/// quiescing. Unlike the manual path, this must not wait for the daemon lock:
/// the caller is that daemon and will release the lock only after this fence is
/// durable.
pub fn begin_current_daemon_upgrade_handoff(
    data_root: &Path,
    upgrade_attempt_id: &str,
    restart_trigger: DaemonTriggerCommandArg,
    loop_interval_seconds: Option<u64>,
) -> Result<DaemonUpgradeHandoff> {
    if !ctx_upgrade_engine::is_valid_upgrade_attempt_id(upgrade_attempt_id) {
        return Err(anyhow!(
            "invalid upgrade attempt identity for daemon handoff"
        ));
    }
    let input = CurrentDaemonUpgradeHandoffInput {
        data_root: data_root.to_path_buf(),
        handoff_id: upgrade_attempt_id.to_owned(),
        persisted_restart_label: restart_trigger.as_str().to_owned(),
        loop_interval_seconds,
        installation_executable: env::current_exe().context("resolve upgrading ctx executable")?,
        current_handoff_token: env::var(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV).ok(),
        handoff_path: daemon_upgrade_handoff_path(data_root),
        restart_request_root: daemon_upgrade_restart_request_root(data_root),
    };
    begin_current_daemon_upgrade_handoff_with(input)
}

#[derive(Debug)]
struct CurrentDaemonUpgradeHandoffInput {
    data_root: PathBuf,
    handoff_id: String,
    persisted_restart_label: String,
    loop_interval_seconds: Option<u64>,
    installation_executable: PathBuf,
    current_handoff_token: Option<String>,
    handoff_path: PathBuf,
    restart_request_root: PathBuf,
}

fn begin_current_daemon_upgrade_handoff_with(
    input: CurrentDaemonUpgradeHandoffInput,
) -> Result<DaemonUpgradeHandoff> {
    let CurrentDaemonUpgradeHandoffInput {
        data_root,
        handoff_id,
        persisted_restart_label,
        loop_interval_seconds,
        installation_executable,
        current_handoff_token,
        handoff_path,
        restart_request_root,
    } = input;
    let _lifecycle_transition = DaemonLifecycleTransitionLock::acquire(&data_root)?;
    if !daemon_lock_is_active(&data_root) {
        return Err(anyhow!(
            "automatic upgrade handoff requires current daemon ownership"
        ));
    }
    match daemon_upgrade_handoff_state_at(&handoff_path) {
        DaemonUpgradeHandoffState::CorruptOrUnreadable => {
            return Err(anyhow!(
                "daemon upgrade handoff state is corrupt or unreadable"
            ));
        }
        DaemonUpgradeHandoffState::Active => {
            let current = read_daemon_upgrade_handoff_at(&handoff_path)
                .ok_or_else(|| anyhow!("active daemon handoff disappeared"))?;
            if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id.as_str())
                || !current_process_owns_daemon_upgrade_handoff_at(
                    &handoff_path,
                    current_handoff_token.as_deref(),
                )
            {
                return Err(anyhow!(
                    "another ctx upgrade owns the daemon lifecycle handoff"
                ));
            }
            return Ok(DaemonUpgradeHandoff {
                data_root,
                fence: ctx_daemon_runtime::DurableHandoffFence::armed(handoff_path, handoff_id),
                installation_executable,
                persisted_restart_label: Some(persisted_restart_label),
                persisted_loop_interval_seconds: loop_interval_seconds,
            });
        }
        DaemonUpgradeHandoffState::Absent | DaemonUpgradeHandoffState::Terminal => {}
    }
    write_daemon_restart_request_at(&restart_request_root, &persisted_restart_label, &handoff_id)?;
    write_daemon_upgrade_handoff_at(&handoff_path, &handoff_id, "ready", None)?;
    Ok(DaemonUpgradeHandoff {
        data_root,
        fence: ctx_daemon_runtime::DurableHandoffFence::armed(handoff_path, handoff_id),
        installation_executable,
        persisted_restart_label: Some(persisted_restart_label),
        persisted_loop_interval_seconds: loop_interval_seconds,
    })
}

fn pause_after_installation_quiescence_for_test() -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let Some(path) = env::var_os("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, b"ready\n")?;
    let release = path.with_extension("continue");
    let deadline = Instant::now() + StdDuration::from_secs(15);
    while !release.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting to continue after test installation quiescence"
            ));
        }
        std::thread::sleep(StdDuration::from_millis(25));
    }
    Ok(())
}

/// Make helper ownership durable before its parent accepts the readiness
/// receipt. This closes the parent-exit window in which a live replacement
/// helper could otherwise lose the daemon-start fence.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn mark_replacement_helper_handoff(
    data_root: &Path,
    handoff_id: &str,
    helper_pid: u32,
) -> Result<()> {
    if helper_pid == 0 {
        return Err(anyhow!("replacement helper PID must be nonzero"));
    }
    let current = read_daemon_upgrade_handoff(data_root)
        .ok_or_else(|| anyhow!("replacement helper has no daemon handoff"))?;
    if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
        return Err(anyhow!(
            "replacement helper daemon handoff identity does not match"
        ));
    }
    write_daemon_upgrade_handoff(data_root, handoff_id, "scheduled", Some(helper_pid))
}

/// Complete a durable replacement handoff from the Windows helper.
///
/// The helper passes the origin-root identity and persistent restart intent
/// captured before the old daemon stopped. Success means either no daemon had been
/// running, or the replacement process owns the existing daemon lifecycle
/// lock; a successful `spawn` alone is never treated as readiness.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn complete_replacement_daemon_handoff(
    data_root: &Path,
    executable: &Path,
    handoff_id: &str,
    restart: Option<(&str, Option<u64>)>,
) -> Result<()> {
    if let Some(current) = read_daemon_upgrade_handoff(data_root) {
        if current.get("handoff_id").and_then(Value::as_str) != Some(handoff_id) {
            return Err(anyhow!(
                "replacement daemon handoff identity does not match its install journal"
            ));
        }
    }
    let captured_restart = restart
        .map(|(trigger, loop_interval_seconds)| {
            parse_daemon_trigger(Some(trigger))
                .map(|trigger| (trigger, loop_interval_seconds))
                .ok_or_else(|| anyhow!("replacement daemon handoff has an invalid trigger"))
        })
        .transpose()?;
    let captured_loop_interval_seconds =
        captured_restart.and_then(|(_, loop_interval_seconds)| loop_interval_seconds);
    let selected_restart = ctx_daemon_runtime::close_daemon_handoff_restart_intake(
        data_root,
        handoff_id,
        captured_restart.map(|(trigger, _)| trigger.as_str()),
    )?;
    if let Some(selected_restart) = selected_restart {
        let trigger = parse_daemon_trigger(Some(&selected_restart))
            .ok_or_else(|| anyhow!("replacement daemon handoff has an invalid restart request"))?;
        if !daemon_lock_is_active(data_root) {
            // Recreate the durable acknowledgement token if an earlier ready
            // daemon consumed it and then exited before handoff completion.
            if read_daemon_restart_request(data_root).is_none() {
                ctx_daemon_runtime::write_finalizing_daemon_restart_request(
                    data_root,
                    handoff_id,
                    trigger.as_str(),
                )?;
            }
            let mut upgrade_fence = ReplacementHandoffSupervisorFence {
                data_root,
                handoff_id,
            };
            let supervisor_resume =
                super::super::daemon_supervisor::resume_daemon_supervisor_after_upgrade(
                    data_root,
                    executable,
                    captured_loop_interval_seconds,
                    &mut upgrade_fence,
                )?;
            match supervisor_resume {
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Native => {
                    wait_for_daemon_ready_ack(data_root)?;
                }
                super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::Fallback
                | super::super::daemon_supervisor::DaemonSupervisorUpgradeResume::ManagerUnavailable => {
                    let launch = daemon_autostart_command(
                        executable,
                        data_root,
                        trigger,
                        captured_loop_interval_seconds,
                        Some(handoff_id),
                    )?;
                    let mut child = spawn_daemon_child(launch)
                        .context("restart persistent ctx daemon after replacement")?;
                    wait_for_replacement_daemon(data_root, &mut child)?;
                }
            }
        } else {
            wait_for_daemon_ready_ack(data_root)?;
        }
        if !daemon_lock_is_active(data_root) || read_daemon_restart_request(data_root).is_some() {
            return Err(anyhow!(
                "replacement ctx daemon did not reach lifecycle readiness"
            ));
        }
    }
    Ok(())
}

/// Mark the helper-owned handoff complete only after its terminal journal is
/// durable and its installation lock has been released.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn finish_replacement_daemon_handoff(data_root: &Path, handoff_id: &str) -> Result<()> {
    ctx_daemon_runtime::finish_replacement_daemon_handoff(data_root, handoff_id)
}

pub(super) fn write_daemon_upgrade_handoff(
    data_root: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    write_daemon_upgrade_handoff_at(
        &daemon_upgrade_handoff_path(data_root),
        handoff_id,
        phase,
        helper_pid,
    )
}

fn write_daemon_upgrade_handoff_at(
    handoff_path: &Path,
    handoff_id: &str,
    phase: &str,
    helper_pid: Option<u32>,
) -> Result<()> {
    ctx_daemon_runtime::write_handoff_marker_at(handoff_path, handoff_id, phase, helper_pid)
}

pub(crate) fn write_daemon_restart_request(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> Result<PathBuf> {
    ctx_daemon_runtime::write_daemon_restart_request_if_intake_open(
        data_root,
        trigger.as_str(),
        request_id,
    )
}

pub(crate) fn defer_restart_for_upgrade_handoff(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> Result<Option<ctx_daemon_runtime::DaemonHandoffRestartDeferral>> {
    ctx_daemon_runtime::defer_restart_for_active_daemon_handoff(
        data_root,
        trigger.as_str(),
        request_id,
        DAEMON_UPGRADE_HANDOFF_STALE_AFTER,
    )
}

fn write_daemon_restart_request_at(
    restart_request_root: &Path,
    persisted_restart_label: &str,
    request_id: &str,
) -> Result<PathBuf> {
    ctx_daemon_runtime::write_restart_request_at(
        restart_request_root,
        persisted_restart_label,
        request_id,
    )
}

pub(crate) fn read_daemon_restart_request(
    data_root: &Path,
) -> Option<(PathBuf, DaemonTriggerCommandArg)> {
    ctx_daemon_runtime::read_restart_requests_at(&daemon_upgrade_restart_request_root(data_root))
        .into_iter()
        .find_map(|(path, label)| parse_daemon_trigger(Some(&label)).map(|trigger| (path, trigger)))
}

#[cfg(test)]
fn read_daemon_restart_request_at(restart_request_root: &Path) -> Option<(PathBuf, String)> {
    read_daemon_restart_requests_at(restart_request_root)
        .into_iter()
        .next()
}

#[cfg(test)]
fn read_daemon_restart_requests_at(restart_request_root: &Path) -> Vec<(PathBuf, String)> {
    ctx_daemon_runtime::read_restart_requests_at(restart_request_root)
}

pub(super) fn remove_daemon_restart_requests(data_root: &Path) {
    ctx_daemon_runtime::remove_restart_requests_at(&daemon_upgrade_restart_request_root(data_root));
}

pub(crate) fn acknowledge_daemon_restart_requests(data_root: &Path) {
    remove_daemon_restart_requests(data_root);
}

#[cfg(test)]
mod seam_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct ObservingStop {
        handoff_path: PathBuf,
        restart_root: PathBuf,
        calls: Arc<AtomicUsize>,
    }

    impl CooperativeStopPort for ObservingStop {
        fn request_stop(&mut self) {
            let handoff = read_daemon_upgrade_handoff_at(&self.handoff_path)
                .expect("handoff fence must precede cooperative stop");
            assert_eq!(handoff["phase"], "preparing");
            let (_, label) = read_daemon_restart_request_at(&self.restart_root)
                .expect("restart intent must precede cooperative stop");
            assert_eq!(label, "opaque-restart-label-v9");
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn opaque_restart_intent_and_fence_are_durable_before_cooperative_stop() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_path = temp.path().join("handoff.json");
        let restart_root = temp.path().join("restart");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut stop = ObservingStop {
            handoff_path: handoff_path.clone(),
            restart_root: restart_root.clone(),
            calls: Arc::clone(&calls),
        };

        persist_handoff_before_cooperative_stop(
            &handoff_path,
            &restart_root,
            "opaque-handoff-id",
            Some("opaque-restart-label-v9"),
            &mut stop,
        )?;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn cooperative_stop_is_not_called_when_handoff_persistence_fails() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let blocked_parent = temp.path().join("not-a-directory");
        fs::write(&blocked_parent, b"blocked")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let mut stop = ObservingStop {
            handoff_path: blocked_parent.join("handoff.json"),
            restart_root: temp.path().join("restart"),
            calls: Arc::clone(&calls),
        };

        assert!(persist_handoff_before_cooperative_stop(
            &stop.handoff_path.clone(),
            &stop.restart_root.clone(),
            "opaque-handoff-id",
            Some("opaque-restart-label-v9"),
            &mut stop,
        )
        .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn product_restart_reader_skips_unknown_opaque_labels_without_rewriting_schema() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let restart_root = daemon_upgrade_restart_request_root(temp.path());
        write_daemon_restart_request_at(
            &restart_root,
            "opaque-future-restart-label",
            "000-opaque",
        )?;
        write_daemon_restart_request_at(&restart_root, "search", "001-product")?;

        let (_, trigger) = read_daemon_restart_request(temp.path())
            .expect("product reader must continue past unknown opaque labels");
        assert_eq!(trigger.as_str(), "search");
        let (_, opaque) = read_daemon_restart_request_at(&restart_root)
            .expect("generic reader preserves the first opaque label");
        assert_eq!(opaque, "opaque-future-restart-label");
        Ok(())
    }

    #[test]
    fn fresh_corrupt_handoff_is_a_start_fence_not_an_absent_handoff() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let handoff_path = daemon_upgrade_handoff_path(temp.path());
        fs::create_dir_all(handoff_path.parent().expect("handoff parent"))?;
        fs::write(&handoff_path, b"{not-json")?;

        assert_eq!(
            daemon_upgrade_handoff_state_at(&handoff_path),
            DaemonUpgradeHandoffState::CorruptOrUnreadable
        );
        assert!(daemon_upgrade_handoff_fences_start(temp.path()));
        assert!(daemon_upgrade_handoff_blocks_current_process(temp.path()));
        Ok(())
    }

    #[test]
    fn restart_reader_returns_at_first_recognized_trigger_in_path_order() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let restart_root = daemon_upgrade_restart_request_root(temp.path());
        write_daemon_restart_request_at(&restart_root, "search", "000-first")?;
        // A directory here would make a full traversal perform another failed
        // file read. The selected first request must make it irrelevant.
        fs::create_dir(restart_root.join("001-never-read.json"))?;

        let (path, trigger) = read_daemon_restart_request(temp.path())
            .expect("first recognized trigger must be selected");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("000-first.json")
        );
        assert_eq!(trigger.as_str(), DaemonTriggerCommandArg::Search.as_str());
        Ok(())
    }
}
