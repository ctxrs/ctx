use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::AgentServerConfigFile;
use ctx_provider_install::install_state::InstallTarget;
use ctx_provider_matrix::{
    extract_version, latest_release, normalize_version, recommended_release, release_for_version,
    release_matches_context, DependencyInstall, ProviderCommand, ProviderInstall,
    ProviderMatrixEntry, ProviderReleaseStatus, VersionProbe,
};

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const MATRIX_STATUS_DETAIL_KEYS: &[&str] = &[
    "managed_checksum_mismatch",
    "managed_dependency_update_available",
    "managed_detected_archive_sha256",
    "managed_detected_fingerprint",
    "managed_expected_archive_sha256",
    "managed_expected_fingerprint",
    "managed_fingerprint_mismatch",
    "matrix_detected_upstream_version",
    "matrix_latest_upstream_version",
    "matrix_latest_version",
    "matrix_recommended_upstream_version",
    "matrix_recommended_version",
    "matrix_update_available",
    "matrix_update_requires_context",
];

pub async fn apply_matrix_to_status(
    data_root: &Path,
    cfg: &AgentServerConfigFile,
    entry: &ProviderMatrixEntry,
    status: &mut ctx_providers::adapters::ProviderStatus,
    current_ctx_version: Option<&str>,
) {
    for key in MATRIX_STATUS_DETAIL_KEYS {
        status.details.remove(*key);
    }
    status
        .details
        .insert("provider_kind".to_string(), entry.kind.as_str().to_string());
    let context_version = current_ctx_version.and_then(ctx_provider_matrix::parse_version_loose);
    let context_version = context_version.as_ref();

    let detected_version = detect_provider_version(data_root, cfg, entry, status).await;
    if let Some(version) = detected_version.clone() {
        status.version = Some(version);
    }

    if let Some(rec) = recommended_release(entry, context_version) {
        status.details.insert(
            "matrix_recommended_version".to_string(),
            rec.version.clone(),
        );
        if let Some(upstream) = rec.upstream_version.as_ref() {
            status.details.insert(
                "matrix_recommended_upstream_version".to_string(),
                upstream.clone(),
            );
        }
    }
    if let Some(latest) = latest_release(entry) {
        status
            .details
            .insert("matrix_latest_version".to_string(), latest.version.clone());
        if let Some(upstream) = latest.upstream_version.as_ref() {
            status.details.insert(
                "matrix_latest_upstream_version".to_string(),
                upstream.clone(),
            );
        }
    }

    let mut diagnostics = Vec::new();
    let is_archive_install = install_target_from_status(status)
        .and_then(|target| {
            entry
                .managed_install
                .as_ref()
                .and_then(|install| {
                    install.archive_target(crate::resolve_matrix_target_key(target).ok()?)
                })
                .map(|_| true)
        })
        .unwrap_or(matches!(
            entry.managed_install.as_ref(),
            Some(ProviderInstall::Archive { .. })
        ));
    if let Some((expected_fingerprint, detected_fingerprint)) =
        detect_managed_artifact_fingerprint_mismatch(
            cfg,
            entry,
            status,
            detected_version.as_deref(),
        )
        .await
    {
        status.details.insert(
            "managed_fingerprint_mismatch".to_string(),
            "true".to_string(),
        );
        status.details.insert(
            "managed_expected_fingerprint".to_string(),
            expected_fingerprint.clone(),
        );
        status.details.insert(
            "managed_detected_fingerprint".to_string(),
            detected_fingerprint.clone(),
        );
        if is_archive_install {
            status
                .details
                .insert("managed_checksum_mismatch".to_string(), "true".to_string());
            status.details.insert(
                "managed_expected_archive_sha256".to_string(),
                expected_fingerprint.clone(),
            );
            status.details.insert(
                "managed_detected_archive_sha256".to_string(),
                detected_fingerprint.clone(),
            );
        }
        status
            .details
            .insert("matrix_update_available".to_string(), "true".to_string());
        status.installed = false;
        status.capabilities = None;
        status.health = ctx_providers::adapters::ProviderHealth::Error;
        if is_archive_install {
            diagnostics.push(format!(
                "Managed provider archive checksum mismatch; expected {expected_fingerprint}, found {detected_fingerprint}. Reinstall {} to restore the pinned release.",
                status.provider_id
            ));
        } else {
            diagnostics.push(format!(
                "Managed provider artifact mismatch; expected {expected_fingerprint}, found {detected_fingerprint}. Reinstall {} to restore the pinned release.",
                status.provider_id
            ));
        }
    }

    let mut unsupported_version = false;
    if status.installed {
        if let Some(version) = detected_version.as_deref() {
            match release_for_version(entry, version) {
                Some(release) => {
                    if let Some(upstream) = release.upstream_version.as_ref() {
                        status.details.insert(
                            "matrix_detected_upstream_version".to_string(),
                            upstream.clone(),
                        );
                    }
                    if release.status != ProviderReleaseStatus::Supported {
                        unsupported_version = true;
                        diagnostics.push(format!(
                            "Provider version {} is blocked by the support matrix",
                            release.version
                        ));
                    } else if !release_matches_context(release, context_version) {
                        unsupported_version = true;
                        let mut msg = "Provider version requires a newer ctx build".to_string();
                        if let Some(min) = release.context_min.as_ref() {
                            msg = format!("Provider version requires ctx >= {min}");
                        }
                        diagnostics.push(msg);
                    }
                }
                None => {
                    unsupported_version = true;
                    diagnostics.push(format!(
                        "Provider version {version} is not in the support matrix"
                    ));
                }
            }
        } else {
            diagnostics.push("Unable to determine provider version".to_string());
        }
    }

    if !diagnostics.is_empty() {
        status.diagnostics.extend(diagnostics);
    }

    if matches!(
        status.health,
        ctx_providers::adapters::ProviderHealth::Ok
            | ctx_providers::adapters::ProviderHealth::UnsupportedVersion
    ) {
        status.health = if unsupported_version {
            ctx_providers::adapters::ProviderHealth::UnsupportedVersion
        } else {
            ctx_providers::adapters::ProviderHealth::Ok
        };
    }

    let release_update_available = match (
        detected_version.as_deref(),
        status.details.get("matrix_recommended_version"),
    ) {
        (Some(installed), Some(recommended)) => {
            normalize_version(installed) != normalize_version(recommended)
        }
        _ => false,
    };
    let dependency_update_available =
        status.installed && managed_dependency_update_available(cfg, entry, status);
    if dependency_update_available {
        status.details.insert(
            "managed_dependency_update_available".to_string(),
            "true".to_string(),
        );
    }
    let update_available = release_update_available || dependency_update_available;
    if update_available {
        status
            .details
            .insert("matrix_update_available".to_string(), "true".to_string());
    }

    let update_requires_context = match (
        status.details.get("matrix_recommended_version"),
        status.details.get("matrix_latest_version"),
    ) {
        (Some(recommended), Some(latest)) => {
            normalize_version(recommended) != normalize_version(latest)
        }
        _ => false,
    };
    if update_requires_context {
        status.details.insert(
            "matrix_update_requires_context".to_string(),
            "true".to_string(),
        );
    }
}

pub fn managed_dependency_update_available(
    cfg: &AgentServerConfigFile,
    entry: &ProviderMatrixEntry,
    status: &ctx_providers::adapters::ProviderStatus,
) -> bool {
    let requested_target = install_target_from_status(status);
    let command =
        crate::managed_provider_command_for_target(cfg, &status.provider_id, requested_target)
            .or_else(|| {
                cfg.providers
                    .get(&status.provider_id)
                    .filter(|command| command.managed.is_none())
                    .cloned()
            });
    let Some(command) = command else {
        return false;
    };
    command.dependencies.iter().any(|dependency_id| {
        let dependency = entry
            .dependencies
            .iter()
            .find(|candidate| candidate.id == *dependency_id);
        let expected_version = crate::expected_managed_dependency_version(dependency_id)
            .map(ToOwned::to_owned)
            .or_else(|| {
                dependency.map(|dependency| match &dependency.install {
                    DependencyInstall::Npm { version, .. }
                    | DependencyInstall::Archive { version, .. } => version.clone(),
                })
            });
        let Some(expected_version) = expected_version else {
            return false;
        };
        let meta = match crate::managed_dependency_install_metadata_for_target(
            cfg,
            dependency_id,
            requested_target,
        ) {
            Some(meta) => meta,
            None => return true,
        };
        match meta.version.as_deref() {
            Some(installed)
                if normalize_version(installed) == normalize_version(&expected_version) => {}
            Some(_) | None => return true,
        }

        let dependency_target = crate::config::managed_dependency_target_from_id(dependency_id)
            .or(meta.target)
            .or(requested_target)
            .unwrap_or(InstallTarget::Host);
        let Some(expected_fingerprint) =
            crate::expected_managed_dependency_artifact_fingerprint_for_id(
                dependency_id,
                dependency,
                dependency_target,
            )
        else {
            return false;
        };
        match meta
            .artifact_fingerprint
            .as_deref()
            .or(meta.archive_sha256.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(installed) => installed != expected_fingerprint,
            None => true,
        }
    })
}

pub(super) fn install_target_from_status(
    status: &ctx_providers::adapters::ProviderStatus,
) -> Option<ctx_provider_install::install_state::InstallTarget> {
    status
        .details
        .get("install_target")
        .or_else(|| status.details.get("managed_target"))
        .and_then(|value| crate::parse_install_target(Some(value.as_str())).ok())
}

pub(super) async fn detect_managed_artifact_fingerprint_mismatch(
    cfg: &AgentServerConfigFile,
    entry: &ProviderMatrixEntry,
    status: &ctx_providers::adapters::ProviderStatus,
    detected_version: Option<&str>,
) -> Option<(String, String)> {
    if !status.installed {
        return None;
    }
    let requested_target = install_target_from_status(status)?;
    let meta = crate::managed_provider_install_metadata_for_target(
        cfg,
        &status.provider_id,
        Some(requested_target),
    )?;
    let version = detected_version.or(meta.version.as_deref())?;
    let expected_fingerprint =
        crate::expected_managed_provider_artifact_fingerprint(entry, version, requested_target)?;
    let detected_fingerprint = meta
        .artifact_fingerprint
        .as_deref()
        .or(meta.archive_sha256.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let archive_target_present = entry
        .managed_install
        .as_ref()
        .and_then(|install| {
            install.archive_target(crate::resolve_matrix_target_key(requested_target).ok()?)
        })
        .is_some();
    match detected_fingerprint {
        Some(detected)
            if expected_fingerprint.eq_ignore_ascii_case(detected) && archive_target_present =>
        {
            None
        }
        Some(detected) if expected_fingerprint == detected => None,
        Some(detected) => Some((expected_fingerprint, detected.to_string())),
        None => Some((expected_fingerprint, "<missing>".to_string())),
    }
}

pub(super) async fn detect_provider_version(
    data_root: &Path,
    cfg: &AgentServerConfigFile,
    entry: &ProviderMatrixEntry,
    status: &ctx_providers::adapters::ProviderStatus,
) -> Option<String> {
    if !status.installed {
        return None;
    }
    let requested_target = install_target_from_status(status);
    if let Some(meta) = crate::managed_provider_install_metadata_for_target(
        cfg,
        &status.provider_id,
        requested_target,
    ) {
        if let Some(version) = meta.version.clone() {
            return Some(version);
        }
    }

    let probe = entry.version_probe.as_ref()?;
    let command = match crate::resolve_runtime_provider_command_for_target(
        cfg,
        &status.provider_id,
        requested_target,
    ) {
        Ok(Some(command)) => ProviderCommand {
            command: command.command_abs_path,
            args: command.args,
        },
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!(
                provider_id = %status.provider_id,
                "skipping version probe: {err}"
            );
            return None;
        }
    };

    match probe {
        VersionProbe::Command { args } => probe_command_version(&command.command, args).await,
        VersionProbe::NodePackage { package } => {
            probe_node_package_version(&command, package, data_root)
        }
    }
}

pub(super) async fn probe_command_version(command: &str, args: &[String]) -> Option<String> {
    let mut cmd = Command::new(command);
    scrub_daemon_auth_env(&mut cmd);
    cmd.args(args)
        .kill_on_drop(true)
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0");

    let output = timeout(VERSION_PROBE_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    extract_version(&format!("{stdout}\n{stderr}"))
}

pub fn probe_node_package_version(
    command: &ProviderCommand,
    package: &str,
    data_root: &Path,
) -> Option<String> {
    let script_path = resolve_explicit_node_package_script_path(command)?;
    let resolved = std::fs::canonicalize(&script_path).unwrap_or(script_path);

    if resolved.starts_with(data_root) {
        if let Some(version) = find_package_version(&resolved, package) {
            return Some(version);
        }
    }

    find_package_version(&resolved, package)
}

fn find_package_version(path: &Path, package: &str) -> Option<String> {
    for ancestor in path.ancestors() {
        let direct = ancestor.join("package.json");
        if let Some(version) = read_package_version(&direct, package) {
            return Some(version);
        }
        let nested = ancestor
            .join("node_modules")
            .join(package)
            .join("package.json");
        if let Some(version) = read_package_version(&nested, package) {
            return Some(version);
        }
    }
    None
}

pub(super) fn read_package_version(path: &Path, package: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let name = json.get("name")?.as_str()?;
    if name != package {
        return None;
    }
    json.get("version")?.as_str().map(|s| s.to_string())
}

fn resolve_explicit_node_package_script_path(command: &ProviderCommand) -> Option<PathBuf> {
    let command_path = Path::new(&command.command);
    if command_path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("node"))
    {
        return command
            .args
            .first()
            .and_then(|arg| resolve_existing_absolute_path(arg));
    }
    resolve_existing_absolute_path(&command.command)
}

fn resolve_existing_absolute_path(raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw);
    (path.is_absolute() && path.exists()).then_some(path)
}

fn scrub_daemon_auth_env(cmd: &mut Command) {
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        cmd.env_remove(key);
    }
}
