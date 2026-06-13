use super::*;

pub(crate) fn local_prefetch_inflight() -> &'static std::sync::Mutex<bool> {
    static INFLIGHT: std::sync::OnceLock<std::sync::Mutex<bool>> = std::sync::OnceLock::new();
    INFLIGHT.get_or_init(|| std::sync::Mutex::new(false))
}

pub(crate) fn linux_sandbox_root(data_dir: &Path) -> PathBuf {
    data_dir.join("linux-sandbox-runtime")
}

pub(crate) fn linux_sandbox_status_path(data_dir: &Path) -> PathBuf {
    linux_sandbox_root(data_dir).join("status.json")
}

pub(crate) fn linux_sandbox_script_path(data_dir: &Path) -> PathBuf {
    linux_sandbox_root(data_dir).join("bootstrap.sh")
}

pub(crate) fn write_local_status_override(
    data_dir: &Path,
    state: &str,
    message: &str,
) -> Result<()> {
    let status_path = linux_sandbox_status_path(data_dir);
    if let Some(parent) = status_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let payload = serde_json::json!({
        "state": state,
        "supported": true,
        "message": message,
        "distro": ""
    });
    std::fs::write(
        &status_path,
        serde_json::to_vec(&payload).context("serializing status")?,
    )
    .with_context(|| format!("writing {}", status_path.display()))?;
    Ok(())
}

pub(crate) fn write_local_bootstrap_script(data_dir: &Path) -> Result<PathBuf> {
    let root = linux_sandbox_root(data_dir);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating linux sandbox bootstrap dir {}", root.display()))?;
    let script_path = linux_sandbox_script_path(data_dir);
    let should_write = match std::fs::read_to_string(&script_path) {
        Ok(existing) => existing != BOOTSTRAP_SCRIPT,
        Err(_) => true,
    };
    if should_write {
        std::fs::write(&script_path, BOOTSTRAP_SCRIPT)
            .with_context(|| format!("writing {}", script_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("setting mode on {}", script_path.display()))?;
        }
    }
    Ok(script_path)
}

pub(crate) fn parse_status_json(raw: &str) -> Result<LinuxSandboxBootstrapStatus> {
    serde_json::from_str::<LinuxSandboxBootstrapStatus>(raw.trim())
        .context("parsing linux sandbox bootstrap status")
}

pub(crate) fn read_local_status(data_dir: &Path) -> Result<LinuxSandboxBootstrapStatus> {
    let status_path = linux_sandbox_status_path(data_dir);
    if !status_path.exists() {
        return Ok(LinuxSandboxBootstrapStatus {
            state: "download_pending".to_string(),
            message: String::new(),
        });
    }
    let raw = std::fs::read_to_string(&status_path)
        .with_context(|| format!("reading {}", status_path.display()))?;
    parse_status_json(&raw)
}

pub(crate) fn read_local_status_via_bootstrap(
    data_dir: &Path,
) -> Result<LinuxSandboxBootstrapStatus> {
    let script_path = write_local_bootstrap_script(data_dir)?;
    let output = Command::new(&script_path)
        .arg("status")
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running local linux sandbox bootstrap status")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        return parse_status_json(&stdout);
    }
    if let Ok(status) = parse_status_json(&stdout) {
        return Ok(status);
    }
    let detail = format_output_detail(&output);
    anyhow::bail!("local linux sandbox bootstrap status failed: {detail}");
}

pub(crate) fn format_output_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) fn local_stage_spawn(app: tauri::AppHandle) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let should_spawn = {
        let mut guard = match local_prefetch_inflight().lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if *guard {
            false
        } else {
            *guard = true;
            true
        }
    };
    if !should_spawn {
        return;
    }
    let data_dir = match daemon_data_dir(&app) {
        Ok(data_dir) => data_dir,
        Err(err) => {
            eprintln!("local linux sandbox bootstrap stage failed: {err:#}");
            if let Ok(mut guard) = local_prefetch_inflight().lock() {
                *guard = false;
            }
            return;
        }
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(LOCAL_PREFETCH_DELAY_MS));
        let result = (|| -> Result<()> {
            let script_path = write_local_bootstrap_script(&data_dir)?;
            let output = Command::new(&script_path)
                .arg("stage")
                .arg("--data-dir")
                .arg(&data_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .context("running local linux sandbox bootstrap stage")?;
            if !output.status.success() {
                let detail = format_output_detail(&output);
                anyhow::bail!("local linux sandbox bootstrap stage failed: {detail}");
            }
            Ok(())
        })();
        if let Err(err) = result {
            let detail = err.to_string();
            let _ = write_local_status_override(&data_dir, "failed", &detail);
            eprintln!("local linux sandbox bootstrap stage failed: {err:#}");
        }
        if let Ok(mut guard) = local_prefetch_inflight().lock() {
            *guard = false;
        }
    });
}

pub(crate) fn schedule_local_linux_sandbox_prefetch(app: tauri::AppHandle) {
    local_stage_spawn(app);
}

pub(crate) fn local_linux_sandbox_runtime_ready(data_dir: &Path) -> bool {
    matches!(
        read_local_status_via_bootstrap(data_dir),
        Ok(status) if status.state == "ready"
    )
}

pub(crate) fn configure_local_linux_sandbox_daemon_env(cmd: &mut Command, data_dir: &Path) {
    if !cfg!(target_os = "linux") {
        return;
    }
    if !local_linux_sandbox_runtime_ready(data_dir) {
        return;
    }
    cmd.env("CTX_HARNESS_SANDBOX_CLI_PATH", ROOTFUL_WRAPPER_PATH);
    cmd.env("CONTAINERD_ADDRESS", MANAGED_CONTAINERD_ADDRESS);
    cmd.env("CONTAINERD_NAMESPACE", MANAGED_CONTAINERD_NAMESPACE);
}

pub(crate) fn remote_linux_sandbox_daemon_env_prefix(_data_dir: &str) -> String {
    format!(
        "if [ -x {wrapper} ] && [ -S {address} ] && {wrapper} info >/dev/null 2>&1; then export CTX_HARNESS_SANDBOX_CLI_PATH={wrapper} CONTAINERD_ADDRESS={address} CONTAINERD_NAMESPACE={namespace}; fi;",
        wrapper = shell_escape(ROOTFUL_WRAPPER_PATH),
        address = shell_escape(MANAGED_CONTAINERD_ADDRESS),
        namespace = shell_escape(MANAGED_CONTAINERD_NAMESPACE),
    )
}

pub(crate) fn wait_for_local_stage_completion(
    data_dir: &Path,
) -> Result<LinuxSandboxBootstrapStatus> {
    let started = Instant::now();
    loop {
        let status = read_local_status(data_dir)?;
        match status.state.as_str() {
            "download_pending" | "downloading" => {
                if started.elapsed() > Duration::from_millis(PREFETCH_WAIT_TIMEOUT_MS) {
                    anyhow::bail!("timed out waiting for Linux sandbox downloads to finish");
                }
                std::thread::sleep(Duration::from_millis(PREFETCH_WAIT_POLL_MS));
            }
            _ => return Ok(status),
        }
    }
}

pub(crate) fn is_posix_safe_username(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn current_local_user() -> Result<String> {
    let output = std::process::Command::new("id")
        .arg("-un")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running id -un for Linux sandbox activation")?;
    if !output.status.success() {
        let detail = format_output_detail(&output);
        if detail.is_empty() {
            anyhow::bail!("id -un failed while preparing Linux sandbox runtime");
        }
        anyhow::bail!("id -un failed while preparing Linux sandbox runtime: {detail}");
    }
    let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("id -un returned an empty username");
    }
    if !is_posix_safe_username(&trimmed) {
        anyhow::bail!("id -un returned a non-POSIX-safe username");
    }
    Ok(trimmed)
}

pub(crate) enum LocalActivationOutcome {
    Ready,
    NeedsPassword,
}

pub(crate) fn activation_args(data_dir: &Path, allow_user: &str) -> Vec<String> {
    vec![
        "bash".to_string(),
        "-s".to_string(),
        "--".to_string(),
        "activate".to_string(),
        "--data-dir".to_string(),
        data_dir.to_string_lossy().to_string(),
        "--allow-user".to_string(),
        allow_user.to_string(),
    ]
}

pub(crate) fn run_command_with_stdin(
    mut command: Command,
    stdin: &[u8],
    context: &str,
) -> Result<std::process::Output> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().with_context(|| context.to_string())?;
    if let Some(mut child_stdin) = child.stdin.take() {
        use std::io::Write as _;
        child_stdin
            .write_all(stdin)
            .with_context(|| format!("{context}: writing stdin payload"))?;
    }
    child
        .wait_with_output()
        .with_context(|| format!("{context}: waiting for command output"))
}

pub(crate) fn local_sudo_needs_password(output: &std::process::Output) -> bool {
    let detail = format_output_detail(output).to_ascii_lowercase();
    detail.contains("a password is required")
        || detail.contains("password is required")
        || detail.contains("sorry, try again")
        || detail.contains("incorrect password")
        || (detail.contains("sudo:")
            && (detail.contains("no tty")
                || detail.contains("askpass")
                || detail.contains("password")))
}

pub(crate) fn run_local_activation(
    data_dir: &Path,
    allow_user: &str,
    admin_password_once: Option<&str>,
) -> Result<LocalActivationOutcome> {
    let args = activation_args(data_dir, allow_user);
    let output = run_command_with_stdin(
        {
            let mut command = Command::new("sudo");
            command.arg("--non-interactive").args(&args);
            command
        },
        BOOTSTRAP_SCRIPT.as_bytes(),
        "running local Linux sandbox activation via sudo",
    )?;
    if output.status.success() {
        return Ok(LocalActivationOutcome::Ready);
    }
    if local_sudo_needs_password(&output) && admin_password_once.is_none() {
        return Ok(LocalActivationOutcome::NeedsPassword);
    }
    if let Some(password) = admin_password_once {
        let mut stdin = Vec::with_capacity(password.len() + BOOTSTRAP_SCRIPT.len() + 1);
        stdin.extend_from_slice(password.as_bytes());
        stdin.push(b'\n');
        stdin.extend_from_slice(BOOTSTRAP_SCRIPT.as_bytes());
        let output = run_command_with_stdin(
            {
                let mut command = Command::new("sudo");
                command.arg("-S").arg("-p").arg("").args(&args);
                command
            },
            &stdin,
            "running local Linux sandbox activation via sudo password",
        )?;
        if output.status.success() {
            return Ok(LocalActivationOutcome::Ready);
        }
        if local_sudo_needs_password(&output) {
            return Ok(LocalActivationOutcome::NeedsPassword);
        }
        let detail = format_output_detail(&output);
        anyhow::bail!("Preparing Linux sandbox runtime failed. {detail}");
    }
    let detail = format_output_detail(&output);
    anyhow::bail!("Preparing Linux sandbox runtime failed. {detail}");
}

pub(crate) fn local_linux_platform_is_supported() -> bool {
    cfg!(target_os = "linux")
}

#[tauri::command]
pub(crate) async fn desktop_ensure_local_linux_sandbox_ready(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopLocalLinuxSandboxEnsureReq,
) -> Result<DesktopLinuxSandboxEnsureResp, String> {
    if !local_linux_platform_is_supported() {
        return Ok(DesktopLinuxSandboxEnsureResp { ready: true });
    }
    let scope = window.label().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let data_dir = daemon_data_dir(&app).map_err(to_err)?;
        write_local_bootstrap_script(&data_dir).map_err(to_err)?;
        local_stage_spawn(app.clone());
        let status = wait_for_local_stage_completion(&data_dir).map_err(to_err)?;
        match status.state.as_str() {
            "ready" => Ok(DesktopLinuxSandboxEnsureResp { ready: true }),
            "downloaded_not_activated" => {
                let allow_user = current_local_user().map_err(to_err)?;
                match run_local_activation(
                    &data_dir,
                    &allow_user,
                    req.admin_password_once.as_deref(),
                )
                .map_err(to_err)?
                {
                    LocalActivationOutcome::NeedsPassword => {
                        return Err(format!(
                            "{LOCAL_ADMIN_PASSWORD_REQUIRED_SENTINEL}: Local admin password required to prepare sandbox on this machine."
                        ));
                    }
                    LocalActivationOutcome::Ready => {}
                }
                let state = app.state::<ConnectionManager>();
                let manager: &ConnectionManager = state.inner();
                let desktop_identity = load_desktop_build_identity(&app).map_err(to_err)?;
                restart_local_with_spawn_for_scope(&scope, manager, || {
                    spawn_and_validate_local_daemon(&app, &data_dir, &desktop_identity)
                })
                .map_err(to_err)?;
                Ok(DesktopLinuxSandboxEnsureResp { ready: true })
            }
            "manual_runtime_required" => Err(if status.message.trim().is_empty() {
                "Preparing Linux sandbox runtime failed. Managed sandbox setup is currently supported on Ubuntu/Debian only.".to_string()
            } else {
                format!("Preparing Linux sandbox runtime failed. {}", status.message.trim())
            }),
            other => Err(if status.message.trim().is_empty() {
                format!("Preparing Linux sandbox runtime failed. bootstrap_state={other}")
            } else {
                format!("Preparing Linux sandbox runtime failed. {}", status.message.trim())
            }),
        }
    })
    .await
    .map_err(|err| format!("Preparing Linux sandbox runtime failed. {err}"))?
}
