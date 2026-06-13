use super::types::{ActiveConnection, LocalConnectionOwnership, SshConnection, SshRuntimeMetadata};
use super::*;

const LOCAL_DAEMON_SHUTDOWN_TOKEN_HEADER: &str = "x-ctx-local-daemon-shutdown-token";

#[derive(Debug, Clone, Copy)]
enum LocalDaemonStopMode {
    BackgroundDrain,
    RequireReclaim,
}

fn request_daemon_shutdown_drain(
    base_url: &str,
    auth_token: &str,
    local_shutdown_token: Option<&str>,
) -> Result<()> {
    let url = format!("{}/api/daemon/shutdown", base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1800))
        .build()
        .context("building daemon shutdown client")?;
    let mut request = client
        .post(url)
        .bearer_auth(auth_token);
    if let Some(token) = local_shutdown_token {
        request = request.header(LOCAL_DAEMON_SHUTDOWN_TOKEN_HEADER, token);
    }
    let response = request
        .json(&serde_json::json!({
            "confirm": true,
            "reason": "desktop_quit",
        }))
        .send()
        .context("requesting daemon shutdown drain")?;
    if !response.status().is_success() {
        anyhow::bail!("daemon shutdown drain failed with status {}", response.status());
    }
    Ok(())
}

fn stop_owned_local_daemon_child(
    base_url: &str,
    auth_token: &str,
    local_shutdown_token: Option<&str>,
    mut child: Child,
    systemd_scope: bool,
    mode: LocalDaemonStopMode,
) -> Result<()> {
    let pid = child.id();
    let drain_err = request_daemon_shutdown_drain(base_url, auth_token, local_shutdown_token).err();
    if drain_err.is_none() && matches!(mode, LocalDaemonStopMode::BackgroundDrain) {
        return Ok(());
    }
    if drain_err.is_none()
        && wait_for_daemon_reclaim(base_url, pid, Duration::from_secs(3), Some(auth_token)).is_ok()
    {
        let _ = child.wait();
        return Ok(());
    }

    if systemd_scope {
        stop_systemd_scope("ctx-daemon");
        if let Some(scope) = systemd_scope_for_local_daemon_url(base_url) {
            stop_systemd_scope(&scope);
        }
    }

    let graceful_err = terminate_pid(pid, false).err();
    if wait_for_daemon_reclaim(base_url, pid, Duration::from_secs(3), Some(auth_token)).is_ok() {
        let _ = child.wait();
        return Ok(());
    }
    if let Some(err) = drain_err {
        eprintln!("failed to request local daemon shutdown drain {pid}: {err:#}");
    }
    if let Some(err) = graceful_err {
        eprintln!("failed to gracefully terminate local daemon child {pid}: {err:#}");
    }

    try_kill_child(child)
}

fn cleanup_active_connection_result(
    active: ActiveConnection,
    mode: LocalDaemonStopMode,
) -> Result<()> {
    match active {
        ActiveConnection::Local(c) => {
            let super::types::LocalConnection {
                base_url,
                token,
                local_shutdown_token,
                ownership,
                ..
            } = c;
            match ownership {
                LocalConnectionOwnership::OwnedChild {
                    child,
                    systemd_scope,
                } => stop_owned_local_daemon_child(
                    &base_url,
                    &token,
                    local_shutdown_token.as_deref(),
                    child,
                    systemd_scope,
                    mode,
                ),
                LocalConnectionOwnership::UnownedExternal => Ok(()),
            }
        }
        ActiveConnection::Ssh(c) => try_kill_child(c.tunnel),
    }
}

pub(super) fn cleanup_active_connection(active: ActiveConnection) {
    let _ = cleanup_active_connection_result(active, LocalDaemonStopMode::BackgroundDrain);
}

pub(super) fn cleanup_active_connection_result_for_restart(active: ActiveConnection) -> Result<()> {
    cleanup_active_connection_result(active, LocalDaemonStopMode::RequireReclaim)
}

pub(super) fn build_ssh_connection(
    base_url: String,
    token: Option<String>,
    tunnel: Child,
    host: String,
    user: Option<String>,
    remote_port: u16,
    remote_data_dir: Option<String>,
    runtime: SshRuntimeMetadata,
) -> ActiveConnection {
    ActiveConnection::Ssh(SshConnection {
        base_url,
        token,
        tunnel,
        host,
        user,
        remote_port,
        remote_data_dir,
        runtime,
        remote_update_status: None,
        http_client: std::sync::OnceLock::new(),
    })
}

pub(super) fn should_preserve_local_handoff(
    base_url: &str,
    token: &str,
    daemon_pid: Option<u32>,
    previous_base_url: &str,
    previous_token: &str,
    previous_daemon_pid: Option<u32>,
) -> bool {
    daemon_pid.is_some()
        && daemon_pid == previous_daemon_pid
        && previous_base_url == base_url
        && previous_token == token
}
