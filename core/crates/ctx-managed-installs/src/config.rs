use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use ctx_bundled_assets as bundled_assets;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use self::migration::{bundled_only_mode_applies_to_provider, migrate_agent_server_config};
use self::targeting::{
    infer_legacy_managed_target, install_target_bucket_key, legacy_managed_metadata_matches_target,
    migrate_managed_provider_command_args, requested_target_or_host, target_bucket_lookup,
};
use super::expected_managed_dependency_version;
use ctx_provider_install::install_state::{truncate_for_storage, InstallErrorCode, InstallTarget};

mod migration;
mod targeting;

static AGENT_SERVER_CONFIG_MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn agent_server_config_mutation_lock() -> &'static Mutex<()> {
    AGENT_SERVER_CONFIG_MUTATION_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedInstallError {
    pub at: String,
    pub stage: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<InstallErrorCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedInstallMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_fingerprint: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "archive_sha256",
        alias = "sha256"
    )]
    pub archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<InstallTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir_rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_dir_rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ManagedInstallError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentServerCommand {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedInstallMetadata>,
}

/// Internal-only configuration for provider-specific login executables.
///
/// Login execution is narrower than provider runtime execution: ctx owns the
/// invocation contract and persists only the explicit executable path for the
/// providers whose login binary differs from the runtime command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderLoginExecutable {
    pub executable_path: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AgentServerConfigFile {
    #[serde(default)]
    pub providers: HashMap<String, AgentServerCommand>,
    #[serde(default)]
    pub provider_login_executables: HashMap<String, ProviderLoginExecutable>,
    #[serde(default, skip_serializing)]
    pub provider_login_commands: HashMap<String, AgentServerCommand>,
    #[serde(default)]
    pub managed_installs: HashMap<String, ManagedInstallMetadata>,
    #[serde(default)]
    pub managed_provider_targets: HashMap<String, HashMap<String, AgentServerCommand>>,
    #[serde(default)]
    pub managed_install_targets: HashMap<String, HashMap<String, ManagedInstallMetadata>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRuntimeCommandSource {
    UserOverride,
    ManagedInstall,
    BundledSeed,
    PreparedLoginExecutable,
}

impl ProviderRuntimeCommandSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserOverride => "user_override",
            Self::ManagedInstall => "managed_install",
            Self::BundledSeed => "bundled_seed",
            Self::PreparedLoginExecutable => "prepared_login_executable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRuntimeCommand {
    pub provider_id: String,
    pub command_abs_path: String,
    pub args: Vec<String>,
    pub dependencies: Vec<String>,
    pub source: ProviderRuntimeCommandSource,
}

fn user_override_provider_command(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
) -> Option<AgentServerCommand> {
    let configured = cfg.providers.get(provider_id)?;
    configured.managed.is_none().then(|| configured.clone())
}

fn configured_provider_login_command<'a>(
    cfg: &'a AgentServerConfigFile,
    provider_id: &str,
) -> Option<&'a ProviderLoginExecutable> {
    cfg.provider_login_executables.get(provider_id)
}

pub fn managed_install_metadata_for_target<'a>(
    cfg: &'a AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Option<&'a ManagedInstallMetadata> {
    target_bucket_lookup(&cfg.managed_install_targets, provider_id, requested_target)
        .or_else(|| {
            cfg.providers
                .get(provider_id)
                .and_then(|e| e.managed.as_ref())
                .filter(|meta| legacy_managed_metadata_matches_target(meta, requested_target))
        })
        .or_else(|| {
            cfg.managed_installs
                .get(provider_id)
                .filter(|meta| legacy_managed_metadata_matches_target(meta, requested_target))
        })
}

pub fn managed_provider_install_metadata_for_target<'a>(
    cfg: &'a AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Option<&'a ManagedInstallMetadata> {
    target_bucket_lookup(&cfg.managed_install_targets, provider_id, requested_target)
}

pub(crate) fn managed_dependency_target_from_id(dependency_id: &str) -> Option<InstallTarget> {
    targeting::managed_dependency_target_from_id(dependency_id)
}

pub fn managed_dependency_install_metadata_for_target<'a>(
    cfg: &'a AgentServerConfigFile,
    dependency_id: &str,
    requested_target: Option<InstallTarget>,
) -> Option<&'a ManagedInstallMetadata> {
    let dependency_target = targeting::managed_dependency_target_from_id(dependency_id);
    managed_install_metadata_for_target(cfg, dependency_id, dependency_target.or(requested_target))
}

pub fn managed_provider_command_for_target(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Option<AgentServerCommand> {
    target_bucket_lookup(&cfg.managed_provider_targets, provider_id, requested_target).cloned()
}

pub fn apply_managed_install_details_for_target(
    status: &mut ctx_providers::adapters::ProviderStatus,
    cfg: &AgentServerConfigFile,
    requested_target: Option<InstallTarget>,
) {
    let provider_id = status.provider_id.as_str();
    let Some(meta) =
        managed_provider_install_metadata_for_target(cfg, provider_id, requested_target)
    else {
        return;
    };

    if let Some(v) = &meta.version {
        status
            .details
            .insert("managed_version".to_string(), v.clone());
    }
    if let Some(fingerprint) = &meta.artifact_fingerprint {
        status.details.insert(
            "managed_artifact_fingerprint".to_string(),
            fingerprint.clone(),
        );
    }
    if let Some(archive_sha256) = &meta.archive_sha256 {
        status
            .details
            .insert("managed_archive_sha256".to_string(), archive_sha256.clone());
    }
    if let Some(target) = meta.target {
        status
            .details
            .insert("managed_target".to_string(), target.as_str().to_string());
    }
    if let Some(p) = &meta.package {
        status
            .details
            .insert("managed_package".to_string(), p.clone());
    }
    if let Some(d) = &meta.install_dir_rel {
        status
            .details
            .insert("managed_install_dir".to_string(), d.clone());
    }
    if let Some(d) = &meta.bin_dir_rel {
        status
            .details
            .insert("managed_bin_dir".to_string(), d.clone());
    }
    if let Some(ts) = &meta.last_success_at {
        status
            .details
            .insert("managed_last_success_at".to_string(), ts.clone());
    }
    if let Some(err) = &meta.last_error {
        status.details.insert(
            "managed_last_error".to_string(),
            truncate_for_storage(&err.message, 1200),
        );
        status
            .details
            .insert("managed_last_error_at".to_string(), err.at.clone());
        status
            .details
            .insert("managed_last_error_stage".to_string(), err.stage.clone());
    }
}

pub fn apply_managed_install_details(
    status: &mut ctx_providers::adapters::ProviderStatus,
    cfg: &AgentServerConfigFile,
) {
    apply_managed_install_details_for_target(status, cfg, None);
}

pub fn resolve_provider_command(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
) -> Option<AgentServerCommand> {
    if let Some(configured) = user_override_provider_command(cfg, provider_id) {
        return Some(configured.clone());
    }
    if let Some(configured) = managed_provider_command_for_target(cfg, provider_id, None) {
        return Some(configured);
    }
    if let Some(bundled) = bundled_assets::bundled_provider_command(provider_id) {
        return Some(AgentServerCommand {
            command: bundled.command,
            args: bundled.args,
            dependencies: Vec::new(),
            managed: None,
        });
    }
    None
}

fn runtime_command_candidate(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<(AgentServerCommand, ProviderRuntimeCommandSource)>> {
    let allow_bundled_seed = matches!(
        requested_target_or_host(requested_target),
        InstallTarget::Host
    );
    if bundled_only_mode_applies_to_provider(provider_id) {
        if allow_bundled_seed {
            if let Some(bundled) = bundled_assets::bundled_provider_command(provider_id) {
                return Ok(Some((
                    AgentServerCommand {
                        command: bundled.command,
                        args: bundled.args,
                        dependencies: Vec::new(),
                        managed: None,
                    },
                    ProviderRuntimeCommandSource::BundledSeed,
                )));
            }
        } else {
            anyhow::bail!(
                "runtime_command_missing_bundled_target: provider={provider_id} target={target}",
                target = requested_target_or_host(requested_target).as_str()
            );
        }
        anyhow::bail!(
            "runtime_command_missing_bundled: provider={provider_id} (set CTX_BUNDLE_DIR and ensure bundled manifest includes provider)",
        );
    }

    if let Some(configured) = user_override_provider_command(cfg, provider_id) {
        return Ok(Some((
            configured,
            ProviderRuntimeCommandSource::UserOverride,
        )));
    }

    if let Some(configured) =
        managed_provider_command_for_target(cfg, provider_id, requested_target)
    {
        return Ok(Some((
            configured,
            ProviderRuntimeCommandSource::ManagedInstall,
        )));
    }
    if allow_bundled_seed {
        if let Some(bundled) = bundled_assets::bundled_provider_command(provider_id) {
            return Ok(Some((
                AgentServerCommand {
                    command: bundled.command,
                    args: bundled.args,
                    dependencies: Vec::new(),
                    managed: None,
                },
                ProviderRuntimeCommandSource::BundledSeed,
            )));
        }
    }
    Ok(None)
}

fn preserve_raw_bundle_command_path(path: &Path) -> Option<PathBuf> {
    let raw_bundle_dir = std::env::var("CTX_BUNDLE_DIR").ok()?;
    let raw_bundle_dir = PathBuf::from(raw_bundle_dir.trim());
    if raw_bundle_dir.as_os_str().is_empty() || !raw_bundle_dir.is_absolute() {
        return None;
    }
    path.starts_with(&raw_bundle_dir)
        .then(|| path.to_path_buf())
}

fn resolve_absolute_command_path(provider_id: &str, source: &str, raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("runtime_command_missing: provider={provider_id} source={source}",);
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        anyhow::bail!(
            "runtime_command_not_absolute: provider={provider_id} source={source} command={trimmed}",
        );
    }
    if !path.exists() {
        anyhow::bail!(
            "runtime_command_not_found: provider={provider_id} source={source} command={trimmed}",
        );
    }
    Ok(preserve_raw_bundle_command_path(path)
        .unwrap_or_else(|| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())))
}

fn build_provider_runtime_command(
    provider_id: &str,
    candidate: &AgentServerCommand,
    resolve_source: &str,
    source: ProviderRuntimeCommandSource,
) -> Result<ProviderRuntimeCommand> {
    let command_abs_path =
        resolve_absolute_command_path(provider_id, resolve_source, &candidate.command)?
            .to_string_lossy()
            .to_string();

    Ok(ProviderRuntimeCommand {
        provider_id: provider_id.to_string(),
        command_abs_path,
        args: candidate.args.clone(),
        dependencies: candidate.dependencies.clone(),
        source,
    })
}

/// Configured login executables are narrower than provider runtime commands:
/// they only pin the provider-owned login executable path.
pub fn resolve_provider_login_command(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
) -> Result<Option<ProviderRuntimeCommand>> {
    let Some(configured) = configured_provider_login_command(cfg, provider_id) else {
        return Ok(None);
    };
    let candidate = AgentServerCommand {
        command: configured.executable_path.clone(),
        args: Vec::new(),
        dependencies: Vec::new(),
        managed: None,
    };
    build_provider_runtime_command(
        provider_id,
        &candidate,
        "login_executable",
        ProviderRuntimeCommandSource::PreparedLoginExecutable,
    )
    .map(Some)
}

pub fn resolve_runtime_provider_command_for_target(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<ProviderRuntimeCommand>> {
    let Some((candidate, source)) = runtime_command_candidate(cfg, provider_id, requested_target)?
    else {
        return Ok(None);
    };
    build_provider_runtime_command(provider_id, &candidate, source.as_str(), source).map(Some)
}

pub fn resolve_runtime_provider_command_for_target_repairable_managed(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<Option<ProviderRuntimeCommand>> {
    let Some((candidate, source)) = runtime_command_candidate(cfg, provider_id, requested_target)?
    else {
        return Ok(None);
    };
    match build_provider_runtime_command(provider_id, &candidate, source.as_str(), source) {
        Ok(command) => Ok(Some(command)),
        Err(_err) if matches!(source, ProviderRuntimeCommandSource::ManagedInstall) => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn resolve_runtime_provider_command(
    cfg: &AgentServerConfigFile,
    provider_id: &str,
) -> Result<Option<ProviderRuntimeCommand>> {
    resolve_runtime_provider_command_for_target(cfg, provider_id, None)
}

#[cfg(test)]
mod tests;
pub fn agent_server_config_path(data_root: &Path) -> PathBuf {
    data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json")
}

pub async fn load_agent_server_config(data_root: &Path) -> Result<AgentServerConfigFile> {
    let path = agent_server_config_path(data_root);
    if !path.exists() {
        return Ok(AgentServerConfigFile::default());
    }
    let txt = tokio::fs::read_to_string(&path).await?;
    if txt.trim().is_empty() {
        return Ok(AgentServerConfigFile::default());
    }
    let mut cfg: AgentServerConfigFile =
        serde_json::from_str(&txt).context("parsing agent server config")?;
    if migrate_agent_server_config(&mut cfg) {
        if let Err(error) = save_agent_server_config(data_root, &cfg).await {
            tracing::warn!(
                "failed to persist migrated agent server config at {}: {error:#}",
                path.display()
            );
        }
    }
    Ok(cfg)
}

pub async fn mutate_agent_server_config<T, F>(data_root: &Path, mutate: F) -> Result<T>
where
    F: FnOnce(&mut AgentServerConfigFile) -> T,
{
    let _guard = agent_server_config_mutation_lock().lock().await;
    let mut cfg = load_agent_server_config(data_root).await?;
    let result = mutate(&mut cfg);
    save_agent_server_config(data_root, &cfg).await?;
    Ok(result)
}

pub async fn save_agent_server_config(data_root: &Path, cfg: &AgentServerConfigFile) -> Result<()> {
    let mut cfg_to_save = cfg.clone();
    migrate_agent_server_config(&mut cfg_to_save);
    let path = agent_server_config_path(data_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = path.with_file_name(format!(
        "{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        nanos
    ));
    tokio::fs::write(&tmp_path, serde_json::to_string_pretty(&cfg_to_save)?).await?;
    if let Err(err) = tokio::fs::rename(&tmp_path, &path).await {
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::rename(&tmp_path, &path).await?;
        if !matches!(err.kind(), std::io::ErrorKind::AlreadyExists) {
            return Err(err.into());
        }
    }
    Ok(())
}
