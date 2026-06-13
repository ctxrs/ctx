use super::*;

fn remote_prewarm_dedupe_key(
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
) -> String {
    let host = host.trim().to_ascii_lowercase();
    let user = user.unwrap_or("").trim().to_string();
    let data_dir = remote_data_dir
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("~/.ctx")
        .to_string();
    format!("{user}@{host}:{remote_port}:{data_dir}")
}

fn remote_prewarm_inflight() -> &'static std::sync::Mutex<HashSet<String>> {
    static REMOTE_PREWARM_INFLIGHT: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    REMOTE_PREWARM_INFLIGHT.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

#[tauri::command]
pub(crate) fn desktop_list_ssh_hosts() -> Result<Vec<DesktopSshHost>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for path in ssh_config_paths() {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        for entry in parse_ssh_config(&text) {
            if seen.insert(entry.host.clone()) {
                out.push(entry);
            }
        }
    }
    Ok(out)
}

#[tauri::command]
pub(crate) async fn desktop_list_ssh_paths(
    req: DesktopSshPathReq,
) -> Result<Vec<DesktopSshPathEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let host = req.host.trim().to_string();
        if host.is_empty() {
            return Err("host is required".to_string());
        }
        let target = ssh_target(&host, req.user.as_deref());
        let raw = req.path.unwrap_or_default();
        let (parent, prefix) = split_remote_path(&raw);
        let cmd = format!("ls -a1 -p -- {}", remote_path_expr(&parent));
        let remote_cmd = format!("sh -lc {}", shell_escape(&cmd));
        let output = new_ssh_command()
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=8")
            .arg(target)
            .arg(remote_cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to spawn ssh: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                return Err("ssh failed to list paths".to_string());
            }
            return Err(format!("ssh failed: {stderr}"));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();
        for line in stdout.lines() {
            let name = line.trim();
            if name.is_empty() || !name.ends_with('/') {
                continue;
            }
            let name = name.trim_end_matches('/');
            if name == "." {
                continue;
            }
            if !prefix.is_empty() && !name.starts_with(&prefix) {
                continue;
            }
            let full_path = join_remote_path(&parent, name);
            entries.push(DesktopSshPathEntry {
                name: name.to_string(),
                path: full_path,
            });
        }
        Ok(entries)
    })
    .await
    .map_err(|e| format!("ssh list failed: {e}"))?
}

#[tauri::command]
pub(crate) async fn desktop_test_ssh(req: DesktopSshTestReq) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let host = req.host.trim().to_string();
        if host.is_empty() {
            return Err("host is required".to_string());
        }
        let user = normalize_optional_text(req.user.as_deref());
        let password_once = normalize_optional_text(req.password_once.as_deref());
        probe_remote_linux_platform_with_optional_password(
            &host,
            user.as_deref(),
            password_once.as_deref(),
        )
        .map(|_| ())
        .map_err(to_err)
    })
    .await
    .map_err(|e| format!("ssh check failed: {e}"))?
}

#[tauri::command]
pub(crate) async fn desktop_get_git_branch(
    req: DesktopGitBranchReq,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let raw = req.path.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let mut path = expand_tilde(raw).unwrap_or_else(|| PathBuf::from(raw));
        if !path.is_absolute() {
            return Ok(None);
        }
        path = normalize_path(&path);
        let output = Command::new("git")
            .arg("-C")
            .arg(&path)
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("HEAD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(output) = output else {
            return Ok(None);
        };
        if !output.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() || value == "HEAD" {
            return Ok(None);
        }
        Ok(Some(value))
    })
    .await
    .map_err(|e| format!("git branch lookup failed: {e}"))?
}

#[tauri::command]
pub(crate) async fn desktop_kickoff_remote_prewarm(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopRemotePrewarmReq,
) -> Result<(), String> {
    schedule_remote_prewarm_request(
        app,
        window.label().to_string(),
        req.host,
        req.user,
        req.remote_port.unwrap_or(4399),
        req.remote_data_dir,
    )
}

pub(super) fn schedule_remote_prewarm_request(
    app: tauri::AppHandle,
    scope: String,
    host: String,
    user: Option<String>,
    remote_port: u16,
    remote_data_dir: Option<String>,
) -> Result<(), String> {
    let host = host.trim().to_string();
    if host.is_empty() {
        return Err("host is required".to_string());
    }
    let user = normalize_optional_text(user.as_deref());
    let remote_data_dir = normalize_optional_text(remote_data_dir.as_deref());
    let key = remote_prewarm_dedupe_key(
        &host,
        user.as_deref(),
        remote_port,
        remote_data_dir.as_deref(),
    );
    let should_spawn = {
        let inflight = remote_prewarm_inflight();
        let mut guard = inflight
            .lock()
            .map_err(|_| "remote prewarm lock poisoned".to_string())?;
        guard.insert(key.clone())
    };
    if !should_spawn {
        return Ok(());
    }

    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || {
            request_remote_startup_prewarm(
                &app,
                &scope,
                &host,
                user.as_deref(),
                remote_port,
                remote_data_dir.as_deref(),
            )
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("remote prewarm failed: {err:#}"),
            Err(join_err) => eprintln!("remote prewarm task join failed: {join_err:#}"),
        }
        if let Ok(mut guard) = remote_prewarm_inflight().lock() {
            guard.remove(&key);
        }
    });
    Ok(())
}

fn request_remote_startup_prewarm(
    app: &tauri::AppHandle,
    scope: &str,
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
) -> Result<()> {
    let state = app.state::<ConnectionManager>();
    let manager: &ConnectionManager = state.inner();
    let active = manager.ssh_target_for_scope(scope)?;
    let requested_key = remote_prewarm_dedupe_key(host, user, remote_port, remote_data_dir);
    let active_key = remote_prewarm_dedupe_key(
        &active.host,
        active.user.as_deref(),
        active.remote_port,
        active.remote_data_dir.as_deref(),
    );
    if requested_key != active_key {
        anyhow::bail!("current SSH connection target does not match remote prewarm request");
    }
    let response =
        manager.daemon_request_for_scope(scope, build_remote_startup_prewarm_request())?;
    if (200..300).contains(&response.status) {
        let stage_response =
            manager.daemon_request_for_scope(scope, build_remote_linux_sandbox_stage_request())?;
        if (200..300).contains(&stage_response.status) {
            return Ok(());
        }
        anyhow::bail!(
            "remote linux sandbox stage request failed with status {}",
            stage_response.status
        );
    }
    anyhow::bail!(
        "remote startup prewarm request failed with status {}",
        response.status
    );
}

pub(super) fn build_remote_startup_prewarm_request() -> DesktopDaemonRequest {
    DesktopDaemonRequest {
        method: "POST".to_string(),
        path: "/api/execution/launch/start".to_string(),
        body: Some(
            serde_json::json!({
                "kind": "startup_prewarm",
                "prewarm_scope": "all",
            })
            .to_string(),
        ),
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
    }
}

pub(super) fn build_remote_linux_sandbox_stage_request() -> DesktopDaemonRequest {
    DesktopDaemonRequest {
        method: "POST".to_string(),
        path: "/api/execution/linux_sandbox_runtime/stage".to_string(),
        body: None,
        headers: vec![("Content-Type".to_string(), "application/json".to_string())],
    }
}
