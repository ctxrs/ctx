use super::*;

pub fn parse_install_target(raw: Option<&str>) -> Result<InstallTarget> {
    let Some(raw) = raw else {
        return Ok(InstallTarget::Host);
    };
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "host" => Ok(InstallTarget::Host),
        "container" => Ok(InstallTarget::Container),
        "linux-aarch64" => Ok(InstallTarget::LinuxAarch64),
        "linux-x86_64" => Ok(InstallTarget::LinuxX8664),
        other => anyhow::bail!(
            "invalid install target '{other}'; expected host, container, linux-aarch64, or linux-x86_64"
        ),
    }
}

pub(crate) fn dependency_target_compatible_with_context(
    dependency_target: Option<InstallTarget>,
    container_exec: bool,
    host_os: &str,
    host_arch: &str,
) -> bool {
    match dependency_target.unwrap_or(InstallTarget::Host) {
        InstallTarget::Host => !container_exec,
        InstallTarget::Container => {
            if container_exec {
                matches!(host_arch, "x86_64" | "aarch64")
            } else {
                host_os == "linux" && matches!(host_arch, "x86_64" | "aarch64")
            }
        }
        InstallTarget::LinuxAarch64 => {
            host_arch == "aarch64" && (container_exec || host_os == "linux")
        }
        InstallTarget::LinuxX8664 => {
            host_arch == "x86_64" && (container_exec || host_os == "linux")
        }
    }
}

pub(crate) fn provider_env_targets_linux_sandbox(provider_env: &HashMap<String, String>) -> bool {
    provider_env
        .get(ctx_harness_runtime::CTX_HARNESS_LINUX_SANDBOX_ENV)
        .is_some_and(|value| value == "1")
        || provider_env.contains_key("CTX_HARNESS_CONTAINER_ID")
}

pub(crate) fn prepend_bundled_seed_node_bin_dir(
    bin_dirs: &mut Vec<PathBuf>,
    runtime_cmd: &ProviderRuntimeCommand,
    bundled_node_runtime: Option<bundled_assets::BundledRuntimePaths>,
) {
    if runtime_cmd.source != ProviderRuntimeCommandSource::BundledSeed {
        return;
    }
    let runtime_cmd_path = Path::new(&runtime_cmd.command_abs_path);
    if !archive_bin_requires_node_runtime(&runtime_cmd.command_abs_path, runtime_cmd_path) {
        return;
    }
    let Some(node_bin_dir) = bundled_node_runtime
        .as_ref()
        .and_then(|runtime| runtime.bin.parent())
        .map(Path::to_path_buf)
    else {
        return;
    };
    if !bin_dirs.contains(&node_bin_dir) {
        bin_dirs.push(node_bin_dir);
    }
}

pub fn prepend_runtime_bin_dirs_to_provider_path_for_target(
    provider_env: &mut HashMap<String, String>,
    cfg: &AgentServerConfigFile,
    runtime_provider_id: &str,
    data_root: &Path,
    requested_target: Option<InstallTarget>,
) {
    let mut bin_dirs: Vec<PathBuf> = Vec::new();
    let container_exec = provider_env_targets_linux_sandbox(provider_env);
    if let Ok(Some(runtime_cmd)) =
        resolve_runtime_provider_command_for_target(cfg, runtime_provider_id, requested_target)
    {
        let runtime_cmd_path = Path::new(&runtime_cmd.command_abs_path);
        if let Some(parent) = runtime_cmd_path.parent() {
            let parent_dir = parent.to_path_buf();
            if !bin_dirs.contains(&parent_dir) {
                bin_dirs.push(parent_dir);
            }
        }
        for dep in &runtime_cmd.dependencies {
            if let Some(meta) = cfg.managed_installs.get(dep) {
                if !dependency_target_compatible_with_context(
                    meta.target,
                    container_exec,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                ) {
                    continue;
                }
                if let Some(rel) = meta.bin_dir_rel.as_ref() {
                    let dep_dir = data_root.join(rel);
                    if !bin_dirs.contains(&dep_dir) {
                        bin_dirs.push(dep_dir);
                    }
                }
                continue;
            }
            if let Ok(Some(dep_runtime_cmd)) =
                resolve_runtime_provider_command_for_target(cfg, dep, requested_target)
            {
                let dep_runtime_path = Path::new(&dep_runtime_cmd.command_abs_path);
                if let Some(parent) = dep_runtime_path.parent() {
                    let dep_dir = parent.to_path_buf();
                    if !bin_dirs.contains(&dep_dir) {
                        bin_dirs.push(dep_dir);
                    }
                }
            }
        }
        prepend_bundled_seed_node_bin_dir(
            &mut bin_dirs,
            &runtime_cmd,
            bundled_assets::bundled_node_runtime(),
        );
    }
    if bin_dirs.is_empty() {
        return;
    }

    let mut path_parts: Vec<PathBuf> = bin_dirs;
    if let Some(current) = provider_env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
    {
        path_parts.extend(std::env::split_paths(std::ffi::OsStr::new(&current)));
    }
    if let Ok(joined) = std::env::join_paths(path_parts) {
        provider_env.insert("PATH".to_string(), joined.to_string_lossy().to_string());
    }
}

pub(crate) fn resolve_codex_cli_command_path_for_target(
    cfg: &AgentServerConfigFile,
    requested_target: Option<InstallTarget>,
) -> Result<Option<String>> {
    Ok(
        resolve_runtime_provider_command_for_target(cfg, "codex-cli", requested_target)?
            .map(|command| command.command_abs_path),
    )
}

fn normalize_explicit_command_path(env_name: &str, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{env_name} is set but empty"));
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        return Err(anyhow!(
            "{env_name} must be an absolute path, got `{trimmed}`"
        ));
    }
    if !path.exists() {
        return Err(anyhow!("{env_name} points to a missing path `{trimmed}`"));
    }
    Ok(std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string())
}

pub fn require_codex_cli_command_path_for_target(
    cfg: &AgentServerConfigFile,
    requested_target: Option<InstallTarget>,
) -> Result<String> {
    let target_label = requested_target
        .map(|target| target.as_str().to_string())
        .unwrap_or_else(|| "default".to_string());
    resolve_codex_cli_command_path_for_target(cfg, requested_target)?.ok_or_else(|| {
        anyhow!("explicit codex-cli runtime path is not configured for target `{target_label}`")
    })
}

pub fn ensure_codex_cli_command_env_for_target(
    provider_env: &mut HashMap<String, String>,
    cfg: &AgentServerConfigFile,
    runtime_provider_id: &str,
    requested_target: Option<InstallTarget>,
) -> Result<()> {
    if runtime_provider_id != ctx_core::provider_ids::CODEX_PROVIDER_ID {
        return Ok(());
    }
    if let Some(configured) = provider_env.get("CTX_CODEX_BIN_PATH") {
        let normalized = normalize_explicit_command_path("CTX_CODEX_BIN_PATH", configured)?;
        provider_env.insert("CTX_CODEX_BIN_PATH".to_string(), normalized);
        return Ok(());
    }
    let codex_bin = require_codex_cli_command_path_for_target(cfg, requested_target)?;
    provider_env.insert("CTX_CODEX_BIN_PATH".to_string(), codex_bin);
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn prepend_runtime_bin_dirs_to_provider_path(
    provider_env: &mut HashMap<String, String>,
    cfg: &AgentServerConfigFile,
    runtime_provider_id: &str,
    data_root: &Path,
) {
    prepend_runtime_bin_dirs_to_provider_path_for_target(
        provider_env,
        cfg,
        runtime_provider_id,
        data_root,
        None,
    )
}

pub fn resolve_matrix_target_key(target: InstallTarget) -> Result<&'static str> {
    match target {
        InstallTarget::Host => host_target_key(),
        InstallTarget::Container => container_target_key(),
        InstallTarget::LinuxAarch64 => Ok("linux-aarch64"),
        InstallTarget::LinuxX8664 => Ok("linux-x86_64"),
    }
}

pub fn is_supported_managed_provider_for_target(
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
) -> bool {
    if !is_supported_managed_provider(matrix, provider_id) {
        return false;
    }
    let Some(entry) = provider_matrix::get_entry(matrix, provider_id) else {
        return false;
    };
    let Some(install) = entry.managed_install.as_ref() else {
        return false;
    };
    let target_key = match resolve_matrix_target_key(target) {
        Ok(key) => key,
        Err(_) => return false,
    };

    match install {
        provider_matrix::ProviderInstall::Archive { targets, .. } => {
            targets.contains_key(target_key)
        }
        provider_matrix::ProviderInstall::Npm { targets, .. }
        | provider_matrix::ProviderInstall::Python { targets, .. } => match target {
            InstallTarget::Host => true,
            InstallTarget::Container | InstallTarget::LinuxAarch64 | InstallTarget::LinuxX8664 => {
                targets.contains_key(target_key)
            }
        },
    }
}

pub fn is_compatible_managed_provider_for_target(
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
    current_ctx_version: Option<&str>,
) -> bool {
    let Some(context_version) = current_ctx_version.and_then(provider_matrix::parse_version_loose)
    else {
        return false;
    };
    if !provider_matrix::is_managed_supported_for_context(
        matrix,
        provider_id,
        Some(&context_version),
    ) {
        return false;
    }
    is_supported_managed_provider_for_target(matrix, provider_id, target)
}

pub fn managed_install_download_size_bytes(
    matrix: &provider_matrix::ProviderMatrix,
    provider_id: &str,
    target: InstallTarget,
) -> Option<u64> {
    let entry = provider_matrix::get_entry(matrix, provider_id)?;
    let install = entry.managed_install.as_ref()?;
    let target_key = resolve_matrix_target_key(target).ok()?;

    let mut total: u64 = 0;
    let mut any = false;

    match install {
        provider_matrix::ProviderInstall::Archive { targets, .. } => {
            let target_entry = targets.get(target_key)?;
            let size = target_entry.size_bytes?;
            total = total.saturating_add(size);
            any = true;
        }
        provider_matrix::ProviderInstall::Npm { targets, .. }
        | provider_matrix::ProviderInstall::Python { targets, .. } => {
            if let Some(target_entry) = targets.get(target_key) {
                let size = target_entry.size_bytes?;
                total = total.saturating_add(size);
                any = true;
            }
        }
    }

    for dependency in &entry.dependencies {
        match &dependency.install {
            provider_matrix::DependencyInstall::Archive { targets, .. } => {
                let target_entry = targets.get(target_key)?;
                let size = target_entry.size_bytes?;
                total = total.saturating_add(size);
                any = true;
            }
            provider_matrix::DependencyInstall::Npm { .. } => {}
        }
    }

    if any {
        Some(total)
    } else {
        None
    }
}

pub fn apply_install_target_status(
    status: &mut ctx_providers::adapters::ProviderStatus,
    target: InstallTarget,
) {
    let requested_target = target.as_str();
    if matches!(target, InstallTarget::Host) {
        let Some(managed_target) = status.details.get("managed_target").cloned() else {
            return;
        };
        if managed_target == requested_target {
            status.details.remove("target_mismatch");
            status.details.remove("target_unverified");
            status.details.remove("target_mismatch_reason");
            return;
        }

        status.installed = false;
        status.health = ctx_providers::adapters::ProviderHealth::Missing;
        status
            .details
            .insert("target_mismatch".to_string(), "true".to_string());
        status.details.remove("target_unverified");
        status.details.insert(
            "target_mismatch_reason".to_string(),
            format!(
                "provider managed install target is '{managed_target}', requested '{requested_target}'"
            ),
        );
        let diagnostic = format!(
            "provider is installed for target '{managed_target}', not '{requested_target}'"
        );
        if !status.diagnostics.iter().any(|msg| msg == &diagnostic) {
            status.diagnostics.insert(0, diagnostic);
        }
        return;
    }

    let Some(managed_target) = status.details.get("managed_target").cloned() else {
        if !status.installed {
            return;
        }
        status.installed = false;
        status.health = ctx_providers::adapters::ProviderHealth::Missing;
        status.details.remove("target_mismatch");
        status
            .details
            .insert("target_unverified".to_string(), "true".to_string());
        status.details.insert(
            "target_mismatch_reason".to_string(),
            format!(
                "provider status was detected from the host environment and cannot verify target '{requested_target}'"
            ),
        );
        let diagnostic = format!(
            "provider status reflects the host environment and does not verify target '{requested_target}'"
        );
        if !status.diagnostics.iter().any(|msg| msg == &diagnostic) {
            status.diagnostics.insert(0, diagnostic);
        }
        return;
    };
    if managed_target == requested_target {
        status.details.remove("target_mismatch");
        status.details.remove("target_unverified");
        status.details.remove("target_mismatch_reason");
        return;
    }

    status.installed = false;
    status.health = ctx_providers::adapters::ProviderHealth::Missing;
    status
        .details
        .insert("target_mismatch".to_string(), "true".to_string());
    status.details.remove("target_unverified");
    status.details.insert(
        "target_mismatch_reason".to_string(),
        format!(
            "provider managed install target is '{managed_target}', requested '{requested_target}'"
        ),
    );
    let diagnostic =
        format!("provider is installed for target '{managed_target}', not '{requested_target}'");
    if !status.diagnostics.iter().any(|msg| msg == &diagnostic) {
        status.diagnostics.insert(0, diagnostic);
    }
}
