use super::diagnostics::ssh_log_snippet;
use super::*;

mod reclaim;
#[cfg(test)]
mod tests;
mod transport;

#[cfg(test)]
use reclaim::{health_reports_expected_pid, reclaim_complete, reclaim_health_probe_timeout};
pub(crate) use reclaim::{
    normalize_daemon_pid, reclaim_incompatible_local_daemon,
    should_reclaim_incompatible_local_daemon, terminate_pid, wait_for_daemon_reclaim,
};
pub(crate) use transport::daemon_health_with_auth;
#[cfg(test)]
use transport::{
    daemon_health_client_build_count, daemon_health_with_timeout,
    reset_daemon_health_client_build_count,
};

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct DaemonHealthCompatibility {
    #[serde(default)]
    pub(super) desktop_exact_version: String,
    #[serde(default)]
    pub(super) desktop_build_id: String,
    #[serde(default)]
    pub(super) desktop_dev_instance_id: String,
    #[serde(default)]
    pub(super) protocol_compatibility_token: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct DaemonHealthSummary {
    #[serde(default)]
    pub(crate) pid: u32,
    #[serde(default)]
    pub(super) data_root: String,
    #[serde(default)]
    pub(super) compatibility: DaemonHealthCompatibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonCompatibilityState {
    Exact,
    CompatibleMismatch,
    IncompatibleMismatch,
}

impl DaemonHealthCompatibility {
    pub(crate) fn protocol_token(&self) -> &str {
        self.protocol_compatibility_token.trim()
    }
}

fn normalize_path_for_compare(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

pub(crate) fn local_daemon_health_matches_expected(
    health: &DaemonHealthSummary,
    expected_data_dir: &Path,
    expected_identity: &DesktopBuildIdentity,
) -> bool {
    let daemon_data_root = health.data_root.trim();
    if daemon_data_root.is_empty() {
        return false;
    }
    let daemon_root = normalize_path_for_compare(Path::new(daemon_data_root));
    let expected_root = normalize_path_for_compare(expected_data_dir);
    if daemon_root != expected_root {
        return false;
    }
    let expected_version = expected_identity.exact_version.trim();
    if expected_version.is_empty() {
        return false;
    }
    if health.compatibility.desktop_exact_version.trim() != expected_version {
        return false;
    }
    let expected_build_id = expected_identity.build_id.trim();
    if expected_build_id.is_empty() {
        return false;
    }
    if health.compatibility.desktop_build_id.trim() != expected_build_id {
        return false;
    }
    let expected_compatibility_token = expected_identity.compatibility_token.trim();
    if expected_compatibility_token.is_empty() {
        return false;
    }
    if health.compatibility.protocol_token() != expected_compatibility_token {
        return false;
    }
    true
}

pub(crate) fn classify_daemon_compatibility(
    health: &DaemonHealthSummary,
    expected_identity: &DesktopBuildIdentity,
) -> DaemonCompatibilityState {
    let expected_token = expected_identity.compatibility_token.trim();
    if expected_token.is_empty() {
        return DaemonCompatibilityState::IncompatibleMismatch;
    }
    if health.compatibility.protocol_token() != expected_token {
        return DaemonCompatibilityState::IncompatibleMismatch;
    }
    let version_matches =
        health.compatibility.desktop_exact_version.trim() == expected_identity.exact_version.trim();
    let build_matches =
        health.compatibility.desktop_build_id.trim() == expected_identity.build_id.trim();
    if version_matches && build_matches {
        DaemonCompatibilityState::Exact
    } else {
        DaemonCompatibilityState::CompatibleMismatch
    }
}

fn display_nonempty(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "<empty>".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn spawned_local_daemon_incompatibility_message(
    base_url: &str,
    expected_data_dir: &Path,
    expected_identity: &DesktopBuildIdentity,
    health: &DaemonHealthSummary,
) -> String {
    format!(
        "spawned local daemon is incompatible (expected_version={}, daemon_version={}, expected_build_id={}, daemon_build_id={}, expected_dev_instance_id={}, daemon_dev_instance_id={}, expected_data_dir={}, daemon_data_root={}, daemon_pid={}, url={})",
        display_nonempty(&expected_identity.exact_version),
        display_nonempty(&health.compatibility.desktop_exact_version),
        display_nonempty(&expected_identity.build_id),
        display_nonempty(&health.compatibility.desktop_build_id),
        display_nonempty(&expected_identity.compatibility_token),
        display_nonempty(&health.compatibility.desktop_dev_instance_id),
        expected_data_dir.display(),
        display_nonempty(&health.data_root),
        health.pid,
        base_url,
    )
}

pub(crate) fn existing_local_daemon_matches(
    base_url: &str,
    expected_data_dir: &Path,
    expected_identity: &DesktopBuildIdentity,
) -> Result<bool> {
    existing_local_daemon_matches_with_auth(base_url, None, expected_data_dir, expected_identity)
}

pub(crate) fn existing_local_daemon_matches_with_auth(
    base_url: &str,
    auth_token: Option<&str>,
    expected_data_dir: &Path,
    expected_identity: &DesktopBuildIdentity,
) -> Result<bool> {
    let health = daemon_health_with_auth(base_url, auth_token)?;
    Ok(local_daemon_health_matches_expected(
        &health,
        expected_data_dir,
        expected_identity,
    ))
}

pub(crate) fn existing_local_daemon_matches_or_absent(
    base_url: &str,
    expected_data_dir: &Path,
    expected_identity: &DesktopBuildIdentity,
) -> bool {
    existing_local_daemon_matches(base_url, expected_data_dir, expected_identity).unwrap_or(false)
}

pub(crate) fn probe_daemon_health(base_url: &str) -> Result<()> {
    probe_daemon_health_with_auth(base_url, None)
}

pub(crate) fn probe_daemon_health_with_auth(
    base_url: &str,
    auth_token: Option<&str>,
) -> Result<()> {
    let _ = daemon_health_with_auth(base_url, auth_token)?;
    Ok(())
}

pub(crate) fn probe_local_daemon_health_with_retry_auth(
    base_url: &str,
    auth_token: Option<&str>,
) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..LOCAL_DAEMON_HEALTH_RETRIES {
        match probe_daemon_health_with_auth(base_url, auth_token) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
        let delay = LOCAL_DAEMON_HEALTH_BASE_DELAY_MS.saturating_mul((attempt + 1) as u64);
        std::thread::sleep(Duration::from_millis(delay));
    }
    Err(last_err.unwrap_or_else(|| anyhow!("requesting /api/health failed")))
}

pub(crate) fn probe_daemon_health_with_retry(
    base_url: &str,
    auth_token: Option<&str>,
    local_port: u16,
    tunnel: &mut Child,
    stderr_log: &std::sync::Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..SSH_TUNNEL_HEALTH_RETRIES {
        match probe_daemon_health_with_auth(base_url, auth_token) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = Some(err);
                if let Ok(Some(status)) = tunnel.try_wait() {
                    let stderr = ssh_log_snippet(stderr_log);
                    if stderr.is_empty() {
                        return Err(anyhow!("ssh tunnel exited ({status})"));
                    }
                    return Err(anyhow!("ssh tunnel exited ({status}): {stderr}"));
                }
            }
        }
        let delay = SSH_TUNNEL_HEALTH_BASE_DELAY_MS.saturating_mul((attempt + 1) as u64);
        std::thread::sleep(Duration::from_millis(delay));
    }
    let err = last_err.unwrap_or_else(|| anyhow!("requesting /api/health failed"));
    let stderr = ssh_log_snippet(stderr_log);
    if stderr.is_empty() {
        Err(anyhow!(
            "daemon did not become healthy at {base_url} (local_port={local_port}): {err:#}"
        ))
    } else {
        Err(anyhow!(
            "daemon did not become healthy at {base_url} (local_port={local_port}): {err:#}; ssh stderr: {stderr}"
        ))
    }
}
