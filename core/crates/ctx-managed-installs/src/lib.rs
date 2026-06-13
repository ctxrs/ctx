use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use ctx_provider_install::install_state::{
    truncate_for_storage, InstallErrorCode, InstallEventLevel, InstallId, InstallInfo,
    InstallProgressEvent, InstallStateKind, InstallTarget,
};
use ctx_provider_matrix as provider_matrix;
use ctx_providers::adapters::{ProviderAdapter, ProviderStatus};
use ctx_providers::crp::Tier1CrpAdapter;
use sha2::Digest;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;

mod artifacts;
mod config;
mod dependencies;
mod install_policy;
mod managed_installers;
mod provider_install;
pub mod provider_install_contract;
pub mod provider_status_matrix;
mod runtime_commands;
mod runtime_lock;
mod targets;
pub mod title_generation;
pub mod title_generation_local;
mod toolchains;

#[cfg(test)]
mod provider_status_matrix_tests;
#[cfg(test)]
mod test_support;

pub(crate) use self::artifacts::{
    commit_atomic_install_dir, download_to_file, download_to_file_with_managed_runtime_redirects,
    ensure_executable, extract_tar_gz_to_dir, extract_zip_to_dir, find_unique_path_ending_with,
    install_agent_server_url_binary, prepare_atomic_install_dir, run_command_with_timeout,
    sha256_file, validate_expected_sha256, validate_sha256_digest,
};
use self::dependencies::{
    install_managed_archive_dependency, install_managed_npm_dependency, map_archive_kind,
    resolve_install_args,
};
use self::managed_installers::{
    install_managed_archive_provider, install_managed_npm_provider, install_managed_python_provider,
};
use self::provider_install::{
    classify_install_error, emit_install, ensure_install_not_cancelled, install_provider_impl,
    repair_install_dir, run_tracked_provider_install,
};
use self::runtime_commands::{is_acp_provider_id, managed_provider_runtime_command};
pub(crate) use ctx_bundled_assets as bundled_assets;

pub use config::{
    agent_server_config_path, apply_managed_install_details,
    apply_managed_install_details_for_target, load_agent_server_config,
    managed_dependency_install_metadata_for_target, managed_install_metadata_for_target,
    managed_provider_command_for_target, managed_provider_install_metadata_for_target,
    mutate_agent_server_config, resolve_provider_command, resolve_provider_login_command,
    resolve_runtime_provider_command, resolve_runtime_provider_command_for_target,
    resolve_runtime_provider_command_for_target_repairable_managed, save_agent_server_config,
    AgentServerCommand, AgentServerConfigFile, ManagedInstallError, ManagedInstallMetadata,
    ProviderLoginExecutable, ProviderRuntimeCommand, ProviderRuntimeCommandSource,
};
pub use provider_install::refresh_provider_statuses;
#[allow(unused_imports)]
pub use targets::{
    apply_install_target_status, ensure_codex_cli_command_env_for_target,
    prepend_runtime_bin_dirs_to_provider_path_for_target,
    require_codex_cli_command_path_for_target,
};
#[cfg(test)]
pub(crate) use targets::{
    dependency_target_compatible_with_context, prepend_bundled_seed_node_bin_dir,
    provider_env_targets_linux_sandbox, resolve_codex_cli_command_path_for_target,
};
pub use targets::{
    is_compatible_managed_provider_for_target, is_supported_managed_provider_for_target,
    managed_install_download_size_bytes, parse_install_target, resolve_matrix_target_key,
};
pub use title_generation::install_title_generation_local_with_progress;
#[allow(unused_imports)]
pub use toolchains::{
    archive_bin_requires_node_runtime, ensure_node_runtime, ensure_python_pip,
    ensure_python_runtime_versioned, install_dir_for_provider, install_dir_rel,
    node_runtime_dependency_id, node_runtime_dependency_metadata,
    node_runtime_dependency_targets_for_install_target, npm_dependency_matches, npm_install,
    npm_install_one, resolve_node_package_bin, sanitize_npm_package_for_path, venv_exe,
    NodeRuntime,
};

const NODE_VERSION: &str = "24.15.0";
const PYTHON_VERSION: &str = "3.13.13";
const PYTHON_BUILD_TAG: &str = "20260414";

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const NPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(12 * 60);
const PIP_INSTALL_TIMEOUT: Duration = Duration::from_secs(12 * 60);
const RETRY_COUNT: u32 = 2;
const RETRY_BACKOFF_BASE_MS: u64 = 750;
const LAST_ERROR_MAX_LEN: usize = 8000;
const INSTALL_EVENT_ERROR_MAX_LEN: usize = 6000;
const INSTALL_REGISTRY_POLL_INTERVAL: Duration = Duration::from_millis(100);

static NODE_RUNTIME_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PYTHON_RUNTIME_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PROVIDER_INSTALL_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
const TITLE_GENERATION_LOCAL_INSTALL_KEY: &str = "title_generation_local";
const MANAGED_PROVIDER_INSTALLS_ENABLED: bool = true;

#[async_trait]
pub trait ManagedInstallHost: Send + Sync + 'static {
    fn data_root(&self) -> &Path;

    fn current_ctx_version(&self) -> Option<String>;

    async fn load_provider_matrix(&self) -> provider_matrix::ProviderMatrix;

    async fn invalidate_provider_matrix_cache(&self);

    async fn inspect_provider_adapters(&self) -> Vec<(String, Result<ProviderStatus, String>)>;

    async fn upsert_provider_adapter(&self, provider_id: String, adapter: Arc<dyn ProviderAdapter>);

    async fn upsert_target_provider_adapter(
        &self,
        cache_key: String,
        adapter: Arc<dyn ProviderAdapter>,
    );

    async fn replace_provider_statuses(&self, statuses: HashMap<String, ProviderStatus>);

    fn validate_install_target_allowed(&self, _target: InstallTarget) -> Result<()> {
        Ok(())
    }

    async fn start_install(
        &self,
        provider_id: String,
        target: Option<InstallTarget>,
    ) -> (InstallId, bool);

    async fn get_install_info(&self, install_id: InstallId) -> Option<InstallInfo>;

    async fn register_install_progress_mirror(
        &self,
        source_install_id: InstallId,
        mirror_install_id: InstallId,
    ) -> bool;

    async fn set_install_progress_pct_override(&self, install_id: InstallId, pct: Option<u8>);

    async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent);

    async fn finish_install(
        &self,
        install_id: InstallId,
        success: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    );

    async fn is_install_cancelled(&self, install_id: InstallId) -> bool;

    async fn update_install_start_event(
        &self,
        install_id: InstallId,
        provider_id: &str,
        target: Option<InstallTarget>,
        message: String,
        only_if_default: bool,
    );

    async fn ensure_builder_ready(&self) -> Result<()>;

    async fn run_builder_command(
        &self,
        cwd: &Path,
        env: &[(String, String)],
        argv: &[String],
        timeout_dur: Duration,
    ) -> Result<Output>;

    fn is_acp_provider_id(&self, provider_id: &str) -> bool;

    fn normalize_acp_provider_command(
        &self,
        data_root: &Path,
        provider_id: &str,
        cmd: AgentServerCommand,
    ) -> Result<AgentServerCommand>;

    fn acp_bridge_command(
        &self,
        bridge_cmd: &AgentServerCommand,
        acp_cmd: AgentServerCommand,
    ) -> AgentServerCommand;
}

pub type ManagedInstallHostObject = dyn ManagedInstallHost;

#[async_trait]
pub trait InstallProgressHost: Send + Sync + 'static {
    async fn get_install_info(&self, install_id: InstallId) -> Option<InstallInfo>;

    async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent);

    async fn finish_install(
        &self,
        install_id: InstallId,
        success: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    );

    async fn is_install_cancelled(&self, install_id: InstallId) -> bool;
}

#[async_trait]
impl<T> InstallProgressHost for T
where
    T: ManagedInstallHost + ?Sized,
{
    async fn get_install_info(&self, install_id: InstallId) -> Option<InstallInfo> {
        ManagedInstallHost::get_install_info(self, install_id).await
    }

    async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent) {
        ManagedInstallHost::emit_install_event(self, install_id, event).await;
    }

    async fn finish_install(
        &self,
        install_id: InstallId,
        success: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    ) {
        ManagedInstallHost::finish_install(self, install_id, success, error, error_code).await;
    }

    async fn is_install_cancelled(&self, install_id: InstallId) -> bool {
        ManagedInstallHost::is_install_cancelled(self, install_id).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPythonRuntimeSpec {
    version: String,
    build_tag: String,
}

pub fn expected_managed_dependency_version(dependency_id: &str) -> Option<&'static str> {
    let normalized = dependency_id.trim().to_ascii_lowercase();
    if normalized.starts_with("runtime-node-") {
        return Some(NODE_VERSION);
    }
    if normalized.starts_with("runtime-python-") {
        return Some(PYTHON_VERSION);
    }
    None
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn npm_artifact_fingerprint(package: &str, version: &str) -> Option<String> {
    let package = package.trim();
    let version = version.trim();
    if package.is_empty() || version.is_empty() {
        return None;
    }
    Some(format!("npm:{package}@{version}"))
}

fn python_artifact_fingerprint(
    package: &str,
    version: &str,
    python_version: Option<&str>,
    python_build_tag: Option<&str>,
    python_runtime_sha256: Option<&str>,
) -> Option<String> {
    let package = package.trim();
    let version = version.trim();
    if package.is_empty() || version.is_empty() {
        return None;
    }
    let mut fingerprint = format!("python:{package}=={version}");
    if let Some(python_version) = trimmed_non_empty(python_version) {
        fingerprint.push_str(&format!("|python={python_version}"));
    }
    if let Some(python_build_tag) = trimmed_non_empty(python_build_tag) {
        fingerprint.push_str(&format!("|build={python_build_tag}"));
    }
    if let Some(python_runtime_sha256) = trimmed_non_empty(python_runtime_sha256) {
        fingerprint.push_str(&format!("|runtime_sha256={python_runtime_sha256}"));
    }
    Some(fingerprint)
}

fn expected_python_runtime_sha256_for_target(
    python_version: &str,
    python_build_tag: &str,
    target: InstallTarget,
) -> Option<String> {
    if toolchains::python_target_can_use_bundled_runtime(target) {
        if let Some(bundled) = bundled_assets::bundled_python_runtime_version(python_version) {
            if bundled.version == python_version {
                return Some(bundled.sha256);
            }
        }
    }
    let target_triple = toolchains::python_target_triple_for_install_target(target).ok()?;
    runtime_lock::resolve_python_runtime_archive(python_version, python_build_tag, target_triple)
        .ok()
        .map(|spec| spec.sha256.to_string())
}

fn expected_node_runtime_sha256_for_target(target: InstallTarget) -> Option<String> {
    if matches!(target, InstallTarget::Host) {
        if let Some(bundled) = bundled_assets::bundled_node_runtime() {
            if bundled.version == NODE_VERSION && bundled.npm_cli.is_some() {
                return Some(bundled.sha256);
            }
        }
    }
    let node_target = toolchains::node_runtime_target_for_install_target(target).ok()?;
    let archive_kind = if node_target.is_windows {
        runtime_lock::ManagedRuntimeArchiveKind::Zip
    } else {
        runtime_lock::ManagedRuntimeArchiveKind::TarGz
    };
    runtime_lock::resolve_node_runtime_archive(NODE_VERSION, node_target.dist_target, archive_kind)
        .ok()
        .map(|spec| spec.sha256.to_string())
}

fn expected_node_runtime_artifact_fingerprint(target: InstallTarget) -> Option<String> {
    expected_node_runtime_sha256_for_target(target)
        .map(|sha256| format!("runtime:node:{NODE_VERSION}:sha256:{sha256}"))
}

pub fn expected_managed_provider_artifact_fingerprint(
    entry: &provider_matrix::ProviderMatrixEntry,
    version: &str,
    target: InstallTarget,
) -> Option<String> {
    let install = entry.managed_install.as_ref()?;
    match install {
        provider_matrix::ProviderInstall::Npm {
            package,
            version: install_version,
            targets,
            ..
        } => {
            if provider_matrix::normalize_version(install_version)
                != provider_matrix::normalize_version(version)
            {
                return None;
            }
            targets
                .get(resolve_matrix_target_key(target).ok()?)
                .and_then(|target_entry| trimmed_non_empty(target_entry.sha256.as_deref()))
                .or_else(|| {
                    if matches!(target, InstallTarget::Host) {
                        npm_artifact_fingerprint(package, install_version)
                    } else {
                        None
                    }
                })
        }
        provider_matrix::ProviderInstall::Archive {
            version: install_version,
            targets,
            ..
        } => {
            if provider_matrix::normalize_version(install_version)
                != provider_matrix::normalize_version(version)
            {
                return None;
            }
            let target_key = resolve_matrix_target_key(target).ok()?;
            trimmed_non_empty(targets.get(target_key)?.sha256.as_deref())
        }
        provider_matrix::ProviderInstall::Python {
            package,
            version: install_version,
            targets,
            python_version,
            python_build_tag,
            ..
        } => {
            if provider_matrix::normalize_version(install_version)
                != provider_matrix::normalize_version(version)
            {
                return None;
            }
            targets
                .get(resolve_matrix_target_key(target).ok()?)
                .and_then(|target_entry| trimmed_non_empty(target_entry.sha256.as_deref()))
                .or_else(|| {
                    if matches!(target, InstallTarget::Host) {
                        let runtime_spec = managed_python_runtime_spec(
                            python_version.as_deref(),
                            python_build_tag.as_deref(),
                        );
                        let runtime_sha256 = expected_python_runtime_sha256_for_target(
                            &runtime_spec.version,
                            &runtime_spec.build_tag,
                            target,
                        );
                        python_artifact_fingerprint(
                            package,
                            install_version,
                            Some(&runtime_spec.version),
                            Some(&runtime_spec.build_tag),
                            runtime_sha256.as_deref(),
                        )
                    } else {
                        None
                    }
                })
        }
    }
}

pub fn expected_managed_dependency_artifact_fingerprint(
    dependency: &provider_matrix::ProviderDependency,
    target: InstallTarget,
) -> Option<String> {
    match &dependency.install {
        provider_matrix::DependencyInstall::Npm { package, version } => {
            npm_artifact_fingerprint(package, version)
        }
        provider_matrix::DependencyInstall::Archive {
            version: _,
            targets,
        } => {
            let target_key = resolve_matrix_target_key(target).ok()?;
            trimmed_non_empty(targets.get(target_key)?.sha256.as_deref())
        }
    }
}

pub fn expected_managed_dependency_artifact_fingerprint_for_id(
    dependency_id: &str,
    dependency: Option<&provider_matrix::ProviderDependency>,
    target: InstallTarget,
) -> Option<String> {
    let normalized = dependency_id.trim().to_ascii_lowercase();
    if normalized.starts_with("runtime-node-") {
        return expected_node_runtime_artifact_fingerprint(target);
    }
    dependency
        .and_then(|dependency| expected_managed_dependency_artifact_fingerprint(dependency, target))
}

fn node_runtime_install_lock() -> &'static Mutex<()> {
    NODE_RUNTIME_INSTALL_LOCK.get_or_init(|| Mutex::new(()))
}

fn python_runtime_install_lock() -> &'static Mutex<()> {
    PYTHON_RUNTIME_INSTALL_LOCK.get_or_init(|| Mutex::new(()))
}

fn managed_python_runtime_spec(
    python_version: Option<&str>,
    python_build_tag: Option<&str>,
) -> ManagedPythonRuntimeSpec {
    let version = python_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(PYTHON_VERSION)
        .to_string();
    let build_tag = python_build_tag
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(PYTHON_BUILD_TAG)
        .to_string();
    ManagedPythonRuntimeSpec { version, build_tag }
}

fn provider_install_locks() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    PROVIDER_INSTALL_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn acquire_provider_install_lock(
    provider_id: &str,
    target: InstallTarget,
) -> tokio::sync::OwnedMutexGuard<()> {
    let key = format!("{provider_id}@{}", target.as_str());
    let lock = {
        let mut locks = provider_install_locks().lock().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    lock.lock_owned().await
}

pub async fn install_provider(state: &ManagedInstallHostObject, provider_id: &str) -> Result<()> {
    install_provider_impl(state, provider_id, InstallTarget::Host, None).await
}

pub fn is_supported_managed_provider(
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
) -> bool {
    if !MANAGED_PROVIDER_INSTALLS_ENABLED {
        return false;
    }
    provider_matrix::is_managed_supported_for_context(matrix, provider_id, None)
}

fn validate_post_install_status(
    status: &ctx_providers::adapters::ProviderStatus,
    provider_id: &str,
    target: InstallTarget,
) -> Result<()> {
    if let Some(managed_target) = status.details.get("managed_target") {
        if managed_target != target.as_str() {
            anyhow::bail!(
                "install completed but provider '{}' resolved to managed target '{}' (expected '{}')",
                provider_id,
                managed_target,
                target.as_str()
            );
        }
    }
    if !status.installed || !matches!(status.health, ctx_providers::adapters::ProviderHealth::Ok) {
        anyhow::bail!(
            "install completed but provider is not healthy: {}",
            status.diagnostics.join("; ")
        );
    }
    Ok(())
}

pub async fn install_provider_with_progress(
    state: std::sync::Arc<ManagedInstallHostObject>,
    install_id: InstallId,
    provider_id: String,
    target: InstallTarget,
) -> Result<()> {
    run_tracked_provider_install(state.as_ref(), install_id, &provider_id, target).await
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AgentServerArchive {
    None,
    TarGz,
    TarBz2,
    Zip,
    Dmg,
}

fn host_target_key() -> Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("windows", "aarch64") => Ok("windows-aarch64"),
        _ => anyhow::bail!("unsupported platform: {os}/{arch}"),
    }
}

fn container_target_key() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("linux-x86_64"),
        "aarch64" => Ok("linux-aarch64"),
        other => anyhow::bail!("unsupported container architecture: {other}"),
    }
}

struct ManagedProviderInstall {
    command: String,
    args: Vec<String>,
    meta: ManagedInstallMetadata,
}

struct ManagedDependencyInstall {
    meta: ManagedInstallMetadata,
}

#[cfg(test)]
mod tests;
