use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
mod prepare;
#[cfg(test)]
mod tests;
mod utils;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use ctx_harness_setup::{
    observe_log, observe_phase, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
};

pub use prepare::prepare_linux_sandbox_runtime;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
pub use utils::command_output_message;
use utils::{command_output_with_timeout, find_binary_in_path, redact_sensitive};

const NERDCTL_VERSION: &str = "v2.2.1";
const ROOTFUL_WRAPPER_PATH: &str = "/usr/local/bin/ctx-rootful-nerdctl";
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(300);
const BOOTSTRAP_SCRIPT: &str = include_str!("linux_sandbox_bootstrap.sh");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSandboxRuntimeState {
    Ready,
    DownloadPending,
    DownloadedNotActivated,
    Activating,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinuxSandboxRuntimeStatus {
    pub state: LinuxSandboxRuntimeState,
    pub supported: bool,
    pub distro: Option<String>,
    pub cache_root: String,
    pub staged_archive_path: Option<String>,
    pub activation_script_path: Option<String>,
    pub runtime_cli_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinuxSandboxActivationMode {
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinuxSandboxRuntimePrepareResult {
    pub ready: bool,
    pub needs_password: bool,
    pub status: LinuxSandboxRuntimeStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LinuxSandboxBootstrapStatus {
    state: String,
    supported: bool,
    #[serde(default)]
    distro: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LinuxSandboxPlatform {
    NotLinux,
    UbuntuDebian { distro: String },
    OtherLinux { distro: String },
}

#[derive(Debug, Clone)]
struct LinuxSandboxRuntimeSpec {
    arch: &'static str,
}

impl LinuxSandboxRuntimeSpec {
    fn current() -> Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Self { arch: "amd64" }),
            "aarch64" => Ok(Self { arch: "arm64" }),
            other => {
                anyhow::bail!("unsupported Linux architecture for managed sandbox runtime: {other}")
            }
        }
    }

    fn archive_file_name(&self) -> String {
        format!(
            "nerdctl-{}-linux-{}.tar.gz",
            NERDCTL_VERSION.trim_start_matches('v'),
            self.arch
        )
    }
}

#[derive(Debug, Clone)]
struct LinuxSandboxBootstrapPaths {
    cache_root: PathBuf,
    downloads_root: PathBuf,
    activation_script_path: PathBuf,
    staged_archive_path: Option<PathBuf>,
}

fn linux_sandbox_root(data_root: &Path) -> PathBuf {
    data_root.join("linux-sandbox-runtime")
}

fn linux_sandbox_cache_root(data_root: &Path) -> PathBuf {
    linux_sandbox_root(data_root).join("cache")
}

fn linux_sandbox_downloads_root(data_root: &Path) -> PathBuf {
    linux_sandbox_cache_root(data_root).join("downloads")
}

fn linux_sandbox_bootstrap_paths(data_root: &Path) -> LinuxSandboxBootstrapPaths {
    let root = linux_sandbox_root(data_root);
    let cache_root = linux_sandbox_cache_root(data_root);
    let downloads_root = linux_sandbox_downloads_root(data_root);
    let staged_archive_path = LinuxSandboxRuntimeSpec::current()
        .ok()
        .map(|spec| downloads_root.join(spec.archive_file_name()));
    LinuxSandboxBootstrapPaths {
        activation_script_path: root.join("bootstrap.sh"),
        cache_root,
        downloads_root,
        staged_archive_path,
    }
}

fn parse_os_release_value(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        let Some(value) = trimmed.strip_prefix(key) else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn linux_sandbox_platform() -> LinuxSandboxPlatform {
    if std::env::consts::OS != "linux" {
        return LinuxSandboxPlatform::NotLinux;
    }
    let contents = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distro = parse_os_release_value(&contents, "ID=")
        .or_else(|| parse_os_release_value(&contents, "NAME="))
        .unwrap_or_else(|| "linux".to_string());
    let id_like = parse_os_release_value(&contents, "ID_LIKE=").unwrap_or_default();
    let normalized_distro = distro.to_ascii_lowercase();
    let normalized_like = id_like.to_ascii_lowercase();
    if normalized_distro == "ubuntu"
        || normalized_distro == "debian"
        || normalized_like.contains("ubuntu")
        || normalized_like.contains("debian")
    {
        return LinuxSandboxPlatform::UbuntuDebian { distro };
    }
    LinuxSandboxPlatform::OtherLinux { distro }
}

fn is_posix_safe_username(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn current_username() -> Result<String> {
    let output = std::process::Command::new("id")
        .arg("-un")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running id -un for Linux sandbox bootstrap")?;
    if !output.status.success() {
        let detail = command_output_message(&output);
        if !detail.is_empty() {
            tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "id -un failed while preparing Linux sandbox runtime");
        }
        anyhow::bail!("Failed to determine current user while preparing Linux sandbox runtime");
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        anyhow::bail!("id -un returned an empty username while preparing Linux sandbox runtime");
    }
    if !is_posix_safe_username(&value) {
        anyhow::bail!(
            "id -un returned a non-POSIX-safe username while preparing Linux sandbox runtime"
        );
    }
    Ok(value)
}

pub fn preferred_native_sandbox_cli_path() -> Option<PathBuf> {
    let wrapper = PathBuf::from(ROOTFUL_WRAPPER_PATH);
    if wrapper.is_file() {
        return Some(wrapper);
    }
    None
}

async fn ensure_linux_sandbox_bootstrap_script(paths: &LinuxSandboxBootstrapPaths) -> Result<()> {
    fs::create_dir_all(&paths.downloads_root)
        .await
        .with_context(|| format!("creating {}", paths.downloads_root.display()))?;
    let should_write = match fs::read_to_string(&paths.activation_script_path).await {
        Ok(existing) => existing != BOOTSTRAP_SCRIPT,
        Err(err) if err.kind() == ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading {}", paths.activation_script_path.display()));
        }
    };
    if should_write {
        fs::write(&paths.activation_script_path, BOOTSTRAP_SCRIPT)
            .await
            .with_context(|| format!("writing {}", paths.activation_script_path.display()))?;
        let mut perms = fs::metadata(&paths.activation_script_path)
            .await
            .with_context(|| format!("stat {}", paths.activation_script_path.display()))?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&paths.activation_script_path, perms)
            .await
            .with_context(|| format!("chmod {}", paths.activation_script_path.display()))?;
    }
    Ok(())
}

fn parse_bootstrap_status(raw: &str) -> Result<LinuxSandboxBootstrapStatus> {
    serde_json::from_str::<LinuxSandboxBootstrapStatus>(raw.trim())
        .context("parsing Linux sandbox bootstrap status JSON")
}

fn normalize_bootstrap_state(raw: &str) -> LinuxSandboxRuntimeState {
    match raw {
        "ready" => LinuxSandboxRuntimeState::Ready,
        "downloaded_not_activated" => LinuxSandboxRuntimeState::DownloadedNotActivated,
        "activating" => LinuxSandboxRuntimeState::Activating,
        "manual_runtime_required" => LinuxSandboxRuntimeState::Unsupported,
        "failed" => LinuxSandboxRuntimeState::Failed,
        "download_pending" | "downloading" => LinuxSandboxRuntimeState::DownloadPending,
        _ => LinuxSandboxRuntimeState::Failed,
    }
}

fn platform_default_message(
    platform: &LinuxSandboxPlatform,
    state: &LinuxSandboxRuntimeState,
) -> String {
    match (platform, state) {
        (LinuxSandboxPlatform::NotLinux, _) => {
            "Managed Linux sandbox bootstrap is only used on Linux.".to_string()
        }
        (LinuxSandboxPlatform::UbuntuDebian { distro }, LinuxSandboxRuntimeState::Ready) => {
            format!("Linux sandbox runtime is ready on {distro}.")
        }
        (
            LinuxSandboxPlatform::UbuntuDebian { distro },
            LinuxSandboxRuntimeState::DownloadedNotActivated,
        ) => format!(
            "Linux sandbox runtime downloads are staged on {distro}. Activation runs when sandbox is selected."
        ),
        (
            LinuxSandboxPlatform::UbuntuDebian { distro },
            LinuxSandboxRuntimeState::DownloadPending,
        ) => format!(
            "ctx can manage the Linux sandbox runtime on {distro}. Downloads stage in background and activation runs when sandbox is selected."
        ),
        (LinuxSandboxPlatform::UbuntuDebian { distro }, LinuxSandboxRuntimeState::Activating) => {
            format!("Preparing the Linux sandbox runtime on {distro}.")
        }
        (LinuxSandboxPlatform::UbuntuDebian { distro }, LinuxSandboxRuntimeState::Failed) => {
            format!("Preparing the Linux sandbox runtime failed on {distro}.")
        }
        (LinuxSandboxPlatform::UbuntuDebian { distro }, LinuxSandboxRuntimeState::Unsupported) => {
            format!("Managed sandbox bootstrap is not available on {distro}.")
        }
        (LinuxSandboxPlatform::OtherLinux { distro }, LinuxSandboxRuntimeState::Ready) => {
            format!("Linux sandbox runtime is already ready on {distro}.")
        }
        (LinuxSandboxPlatform::OtherLinux { distro }, _) => format!(
            "ctx desktop is best-effort on {distro}. Sandbox requires a compatible runtime already installed."
        ),
    }
}

fn build_status(
    paths: &LinuxSandboxBootstrapPaths,
    platform: &LinuxSandboxPlatform,
    bootstrap: LinuxSandboxBootstrapStatus,
) -> LinuxSandboxRuntimeStatus {
    let state = normalize_bootstrap_state(&bootstrap.state);
    let supported = match platform {
        LinuxSandboxPlatform::UbuntuDebian { .. } => {
            bootstrap.supported || bootstrap.state != "manual_runtime_required"
        }
        LinuxSandboxPlatform::NotLinux | LinuxSandboxPlatform::OtherLinux { .. } => {
            bootstrap.supported
        }
    };
    let distro = if bootstrap.distro.trim().is_empty() {
        match platform {
            LinuxSandboxPlatform::NotLinux => None,
            LinuxSandboxPlatform::UbuntuDebian { distro }
            | LinuxSandboxPlatform::OtherLinux { distro } => Some(distro.clone()),
        }
    } else {
        Some(bootstrap.distro.trim().to_string())
    };
    let message = if bootstrap.message.trim().is_empty() {
        platform_default_message(platform, &state)
    } else {
        bootstrap.message.trim().to_string()
    };
    LinuxSandboxRuntimeStatus {
        state,
        supported,
        distro,
        cache_root: paths.cache_root.to_string_lossy().to_string(),
        staged_archive_path: paths
            .staged_archive_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        activation_script_path: Some(paths.activation_script_path.to_string_lossy().to_string()),
        runtime_cli_path: preferred_native_sandbox_cli_path()
            .or_else(|| find_binary_in_path("nerdctl"))
            .map(|path| path.to_string_lossy().to_string()),
        message,
    }
}

fn bootstrap_failed_status(
    paths: &LinuxSandboxBootstrapPaths,
    platform: &LinuxSandboxPlatform,
    message: String,
) -> LinuxSandboxRuntimeStatus {
    LinuxSandboxRuntimeStatus {
        state: LinuxSandboxRuntimeState::Failed,
        supported: matches!(platform, LinuxSandboxPlatform::UbuntuDebian { .. }),
        distro: match platform {
            LinuxSandboxPlatform::NotLinux => None,
            LinuxSandboxPlatform::UbuntuDebian { distro }
            | LinuxSandboxPlatform::OtherLinux { distro } => Some(distro.clone()),
        },
        cache_root: paths.cache_root.to_string_lossy().to_string(),
        staged_archive_path: paths
            .staged_archive_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        activation_script_path: Some(paths.activation_script_path.to_string_lossy().to_string()),
        runtime_cli_path: preferred_native_sandbox_cli_path()
            .or_else(|| find_binary_in_path("nerdctl"))
            .map(|path| path.to_string_lossy().to_string()),
        message,
    }
}

async fn run_bootstrap_mode(
    paths: &LinuxSandboxBootstrapPaths,
    data_root: &Path,
    mode: &str,
    allow_user: Option<&str>,
) -> Result<std::process::Output> {
    ensure_linux_sandbox_bootstrap_script(paths).await?;
    let mut command = Command::new(&paths.activation_script_path);
    command.arg(mode).arg("--data-dir").arg(data_root);
    if let Some(user_name) = allow_user {
        command.arg("--allow-user").arg(user_name);
    }
    command_output_with_timeout(command, BOOTSTRAP_TIMEOUT).await
}

async fn status_via_bootstrap(
    data_root: &Path,
    paths: &LinuxSandboxBootstrapPaths,
    platform: &LinuxSandboxPlatform,
) -> Result<LinuxSandboxRuntimeStatus> {
    if matches!(platform, LinuxSandboxPlatform::NotLinux) {
        return Ok(build_status(
            paths,
            platform,
            LinuxSandboxBootstrapStatus {
                state: "manual_runtime_required".to_string(),
                supported: false,
                distro: String::new(),
                message: "Managed Linux sandbox bootstrap is only used on Linux.".to_string(),
            },
        ));
    }
    let output = run_bootstrap_mode(paths, data_root, "status", None).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bootstrap = if output.status.success() {
        parse_bootstrap_status(&stdout)?
    } else if let Ok(parsed) = parse_bootstrap_status(&stdout) {
        parsed
    } else {
        let detail = command_output_message(&output);
        tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "Linux sandbox bootstrap status failed");
        anyhow::bail!("Linux sandbox runtime status check failed");
    };
    Ok(build_status(paths, platform, bootstrap))
}

pub async fn linux_sandbox_runtime_status(data_root: &Path) -> Result<LinuxSandboxRuntimeStatus> {
    let platform = linux_sandbox_platform();
    let paths = linux_sandbox_bootstrap_paths(data_root);
    match status_via_bootstrap(data_root, &paths, &platform).await {
        Ok(status) => Ok(status),
        Err(err) => {
            tracing::warn!(target: "linux_sandbox", error = %redact_sensitive(&err.to_string()), "linux_sandbox_runtime_status failed");
            Ok(bootstrap_failed_status(
                &paths,
                &platform,
                platform_default_message(&platform, &LinuxSandboxRuntimeState::Failed),
            ))
        }
    }
}

pub async fn stage_linux_sandbox_runtime_downloads(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<LinuxSandboxRuntimeStatus> {
    let platform = linux_sandbox_platform();
    let paths = linux_sandbox_bootstrap_paths(data_root);
    observe_phase(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        "staging Linux sandbox runtime downloads",
    );
    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        "staging Linux sandbox runtime downloads in the background",
    );
    let output = run_bootstrap_mode(&paths, data_root, "stage", None).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bootstrap = if output.status.success() {
        parse_bootstrap_status(&stdout)?
    } else if let Ok(parsed) = parse_bootstrap_status(&stdout) {
        parsed
    } else {
        let detail = command_output_message(&output);
        tracing::warn!(target: "linux_sandbox", detail = %redact_sensitive(&detail), "Linux sandbox runtime downloads failed to stage");
        anyhow::bail!("Linux sandbox runtime downloads failed to stage");
    };
    Ok(build_status(&paths, &platform, bootstrap))
}

fn activation_args(data_root: &Path, user_name: &str) -> Vec<String> {
    vec![
        "bash".to_string(),
        "-s".to_string(),
        "--".to_string(),
        "activate".to_string(),
        "--data-dir".to_string(),
        data_root.to_string_lossy().to_string(),
        "--allow-user".to_string(),
        user_name.to_string(),
    ]
}

async fn run_command_with_stdin(
    mut command: Command,
    stdin_bytes: &[u8],
) -> Result<std::process::Output> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("spawning Linux sandbox activation command")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_bytes)
            .await
            .context("writing Linux sandbox activation payload to stdin")?;
    }
    match tokio::time::timeout(BOOTSTRAP_TIMEOUT, child.wait_with_output()).await {
        Ok(output) => Ok(output.context("waiting for Linux sandbox activation command")?),
        Err(_) => anyhow::bail!(
            "Linux sandbox activation command timed out after {}s",
            BOOTSTRAP_TIMEOUT.as_secs()
        ),
    }
}

async fn run_sudo_with_password(args: &[String], password: &str) -> Result<std::process::Output> {
    let mut command = Command::new("sudo");
    command.arg("-S").arg("-p").arg("").args(args);
    let mut stdin = Vec::with_capacity(password.len() + BOOTSTRAP_SCRIPT.len() + 1);
    stdin.extend_from_slice(password.as_bytes());
    stdin.push(b'\n');
    stdin.extend_from_slice(BOOTSTRAP_SCRIPT.as_bytes());
    run_command_with_stdin(command, &stdin).await
}

fn sudo_needs_password(output: &std::process::Output) -> bool {
    let detail = command_output_message(output).to_ascii_lowercase();
    detail.contains("a password is required")
        || detail.contains("password is required")
        || detail.contains("sorry, try again")
        || detail.contains("incorrect password")
        || (detail.contains("sudo:")
            && (detail.contains("no tty")
                || detail.contains("askpass")
                || detail.contains("password")))
}

async fn try_sudo_non_interactive(args: &[String]) -> Result<std::process::Output> {
    let mut command = Command::new("sudo");
    command.arg("--non-interactive").args(args);
    run_command_with_stdin(command, BOOTSTRAP_SCRIPT.as_bytes()).await
}
