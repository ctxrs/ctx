use super::*;

#[cfg(test)]
fn sandbox_machine_data_root_hash(data_root: &Path) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data_root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn sandbox_machine_runtime_root(
    data_root: &Path,
) -> PathBuf {
    let hash = sandbox_machine_data_root_hash(data_root);
    #[cfg(unix)]
    {
        PathBuf::from("/tmp").join("ctxp").join(hash)
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("ctxp").join(hash)
    }
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn sandbox_machine_home_root(data_root: &Path) -> PathBuf {
    sandbox_machine_runtime_root(data_root).join("home")
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn sandbox_machine_temp_root(data_root: &Path) -> PathBuf {
    sandbox_machine_runtime_root(data_root).join("tmp")
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn sandbox_machine_cache_root(
    data_root: &Path,
) -> PathBuf {
    data_root
        .join("sandbox-cli")
        .join("xdg")
        .join("data")
        .join("containers")
        .join("sandbox-cli")
        .join("machine")
}

#[cfg(test)]
pub(super) fn shared_sandbox_machine_cache_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(SANDBOX_MACHINE_CACHE_DIR_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    directories::BaseDirs::new().map(|base| {
        base.cache_dir()
            .join("ctx")
            .join("sandbox-machine")
            .join(std::env::consts::OS)
            .join(std::env::consts::ARCH)
    })
}

#[cfg(test)]
fn managed_sandbox_machine_cache_root(data_root: &Path) -> PathBuf {
    shared_sandbox_machine_cache_root()
        .unwrap_or_else(|| data_root.join("managed").join("machine-cache"))
}

#[cfg(test)]
fn managed_artifact_file_name(url: &str, sha256: &str, fallback_prefix: &str) -> String {
    let basename = Url::parse(url)
        .ok()
        .and_then(|parsed| {
            Path::new(parsed.path())
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{fallback_prefix}-{sha256}.bin"));
    format!("sha256-{}-{}", sha256.trim().to_ascii_lowercase(), basename)
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) fn managed_sandbox_machine_cache_path(
    data_root: &Path,
    source: &bundled_assets::ManagedArtifactSource,
) -> PathBuf {
    managed_sandbox_machine_cache_root(data_root)
        .join("managed")
        .join(managed_artifact_file_name(
            &source.uri,
            &source.sha256,
            SANDBOX_MACHINE_CACHE_ID,
        ))
}
