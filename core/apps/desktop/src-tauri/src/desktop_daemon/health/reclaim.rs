use super::super::login_relay::is_loopback_host_name;
use super::transport::{daemon_health_with_auth, daemon_health_with_timeout_auth};
use super::*;

pub(crate) fn normalize_daemon_pid(pid: u32) -> Option<u32> {
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

pub(crate) fn should_reclaim_incompatible_local_daemon(
    base_url: &str,
    health: &DaemonHealthSummary,
    expected_data_dir: &Path,
) -> bool {
    if health.pid == 0 {
        return false;
    }
    let Ok(parsed) = Url::parse(base_url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if !is_loopback_host_name(host) {
        return false;
    }
    let daemon_data_root = health.data_root.trim();
    if daemon_data_root.is_empty() {
        return false;
    }
    let daemon_root = normalize_path_for_compare(Path::new(daemon_data_root));
    let expected_root = normalize_path_for_compare(expected_data_dir);
    daemon_root == expected_root
}

pub(crate) fn reclaim_incompatible_local_daemon(
    base_url: &str,
    health: &DaemonHealthSummary,
    auth_token: Option<&str>,
) -> Result<()> {
    if health.pid == 0 {
        anyhow::bail!("incompatible local daemon missing pid");
    }
    let pid = health.pid;
    let graceful_revalidated = daemon_reports_expected_pid_with_auth(base_url, pid, auth_token);
    let graceful_err = if graceful_revalidated {
        terminate_pid(pid, false).err()
    } else {
        None
    };
    if wait_for_daemon_reclaim(base_url, pid, Duration::from_secs(3), auth_token).is_ok() {
        return Ok(());
    }
    let force_revalidated = daemon_reports_expected_pid_with_auth(base_url, pid, auth_token);
    let force_err = if force_revalidated {
        terminate_pid(pid, true).err()
    } else {
        None
    };
    if wait_for_daemon_reclaim(base_url, pid, Duration::from_secs(2), auth_token).is_ok() {
        return Ok(());
    }
    let mut details = Vec::new();
    if !graceful_revalidated {
        details.push(
            "skipped graceful terminate (daemon pid could not be revalidated via /api/health)"
                .to_string(),
        );
    }
    if let Some(err) = graceful_err {
        details.push(format!("graceful terminate failed: {err:#}"));
    }
    if !force_revalidated {
        details.push(
            "skipped force terminate (daemon pid could not be revalidated via /api/health)"
                .to_string(),
        );
    }
    if let Some(err) = force_err {
        details.push(format!("force terminate failed: {err:#}"));
    }
    if details.is_empty() {
        anyhow::bail!("incompatible local daemon pid {} did not exit", pid);
    }
    anyhow::bail!(
        "incompatible local daemon pid {} did not exit ({})",
        pid,
        details.join("; ")
    );
}

fn wait_until_daemon_reclaimed(
    base_url: &str,
    pid: u32,
    timeout: Duration,
    auth_token: Option<&str>,
) -> bool {
    const RECLAIM_HEALTH_PROBE_MAX_TIMEOUT: Duration = Duration::from_millis(250);
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let health_timeout =
            reclaim_health_probe_timeout(remaining, RECLAIM_HEALTH_PROBE_MAX_TIMEOUT);
        let health = daemon_health_with_timeout_auth(base_url, auth_token, health_timeout).ok();
        let pid_alive = is_pid_alive(pid).unwrap_or(true);
        if reclaim_complete(pid, pid_alive, health.as_ref()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

pub(crate) fn wait_for_daemon_reclaim(
    base_url: &str,
    pid: u32,
    timeout: Duration,
    auth_token: Option<&str>,
) -> Result<()> {
    if pid == 0 {
        anyhow::bail!("invalid pid 0");
    }
    if wait_until_daemon_reclaimed(base_url, pid, timeout, auth_token) {
        return Ok(());
    }
    anyhow::bail!("local daemon pid {pid} did not exit within {:?}", timeout);
}

pub(super) fn reclaim_health_probe_timeout(
    remaining: Duration,
    max_probe_timeout: Duration,
) -> Duration {
    if remaining.is_zero() {
        return Duration::from_millis(1);
    }
    std::cmp::min(remaining, max_probe_timeout)
}

pub(super) fn reclaim_complete(
    pid: u32,
    pid_alive: bool,
    health: Option<&DaemonHealthSummary>,
) -> bool {
    let same_pid_serving_health = health.map(|h| h.pid == pid).unwrap_or(false);
    !pid_alive && !same_pid_serving_health
}

fn daemon_reports_expected_pid_with_auth(
    base_url: &str,
    pid: u32,
    auth_token: Option<&str>,
) -> bool {
    let health = daemon_health_with_auth(base_url, auth_token).ok();
    health_reports_expected_pid(pid, health.as_ref())
}

pub(super) fn health_reports_expected_pid(pid: u32, health: Option<&DaemonHealthSummary>) -> bool {
    health.map(|h| h.pid == pid).unwrap_or(false)
}

fn is_pid_alive(pid: u32) -> Result<bool> {
    if pid == 0 {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        let output = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .output()
            .with_context(|| format!("running kill -0 {pid}"))?;
        if output.status.success() {
            return Ok(true);
        }
        if command_reports_missing_process(&output) {
            return Ok(false);
        }
        if command_reports_permission_denied(&output) {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("kill -0 {pid} failed: {stderr}");
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .arg("/FI")
            .arg(format!("PID eq {pid}"))
            .arg("/FO")
            .arg("CSV")
            .arg("/NH")
            .output()
            .with_context(|| format!("running tasklist for pid {pid}"))?;
        if !output.status.success() {
            if command_reports_missing_process(&output) {
                return Ok(false);
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            anyhow::bail!("tasklist pid {pid} failed: {stderr}");
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let pid_token = format!(",\"{pid}\",");
        if stdout.contains(&pid_token) {
            return Ok(true);
        }

        if command_reports_missing_process(&output) {
            return Ok(false);
        }
        Ok(false)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        anyhow::bail!("pid liveness checks are unsupported on this platform");
    }
}

pub(crate) fn terminate_pid(pid: u32, force: bool) -> Result<()> {
    if pid == 0 {
        anyhow::bail!("invalid pid 0");
    }

    #[cfg(unix)]
    {
        let signal = if force { "-KILL" } else { "-TERM" };
        let output = Command::new("kill")
            .arg(signal)
            .arg(pid.to_string())
            .output()
            .with_context(|| format!("running kill {signal} {pid}"))?;
        if output.status.success() || command_reports_missing_process(&output) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("kill {signal} {pid} failed: {stderr}");
    }

    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.arg("/PID").arg(pid.to_string()).arg("/T");
        if force {
            cmd.arg("/F");
        }
        let output = cmd
            .output()
            .with_context(|| format!("running taskkill for pid {pid}"))?;
        if output.status.success() || command_reports_missing_process(&output) {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("taskkill pid {pid} failed: {stderr}");
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, force);
        anyhow::bail!("process termination is unsupported on this platform");
    }
}

fn command_reports_missing_process(output: &std::process::Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    stdout.contains("no such process")
        || stderr.contains("no such process")
        || stdout.contains("not found")
        || stderr.contains("not found")
        || stdout.contains("not running")
        || stderr.contains("not running")
        || stdout.contains("no running instance")
        || stderr.contains("no running instance")
}

fn command_reports_permission_denied(output: &std::process::Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    stdout.contains("operation not permitted")
        || stderr.contains("operation not permitted")
        || stdout.contains("permission denied")
        || stderr.contains("permission denied")
}
