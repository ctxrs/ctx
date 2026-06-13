use super::*;
use crate::desktop_daemon::daemon_health_with_auth;
use ctx_desktop_ipc::DesktopRemoteDaemonUpdateState;

#[path = "update/self_update.rs"]
mod self_update;

pub(super) use self_update::run_remote_daemon_self_update;
#[cfg(test)]
pub(super) use self_update::{
    remote_backup_ctx_bin_cmd, remote_cleanup_backup_ctx_bin_cmd, remote_restore_ctx_bin_cmd,
    remote_stop_daemon_cmd, remote_update_backup_ctx_bin,
};

const REMOTE_UPDATE_HEALTH_RETRIES: usize = 24;
const REMOTE_UPDATE_HEALTH_DELAY_MS: u64 = 500;
const REMOTE_PENDING_UPDATE_RETRY_MS: u64 = 5_000;

type RemoteUpdateKeySet = std::sync::Mutex<std::collections::HashSet<String>>;
type RemoteUpdateKeySetGuard =
    std::sync::MutexGuard<'static, std::collections::HashSet<String>>;

fn pending_remote_update_workers() -> &'static RemoteUpdateKeySet {
    static WORKERS: std::sync::OnceLock<RemoteUpdateKeySet> = std::sync::OnceLock::new();
    WORKERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn inflight_remote_updates() -> &'static RemoteUpdateKeySet {
    static UPDATES: std::sync::OnceLock<RemoteUpdateKeySet> = std::sync::OnceLock::new();
    UPDATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn remote_update_key_set_guard(set: &'static RemoteUpdateKeySet) -> RemoteUpdateKeySetGuard {
    match set.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(super) fn remote_update_target_key(
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
) -> String {
    format!(
        "{}|{}|{}|{}",
        host.trim(),
        user.unwrap_or("").trim(),
        remote_port,
        remote_data_dir.unwrap_or("").trim()
    )
}

pub(super) fn remote_update_target_key_for_target(target: &SshConnectionTarget) -> String {
    remote_update_target_key(
        &target.host,
        target.user.as_deref(),
        target.remote_port,
        target.remote_data_dir.as_deref(),
    )
}

fn mark_pending_remote_update_worker(target_key: &str) -> bool {
    let mut guard = remote_update_key_set_guard(pending_remote_update_workers());
    guard.insert(target_key.to_string())
}

fn clear_pending_remote_update_worker(target_key: &str) {
    let mut guard = remote_update_key_set_guard(pending_remote_update_workers());
    guard.remove(target_key);
}

struct RemoteUpdateSingleflightGuard {
    target_key: String,
}

impl Drop for RemoteUpdateSingleflightGuard {
    fn drop(&mut self) {
        let mut guard = remote_update_key_set_guard(inflight_remote_updates());
        guard.remove(&self.target_key);
    }
}

fn acquire_remote_update_singleflight(target_key: &str) -> Option<RemoteUpdateSingleflightGuard> {
    let mut guard = remote_update_key_set_guard(inflight_remote_updates());
    if !guard.insert(target_key.to_string()) {
        return None;
    }
    Some(RemoteUpdateSingleflightGuard {
        target_key: target_key.to_string(),
    })
}

fn trim_remote_update_message(value: &str) -> String {
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.chars().count() <= 220 {
        return text.to_string();
    }
    text.chars().take(220).collect::<String>() + "..."
}

fn mark_remote_update_failed(state: &ConnectionManager, scope: &str, message: impl Into<String>) {
    let text = trim_remote_update_message(&message.into());
    let _ = state.set_ssh_remote_update_state_for_scope(
        scope,
        DesktopRemoteDaemonUpdateState::Failed,
        if text.is_empty() { None } else { Some(text) },
    );
}

fn active_ssh_target_matches_key(state: &ConnectionManager, scope: &str, target_key: &str) -> bool {
    let Ok(target) = state.ssh_target_for_scope(scope) else {
        return false;
    };
    remote_update_target_key_for_target(&target) == target_key
}

pub(super) fn schedule_pending_remote_daemon_update(
    app: &tauri::AppHandle,
    scope: String,
    target_key: String,
    channel: String,
) {
    if !mark_pending_remote_update_worker(&target_key) {
        return;
    }
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking({
            let app_handle = app_handle.clone();
            let scope = scope.clone();
            let target_key = target_key.clone();
            let channel = channel.clone();
            move || {
                run_pending_remote_daemon_update_worker(&app_handle, &scope, &target_key, &channel)
            }
        })
        .await;
        clear_pending_remote_update_worker(&target_key);
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let state = app_handle.state::<ConnectionManager>();
                if active_ssh_target_matches_key(state.inner(), &scope, &target_key) {
                    mark_remote_update_failed(state.inner(), &scope, err.to_string());
                }
            }
            Err(err) => {
                let state = app_handle.state::<ConnectionManager>();
                if active_ssh_target_matches_key(state.inner(), &scope, &target_key) {
                    mark_remote_update_failed(
                        state.inner(),
                        &scope,
                        format!("pending remote daemon update task failed: {err}"),
                    );
                }
            }
        }
    });
}

fn run_pending_remote_daemon_update_worker(
    app: &tauri::AppHandle,
    scope: &str,
    target_key: &str,
    channel: &str,
) -> Result<()> {
    loop {
        let state = app.state::<ConnectionManager>();
        let info = state.info_for_scope(scope);
        if !matches!(info.kind, DesktopConnectionKind::Ssh) {
            return Ok(());
        }
        if info.remote_update_state != Some(DesktopRemoteDaemonUpdateState::Pending) {
            return Ok(());
        }
        let target = match state.ssh_target_for_scope(scope) {
            Ok(target) => target,
            Err(_) => return Ok(()),
        };
        if remote_update_target_key_for_target(&target) != target_key {
            return Ok(());
        }
        let base_url = info
            .base_url
            .clone()
            .ok_or_else(|| anyhow!("pending remote daemon update is missing base_url"))?;
        let token = info
            .token
            .clone()
            .ok_or_else(|| anyhow!("pending remote daemon update is missing auth token"))?;
        let expected_identity = load_desktop_build_identity(app)?;
        let health = daemon_health_with_auth(&base_url, Some(token.as_str()))
            .context("reading remote daemon health during pending update")?;
        if matches!(
            classify_daemon_compatibility(&health, &expected_identity),
            DaemonCompatibilityState::Exact
        ) {
            let _ = state.clear_ssh_remote_update_state_for_matching_target(
                &target.host,
                target.user.as_deref(),
                target.remote_port,
                target.remote_data_dir.as_deref(),
            );
            return Ok(());
        }

        let drained = begin_remote_update_drain(&base_url, &token, "desktop_pending_update")?;
        if !drained {
            std::thread::sleep(Duration::from_millis(REMOTE_PENDING_UPDATE_RETRY_MS));
            continue;
        }

        match update_current_remote_daemon_for_scope(app, state.inner(), scope, Some(channel)) {
            Ok(_) => return Ok(()),
            Err(err) => {
                release_remote_update_drain(&base_url, &token);
                return Err(err);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteUpdateTargetDecision {
    pub(super) ctx_bin: String,
    pub(super) install_managed: bool,
}

pub(super) fn resolve_remote_update_target_ctx_bin(
    recorded_active_ctx_bin: Option<String>,
    managed_remote_ctx_bin: &str,
    recorded_active_exists: bool,
    managed_exists: bool,
) -> RemoteUpdateTargetDecision {
    match recorded_active_ctx_bin.filter(|value| !value.trim().is_empty()) {
        Some(active_ctx_bin) if recorded_active_exists => RemoteUpdateTargetDecision {
            ctx_bin: active_ctx_bin,
            install_managed: false,
        },
        _ if managed_exists => RemoteUpdateTargetDecision {
            ctx_bin: managed_remote_ctx_bin.to_string(),
            install_managed: false,
        },
        _ => RemoteUpdateTargetDecision {
            ctx_bin: managed_remote_ctx_bin.to_string(),
            install_managed: true,
        },
    }
}

pub(super) fn begin_remote_update_drain(
    daemon_base_url: &str,
    token: &str,
    owner: &str,
) -> Result<bool> {
    let url = format!(
        "{}/api/updates/drain/begin",
        daemon_base_url.trim_end_matches('/')
    );
    let res = reqwest::blocking::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "confirm": true,
            "reason": "remote_daemon_update",
            "owner": owner,
        }))
        .send()
        .context("requesting remote daemon update drain")?;
    if res.status() == reqwest::StatusCode::CONFLICT {
        return Ok(false);
    }
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().unwrap_or_default();
        anyhow::bail!("remote daemon update drain failed ({status}): {body}");
    }
    Ok(true)
}

pub(super) fn release_remote_update_drain(daemon_base_url: &str, token: &str) {
    let url = format!(
        "{}/api/updates/drain/release",
        daemon_base_url.trim_end_matches('/')
    );
    let _ = reqwest::blocking::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "confirm": true }))
        .send();
}

#[tauri::command]
pub(crate) async fn desktop_update_remote_daemon(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopRemoteDaemonUpdateReq,
) -> Result<DesktopRemoteDaemonUpdateResp, String> {
    if !req.confirm {
        return Err("confirm required".to_string());
    }
    let channel = req.channel.clone();
    let app_for_update = app.clone();
    let scope = window.label().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_for_update.state::<ConnectionManager>();
        update_current_remote_daemon_for_scope(
            &app_for_update,
            state.inner(),
            &scope,
            channel.as_deref(),
        )
    })
    .await
    .map_err(|err| format!("remote daemon update task failed: {err}"))?
    .map_err(to_err)
}

#[cfg(test)]
pub(crate) fn update_current_remote_daemon(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
    requested_channel: Option<&str>,
) -> Result<DesktopRemoteDaemonUpdateResp> {
    update_current_remote_daemon_for_scope(
        app,
        state,
        DEFAULT_CONNECTION_SCOPE,
        requested_channel,
    )
}

pub(crate) fn update_current_remote_daemon_for_scope(
    app: &tauri::AppHandle,
    state: &ConnectionManager,
    scope: &str,
    requested_channel: Option<&str>,
) -> Result<DesktopRemoteDaemonUpdateResp> {
    let channel =
        resolve_desktop_update_channel(app, requested_channel).map_err(anyhow::Error::msg)?;
    let target = state.ssh_target_for_scope(scope)?;
    let target_key = remote_update_target_key_for_target(&target);
    let _singleflight = acquire_remote_update_singleflight(&target_key)
        .ok_or_else(|| anyhow!("remote daemon update already in progress"))?;
    let _ = state.clear_ssh_remote_update_state_for_matching_target(
        &target.host,
        target.user.as_deref(),
        target.remote_port,
        target.remote_data_dir.as_deref(),
    );
    let managed_remote_ctx_bin = target.runtime.managed_ctx_bin.clone();
    let host = target.host;
    let user = target.user;
    let remote_port = target.remote_port;
    let remote_data_dir = target.remote_data_dir;
    let channel_for_update = channel.clone();
    let remote_platform = probe_remote_linux_platform(&host, user.as_deref())?;
    let daemon_base_url = state
        .info_for_scope(scope)
        .base_url
        .ok_or_else(|| anyhow!("current SSH connection is missing a base_url"))?;
    let daemon_auth_token = state
        .info_for_scope(scope)
        .token
        .ok_or_else(|| anyhow!("current SSH connection is missing an auth token"))?;
    let release_base_url = bootstrap_download_base_url();
    let recorded_active_ctx_bin = target
        .runtime
        .active_ctx_bin
        .filter(|value| !value.trim().is_empty());

    let recorded_active_exists = recorded_active_ctx_bin
        .as_deref()
        .map(|active_ctx_bin| {
            remote_ctx_bin_exists_over_ssh(&host, user.as_deref(), active_ctx_bin)
        })
        .transpose()?
        .unwrap_or(false);
    let managed_exists = if recorded_active_ctx_bin.as_deref() == Some(&managed_remote_ctx_bin) {
        recorded_active_exists
    } else {
        remote_ctx_bin_exists_over_ssh(&host, user.as_deref(), &managed_remote_ctx_bin)?
    };
    let decision = resolve_remote_update_target_ctx_bin(
        recorded_active_ctx_bin,
        &managed_remote_ctx_bin,
        recorded_active_exists,
        managed_exists,
    );

    let result: Result<DesktopRemoteDaemonUpdateResp> = (|| {
        if decision.install_managed {
            install_remote_daemon_over_ssh(
                app,
                &host,
                user.as_deref(),
                remote_platform,
                &managed_remote_ctx_bin,
                &channel,
            )
            .map_err(|install_err| install_err.context(REMOTE_BOOTSTRAP_CAPABILITY_MSG))?;
        }
        let active_ctx_bin = decision.ctx_bin;
        run_remote_daemon_self_update(
            app,
            &host,
            user.as_deref(),
            remote_port,
            remote_data_dir.as_deref(),
            &active_ctx_bin,
            remote_platform.arch,
            &channel_for_update,
            &daemon_base_url,
            &daemon_auth_token,
            &release_base_url,
        )?;
        let auth =
            read_remote_daemon_auth_with_retry(&host, user.as_deref(), remote_data_dir.as_deref())?;
        state
            .update_ssh_auth_and_runtime_for_matching_target(
                &host,
                user.as_deref(),
                remote_port,
                remote_data_dir.as_deref(),
                auth.token,
                SshRuntimeMetadata {
                    managed_ctx_bin: managed_remote_ctx_bin.clone(),
                    active_ctx_bin: Some(active_ctx_bin),
                    ssh_password_once: None,
                    admin_password_once: None,
                },
            )
            .map_err(anyhow::Error::msg)?;
        let _ = state.clear_ssh_remote_update_state_for_matching_target(
            &host,
            user.as_deref(),
            remote_port,
            remote_data_dir.as_deref(),
        );
        Ok(DesktopRemoteDaemonUpdateResp {
            updated: true,
            message: format!("Remote daemon updated on channel `{channel}` and restarted."),
        })
    })();

    if let Err(err) = result.as_ref() {
        let _ = state.set_ssh_remote_update_state_for_matching_target(
            &host,
            user.as_deref(),
            remote_port,
            remote_data_dir.as_deref(),
            DesktopRemoteDaemonUpdateState::Failed,
            Some(err.to_string()),
        );
    }
    result
}
