use super::*;
pub(crate) use ctx_desktop_ipc::{
    DesktopLinuxSandboxEnsureResp, DesktopLocalLinuxSandboxEnsureReq,
    DesktopRemoteLinuxSandboxEnsureReq,
};

const LOCAL_ADMIN_PASSWORD_REQUIRED_SENTINEL: &str = "CTX_LOCAL_ADMIN_PASSWORD_REQUIRED";
const REMOTE_ADMIN_PASSWORD_REQUIRED_SENTINEL: &str = "CTX_REMOTE_ADMIN_PASSWORD_REQUIRED";
pub(crate) const ROOTFUL_WRAPPER_PATH: &str = "/usr/local/bin/ctx-rootful-nerdctl";
pub(crate) const MANAGED_CONTAINERD_ADDRESS: &str = "/run/containerd/containerd.sock";
pub(crate) const MANAGED_CONTAINERD_NAMESPACE: &str = "default";
const LOCAL_PREFETCH_DELAY_MS: u64 = 500;
const PREFETCH_WAIT_POLL_MS: u64 = 250;
const PREFETCH_WAIT_TIMEOUT_MS: u64 = 180_000;

const BOOTSTRAP_SCRIPT: &str =
    include_str!("../../../../crates/ctx-linux-sandbox-runtime/src/linux_sandbox_bootstrap.sh");

#[path = "linux_sandbox/local.rs"]
mod local;

#[cfg(test)]
use local::*;
pub(crate) use local::{
    configure_local_linux_sandbox_daemon_env, desktop_ensure_local_linux_sandbox_ready,
    local_linux_sandbox_runtime_ready, remote_linux_sandbox_daemon_env_prefix,
    schedule_local_linux_sandbox_prefetch,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct LinuxSandboxBootstrapStatus {
    state: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LinuxSandboxPrepareResponse {
    ready: bool,
    needs_password: bool,
    #[serde(default)]
    message: String,
}

fn active_remote_passwords(target: &SshConnectionTarget) -> (Option<String>, Option<String>) {
    (
        target.runtime.ssh_password_once.clone(),
        target.runtime.admin_password_once.clone(),
    )
}

fn build_remote_stage_request() -> DesktopDaemonRequest {
    DesktopDaemonRequest {
        method: "POST".to_string(),
        path: "/api/execution/linux_sandbox_runtime/stage".to_string(),
        body: None,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
    }
}

fn parse_remote_daemon_json<T: serde::de::DeserializeOwned>(
    response: DesktopHttpResponse,
    expected_path: &str,
) -> Result<T> {
    if (200..300).contains(&response.status) {
        return serde_json::from_str::<T>(&response.body)
            .with_context(|| format!("parsing {expected_path} response"));
    }
    let body = response.body.trim();
    if body.is_empty() {
        anyhow::bail!("{expected_path} failed with status {}", response.status);
    }
    anyhow::bail!(
        "{expected_path} failed with status {}: {body}",
        response.status
    );
}

fn remote_daemon_missing_linux_sandbox_endpoint(response: &DesktopHttpResponse) -> bool {
    response.status == 404
}

fn select_remote_admin_password<'a>(
    requested_admin_password: Option<&'a str>,
    cached_admin_password: Option<&'a str>,
    ssh_password_once: Option<&'a str>,
) -> Option<&'a str> {
    // EXCEPTION: Product decision for remote Linux bootstrap is to reuse the
    // session's one-time SSH password for the first sudo attempt when no
    // distinct admin password has been provided, to minimize prompts.
    requested_admin_password
        .or(cached_admin_password)
        .or(ssh_password_once)
}

fn runtime_with_persisted_remote_admin_password(
    mut runtime: SshRuntimeMetadata,
    password: &str,
) -> SshRuntimeMetadata {
    runtime.admin_password_once = Some(password.to_string());
    runtime
}

fn remote_stage_status(
    manager: &ConnectionManager,
    scope: &str,
) -> Result<LinuxSandboxBootstrapStatus> {
    remote_stage_status_response(
        manager.daemon_request_for_scope(scope, build_remote_stage_request())?,
    )
}

fn remote_stage_status_response(
    response: DesktopHttpResponse,
) -> Result<LinuxSandboxBootstrapStatus> {
    if remote_daemon_missing_linux_sandbox_endpoint(&response) {
        anyhow::bail!(
            "remote daemon does not support sandbox preparation yet. Update or install the remote daemon explicitly, then reconnect"
        );
    }
    parse_remote_daemon_json(response, "/api/execution/linux_sandbox_runtime/stage")
}

fn build_remote_prepare_request(sudo_password: Option<&str>) -> DesktopDaemonRequest {
    DesktopDaemonRequest {
        method: "POST".to_string(),
        path: "/api/execution/linux_sandbox_runtime/prepare".to_string(),
        body: Some(
            serde_json::json!({
                "activation_mode": "remote",
                "sudo_password": sudo_password,
            })
            .to_string(),
        ),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
    }
}

fn remote_prepare_status(
    manager: &ConnectionManager,
    scope: &str,
    sudo_password: Option<&str>,
) -> Result<LinuxSandboxPrepareResponse> {
    parse_remote_daemon_json(
        manager.daemon_request_for_scope(scope, build_remote_prepare_request(sudo_password))?,
        "/api/execution/linux_sandbox_runtime/prepare",
    )
}

#[tauri::command]
pub(crate) async fn desktop_ensure_remote_linux_sandbox_ready(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopRemoteLinuxSandboxEnsureReq,
) -> Result<DesktopLinuxSandboxEnsureResp, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<ConnectionManager>();
        let manager: &ConnectionManager = state.inner();
        let scope = window.label().to_string();
        let target = manager.ssh_target_for_scope(&scope).map_err(to_err)?;
        let (ssh_password_once, cached_admin_password) = active_remote_passwords(&target);
        let status = remote_stage_status(manager, &scope).map_err(to_err)?;
        match status.state.as_str() {
            "ready" => Ok(DesktopLinuxSandboxEnsureResp { ready: true }),
            "manual_runtime_required" => Err(if status.message.trim().is_empty() {
                "Preparing sandbox on remote host failed. Managed sandbox setup is currently supported on Ubuntu/Debian only.".to_string()
            } else {
                format!("Preparing sandbox on remote host failed. {}", status.message.trim())
            }),
            "downloaded_not_activated" => {
                let selected_password = select_remote_admin_password(
                    req.admin_password_once.as_deref(),
                    cached_admin_password.as_deref(),
                    ssh_password_once.as_deref(),
                );
                let prepare =
                    remote_prepare_status(manager, &scope, selected_password).map_err(to_err)?;
                if prepare.ready {
                    if let Some(password) = selected_password {
                        let latest_runtime =
                            manager.ssh_target_for_scope(&scope).map_err(to_err)?.runtime;
                        manager
                            .update_ssh_runtime_for_scope(
                                &scope,
                                runtime_with_persisted_remote_admin_password(
                                latest_runtime,
                                password,
                                ),
                            )
                            .map_err(to_err)?;
                    }
                    return Ok(DesktopLinuxSandboxEnsureResp { ready: true });
                }
                if prepare.needs_password {
                    return Err(format!(
                        "{REMOTE_ADMIN_PASSWORD_REQUIRED_SENTINEL}: {}",
                        if prepare.message.trim().is_empty() {
                            "Remote admin password required to prepare sandbox on this host."
                        } else {
                            prepare.message.trim()
                        }
                    ));
                }
                Err(if prepare.message.trim().is_empty() {
                    "Preparing sandbox on remote host failed.".to_string()
                } else {
                    format!("Preparing sandbox on remote host failed. {}", prepare.message.trim())
                })
            }
            other => Err(if status.message.trim().is_empty() {
                format!("Preparing sandbox on remote host failed. bootstrap_state={other}")
            } else {
                format!("Preparing sandbox on remote host failed. {}", status.message.trim())
            }),
        }
    })
    .await
    .map_err(|err| format!("Preparing sandbox on remote host failed. {err}"))?
}

#[cfg(test)]
#[path = "linux_sandbox/tests.rs"]
mod tests;
