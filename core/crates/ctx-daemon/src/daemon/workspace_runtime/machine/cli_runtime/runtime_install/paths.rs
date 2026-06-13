use super::*;

pub(super) fn managed_sandbox_cli_runtime_source() -> Option<bundled_assets::ManagedRuntimeSource> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    bundled_assets::managed_runtime_source("sandbox-cli", os, arch)
}

pub(super) fn managed_sandbox_cli_runtime_bin_path(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> PathBuf {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    data_root
        .join("managed")
        .join("runtimes")
        .join("sandbox-cli")
        .join(os)
        .join(arch)
        .join(format!("sandbox-cli-{}", source.version.trim()))
        .join(source.bin.trim())
}

pub(super) fn managed_sandbox_cli_runtime_root(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> PathBuf {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    data_root
        .join("managed")
        .join("runtimes")
        .join("sandbox-cli")
        .join(os)
        .join(arch)
        .join(format!("sandbox-cli-{}", source.version.trim()))
}

pub(super) fn managed_sandbox_cli_helper_path(
    runtime_root: &Path,
    helper_name: &str,
) -> Option<PathBuf> {
    let helper = helper_name.trim();
    if helper.is_empty() {
        return None;
    }
    Some(
        runtime_root
            .join("usr")
            .join("libexec")
            .join("sandbox-cli")
            .join(helper),
    )
}

pub(super) fn managed_sandbox_cli_install_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn managed_sandbox_cli_runtime_ready_marker_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join(".ctx-managed-ready")
}

pub(super) fn managed_sandbox_cli_runtime_is_ready(
    runtime_root: &Path,
    runtime_bin: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> bool {
    if !runtime_bin.exists()
        || !managed_sandbox_cli_runtime_ready_marker_path(runtime_root).exists()
    {
        return false;
    }
    source.helpers.keys().all(|name| {
        managed_sandbox_cli_helper_path(runtime_root, name)
            .map(|path| path.exists())
            .unwrap_or(true)
    })
}

pub(super) async fn mark_managed_sandbox_cli_runtime_ready(runtime_root: &Path) -> Result<()> {
    let marker = managed_sandbox_cli_runtime_ready_marker_path(runtime_root);
    fs::write(&marker, b"ready")
        .await
        .with_context(|| format!("writing {}", marker.display()))
}
