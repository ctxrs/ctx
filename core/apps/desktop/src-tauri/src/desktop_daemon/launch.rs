use super::*;

use super::diagnostics::daemon_stderr_snippet;
use super::path_env::{resolve_daemon_path_env, resolve_local_daemon_path_env};
use super::resources::{
    desktop_bundle_dir, dev_web_dist, resolve_daemon_bin, resolve_optional_bin,
};
use super::systemd::{
    should_use_systemd_scope, stop_systemd_scope, systemd_scope_for_local_daemon_url,
};

const LOCAL_DAEMON_SHUTDOWN_TOKEN_ENV: &str = "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN";

fn avf_guest_gateway_bind(local_port: u16) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let bind_addr = format!("{AVF_GUEST_GATEWAY_HOST}:{local_port}");
    match std::net::TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            drop(listener);
            Some(bind_addr)
        }
        Err(_) => None,
    }
}

pub(super) fn daemon_env_unset_args() -> Vec<std::ffi::OsString> {
    let mut args = Vec::with_capacity(DAEMON_AUTOMATION_ENV_BLOCKLIST.len() * 2);
    for key in DAEMON_AUTOMATION_ENV_BLOCKLIST {
        args.push("-u".into());
        args.push((*key).into());
    }
    args
}

pub(super) fn strip_automation_env(cmd: &mut Command) {
    for key in DAEMON_AUTOMATION_ENV_BLOCKLIST {
        cmd.env_remove(key);
    }
}

pub(super) fn spawn_daemon(
    app: &tauri::AppHandle,
    data_dir: &Path,
    wait_for_health: bool,
) -> Result<(String, Child, bool, String)> {
    let prefer_systemd_scope = should_use_systemd_scope();
    if prefer_systemd_scope {
        match spawn_daemon_with_mode(app, data_dir, true, wait_for_health) {
            Ok(v) => return Ok(v),
            Err(e) => {
                // Fall back to a direct child process when systemd user services are unavailable
                // (common in headless/dev environments).
                return spawn_daemon_with_mode(app, data_dir, false, wait_for_health)
                    .with_context(|| format!("spawning ctx daemon via systemd-run failed: {e:#}"));
            }
        }
    }
    spawn_daemon_with_mode(app, data_dir, false, wait_for_health)
}

fn spawn_daemon_with_mode(
    app: &tauri::AppHandle,
    data_dir: &Path,
    use_systemd_scope: bool,
    wait_for_health: bool,
) -> Result<(String, Child, bool, String)> {
    let ctx_bin = resolve_daemon_bin(app)?;
    let avf_linux_helper_bin = resolve_optional_bin(app, "ctx-avf-linux-helper");
    let mcp_bin = resolve_optional_bin(app, "ctx-mcp");
    let daemon_path_env = resolve_daemon_path_env();

    let web_dist = app
        .path()
        .resource_dir()
        .ok()
        .and_then(|p| {
            let candidates = [
                p.join("web").join("dist"),
                p.join("web-dist"),
                p.join("dist"),
            ];
            candidates.into_iter().find(|c| c.exists())
        })
        .or_else(dev_web_dist);
    let bundle_dir = desktop_bundle_dir(app);
    let build_identity_path = bundle_dir
        .as_ref()
        .map(|bundle| bundle.join("artifact_identity.json"))
        .filter(|path| path.exists());
    let resolved_path_env = resolve_local_daemon_path_env();

    let seed_codex_auth = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex").join("auth.json").exists())
        .unwrap_or(false);

    let local_port = pick_unused_local_port()?;
    let base_url = format!("http://127.0.0.1:{local_port}");
    let systemd_unit = format!("ctx-daemon-{local_port}");
    let local_shutdown_token = uuid::Uuid::new_v4().to_string();

    if use_systemd_scope {
        stop_systemd_scope("ctx-daemon");
        stop_systemd_scope(&systemd_unit);
    }
    let mut cmd = if use_systemd_scope {
        let mut cmd = Command::new("systemd-run");
        cmd.arg("--user")
            .arg("--scope")
            .arg("--unit")
            .arg(&systemd_unit)
            .arg("--same-dir");
        cmd.arg("--setenv").arg(format!(
            "{LOCAL_DAEMON_SHUTDOWN_TOKEN_ENV}={local_shutdown_token}"
        ));
        if let Some(path_env) = resolved_path_env.as_ref() {
            cmd.arg("--setenv").arg(format!("PATH={path_env}"));
        }
        if let Some(dist) = web_dist.as_ref() {
            cmd.arg("--setenv")
                .arg(format!("CTX_WEB_DIST={}", dist.to_string_lossy()));
        }
        if let Some(mcp) = mcp_bin.as_ref() {
            cmd.arg("--setenv")
                .arg(format!("CTX_MCP_COMMAND={}", mcp.to_string_lossy()));
        }
        if let Some(helper) = avf_linux_helper_bin.as_ref() {
            cmd.arg("--setenv").arg(format!(
                "{AVF_LINUX_HELPER_PATH_ENV}={}",
                helper.to_string_lossy()
            ));
        }
        if let Some(bundle) = bundle_dir.as_ref() {
            cmd.arg("--setenv")
                .arg(format!("CTX_BUNDLE_DIR={}", bundle.to_string_lossy()));
        }
        if let Some(identity_path) = build_identity_path.as_ref() {
            cmd.arg("--setenv").arg(format!(
                "{DESKTOP_BUILD_IDENTITY_PATH_ENV}={}",
                identity_path.to_string_lossy()
            ));
        }
        if local_linux_sandbox_runtime_ready(data_dir) {
            cmd.arg("--setenv")
                .arg(format!(
                    "CTX_HARNESS_SANDBOX_CLI_PATH={ROOTFUL_WRAPPER_PATH}"
                ))
                .arg("--setenv")
                .arg(format!("CONTAINERD_ADDRESS={MANAGED_CONTAINERD_ADDRESS}"))
                .arg("--setenv")
                .arg(format!(
                    "CONTAINERD_NAMESPACE={MANAGED_CONTAINERD_NAMESPACE}"
                ));
        }
        if seed_codex_auth {
            cmd.arg("--setenv").arg("CTX_SEED_CODEX_AUTH_FROM_HOST=1");
        }
        if let Ok(appimage) = std::env::var("APPIMAGE") {
            cmd.arg("--setenv")
                .arg(format!("CTX_APPIMAGE_PATH={appimage}"));
        }
        if let Some(path_value) = daemon_path_env.as_deref() {
            cmd.arg("--setenv")
                .arg(format!("PATH={}", path_value.to_string_lossy()));
        }
        for key in DAEMON_ENV_PASSTHROUGH {
            if let Ok(value) = std::env::var(key) {
                cmd.arg("--setenv").arg(format!("{key}={value}"));
            }
        }
        cmd.arg("/usr/bin/env");
        cmd.args(daemon_env_unset_args());
        cmd.arg(&ctx_bin);
        cmd
    } else {
        let mut cmd = Command::new(&ctx_bin);
        strip_automation_env(&mut cmd);
        cmd.env(LOCAL_DAEMON_SHUTDOWN_TOKEN_ENV, &local_shutdown_token);
        if let Some(path_env) = resolved_path_env.as_ref() {
            cmd.env("PATH", path_env);
        }
        if let Some(dist) = web_dist.as_ref() {
            cmd.env("CTX_WEB_DIST", dist.to_string_lossy().to_string());
        }
        if let Some(mcp) = mcp_bin.as_ref() {
            cmd.env("CTX_MCP_COMMAND", mcp.to_string_lossy().to_string());
        }
        if let Some(helper) = avf_linux_helper_bin.as_ref() {
            cmd.env(
                AVF_LINUX_HELPER_PATH_ENV,
                helper.to_string_lossy().to_string(),
            );
        }
        if let Some(bundle) = bundle_dir.as_ref() {
            cmd.env("CTX_BUNDLE_DIR", bundle.to_string_lossy().to_string());
        }
        if let Some(identity_path) = build_identity_path.as_ref() {
            cmd.env(
                DESKTOP_BUILD_IDENTITY_PATH_ENV,
                identity_path.to_string_lossy().to_string(),
            );
        }
        configure_local_linux_sandbox_daemon_env(&mut cmd, data_dir);
        if seed_codex_auth {
            cmd.env("CTX_SEED_CODEX_AUTH_FROM_HOST", "1");
        }
        if let Ok(appimage) = std::env::var("APPIMAGE") {
            cmd.env("CTX_APPIMAGE_PATH", appimage.clone());
        }
        if let Some(path_value) = daemon_path_env.as_deref() {
            cmd.env("PATH", path_value);
        }
        for key in DAEMON_ENV_PASSTHROUGH {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        cmd
    };

    cmd.arg("serve")
        .arg("--bind")
        .arg(format!("127.0.0.1:{local_port}"));
    if avf_linux_helper_bin.is_some() {
        if let Some(avf_bind) = avf_guest_gateway_bind(local_port) {
            cmd.arg("--bind").arg(avf_bind);
        }
    }
    cmd.arg("--data-dir")
        .arg(data_dir.to_string_lossy().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null());

    let mut stderr_path: Option<PathBuf> = None;
    if use_systemd_scope {
        cmd.stderr(Stdio::inherit());
    } else {
        let log_dir = data_dir.join("logs");
        if let Err(err) = ctx_fs::permissions::ensure_private_dir_sync(&log_dir) {
            eprintln!(
                "failed to create daemon log dir {}: {err}",
                log_dir.display()
            );
            cmd.stderr(Stdio::inherit());
        } else {
            let path = log_dir.join("desktop-daemon-stderr.log");
            match ctx_fs::permissions::open_private_append_sync(&path) {
                Ok(file) => {
                    stderr_path = Some(path);
                    cmd.stderr(file);
                }
                Err(err) => {
                    eprintln!("failed to open daemon stderr log: {err}");
                    cmd.stderr(Stdio::inherit());
                }
            }
        }
    }

    let mut child = cmd.spawn().context("spawning ctx daemon")?;
    if wait_for_health {
        let auth = read_daemon_auth_with_retry(data_dir).context("reading spawned daemon auth")?;
        if let Err(err) =
            probe_local_daemon_health_with_retry_auth(&base_url, Some(auth.token.as_str()))
        {
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = daemon_stderr_snippet(stderr_path.as_deref());
                let mut msg = format!("{err:#}; daemon exited ({status})");
                if !stderr.is_empty() {
                    msg.push_str(&format!("; stderr: {stderr}"));
                }
                return Err(anyhow!(msg));
            }
            let stderr = daemon_stderr_snippet(stderr_path.as_deref());
            if !stderr.is_empty() {
                return Err(anyhow!("{err:#}; stderr: {stderr}"));
            }
            return Err(err).context("waiting for daemon health");
        }
    } else {
        let base_url = base_url.clone();
        let stderr_path = stderr_path.clone();
        let data_dir = data_dir.to_path_buf();
        std::thread::spawn(move || {
            let auth = match read_daemon_auth_with_retry(&data_dir) {
                Ok(auth) => auth,
                Err(err) => {
                    let stderr = daemon_stderr_snippet(stderr_path.as_deref());
                    if stderr.is_empty() {
                        eprintln!("ctx daemon auth read failed after spawn: {err:#}");
                    } else {
                        eprintln!(
                            "ctx daemon auth read failed after spawn: {err:#}; stderr: {stderr}"
                        );
                    }
                    return;
                }
            };
            if let Err(err) =
                probe_local_daemon_health_with_retry_auth(&base_url, Some(auth.token.as_str()))
            {
                let stderr = daemon_stderr_snippet(stderr_path.as_deref());
                if stderr.is_empty() {
                    eprintln!("ctx daemon health check failed after spawn: {err:#}");
                } else {
                    eprintln!(
                        "ctx daemon health check failed after spawn: {err:#}; stderr: {stderr}"
                    );
                }
            }
        });
    }
    Ok((base_url, child, use_systemd_scope, local_shutdown_token))
}

#[allow(dead_code)]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

pub(in super::super) fn try_kill_child(mut child: Child) -> Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

pub(in super::super) struct SpawnedLocalDaemonReady {
    pub(in super::super) url: String,
    pub(in super::super) token: String,
    pub(in super::super) local_shutdown_token: String,
    pub(in super::super) child: Child,
    pub(in super::super) systemd_scope: bool,
}

struct PendingSpawnedLocalDaemon {
    url: String,
    child: Option<Child>,
    systemd_scope: bool,
}

impl PendingSpawnedLocalDaemon {
    fn new(url: String, child: Child, systemd_scope: bool) -> Self {
        Self {
            url,
            child: Some(child),
            systemd_scope,
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn disarm(mut self) -> Result<(String, Child, bool)> {
        let child = self
            .child
            .take()
            .ok_or_else(|| anyhow!("spawned daemon child missing"))?;
        let url = std::mem::take(&mut self.url);
        Ok((url, child, self.systemd_scope))
    }
}

impl Drop for PendingSpawnedLocalDaemon {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            cleanup_rejected_spawned_local_daemon(child, self.systemd_scope, &self.url);
        }
    }
}

pub(in super::super) fn spawn_and_validate_local_daemon(
    app: &tauri::AppHandle,
    data_dir: &Path,
    desktop_identity: &DesktopBuildIdentity,
) -> Result<SpawnedLocalDaemonReady> {
    let (url, child, systemd_scope, local_shutdown_token) = spawn_daemon(app, data_dir, true)?;
    let pending = PendingSpawnedLocalDaemon::new(url, child, systemd_scope);
    let auth = read_daemon_auth_with_retry(data_dir)?;
    let health = daemon_health_with_auth(pending.url(), Some(auth.token.as_str()))
        .context("requesting authenticated /api/health for spawned local daemon compatibility")?;
    let compatible = local_daemon_health_matches_expected(&health, data_dir, desktop_identity);
    if !compatible {
        anyhow::bail!(
            "{}",
            spawned_local_daemon_incompatibility_message(
                pending.url(),
                data_dir,
                desktop_identity,
                &health
            )
        );
    }
    let (url, child, systemd_scope) = pending.disarm()?;
    Ok(SpawnedLocalDaemonReady {
        url,
        token: auth.token,
        local_shutdown_token,
        child,
        systemd_scope,
    })
}

fn cleanup_rejected_spawned_local_daemon(child: Child, systemd_scope: bool, base_url: &str) {
    let _ = try_kill_child(child);
    if systemd_scope {
        if let Some(unit) = systemd_scope_for_local_daemon_url(base_url) {
            stop_systemd_scope(&unit);
        }
    }
}
