use super::*;

mod node_entrypoints;
mod npm;
mod python;

pub use node_entrypoints::archive_bin_requires_node_runtime;
pub use npm::{
    npm_dependency_matches, npm_install, npm_install_one, resolve_node_package_bin,
    sanitize_npm_package_for_path,
};
#[cfg(test)]
pub(crate) use python::resolve_python_bin;
pub use python::{ensure_python_pip, ensure_python_runtime_versioned};
pub(crate) use python::{
    python_target_can_use_bundled_runtime, python_target_triple_for_install_target,
};

pub(crate) fn target_uses_windows_layout(target: InstallTarget) -> bool {
    match target {
        InstallTarget::Host => cfg!(windows),
        InstallTarget::Container | InstallTarget::LinuxAarch64 | InstallTarget::LinuxX8664 => false,
    }
}

pub(crate) fn venv_bin_dir(venv_dir: &Path, target: InstallTarget) -> PathBuf {
    if target_uses_windows_layout(target) {
        venv_dir.join("Scripts")
    } else {
        venv_dir.join("bin")
    }
}

pub fn venv_exe(venv_dir: &Path, name: &str, target: InstallTarget) -> PathBuf {
    let bin = venv_bin_dir(venv_dir, target);
    if target_uses_windows_layout(target) {
        bin.join(format!("{name}.exe"))
    } else {
        bin.join(name)
    }
}

pub struct NodeRuntime {
    pub node_root: PathBuf,
    pub node_bin: PathBuf,
    pub npm_cli_js: PathBuf,
    pub archive_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeRuntimeTarget {
    pub(crate) dist_target: &'static str,
    pub(crate) is_windows: bool,
}

#[derive(Debug, Clone)]
pub struct PythonRuntime {
    #[allow(dead_code)]
    pub python_root: PathBuf,
    pub python_bin: PathBuf,
    pub archive_sha256: Option<String>,
}

pub fn install_dir_rel(data_root: &Path, install_dir: &Path) -> String {
    install_dir
        .strip_prefix(data_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| install_dir.to_string_lossy().to_string())
}

fn install_target_dir_component(target: InstallTarget) -> Option<&'static str> {
    match target {
        InstallTarget::Host => None,
        InstallTarget::Container => Some("container"),
        InstallTarget::LinuxAarch64 => Some("linux-aarch64"),
        InstallTarget::LinuxX8664 => Some("linux-x86_64"),
    }
}

pub fn install_dir_for_provider(
    data_root: &Path,
    provider_id: &str,
    version: &str,
    target: InstallTarget,
) -> PathBuf {
    let mut out = data_root
        .join("providers")
        .join("agent-servers")
        .join(provider_id)
        .join(version);
    if let Some(component) = install_target_dir_component(target) {
        out = out.join(component);
    }
    out
}

pub async fn ensure_node_runtime(
    state: &ManagedInstallHostObject,
    install_id: Option<InstallId>,
    provider_id: &str,
    data_root: &Path,
    target: InstallTarget,
) -> Result<NodeRuntime> {
    let node_target = node_runtime_target_for_install_target(target)?;
    let target_label = node_target.dist_target;
    if matches!(target, InstallTarget::Host) {
        if let Some(bundled) = bundled_assets::bundled_node_runtime() {
            if bundled.version == NODE_VERSION {
                if let Some(npm_cli_js) = bundled.npm_cli.clone() {
                    emit_install(
                        state,
                        install_id,
                        provider_id,
                        InstallEventLevel::Info,
                        "node",
                        format!("Using bundled Node runtime v{NODE_VERSION} ({target_label})"),
                        None,
                        None,
                        None,
                    )
                    .await;
                    return Ok(NodeRuntime {
                        node_root: bundled.root,
                        node_bin: bundled.bin,
                        npm_cli_js,
                        archive_sha256: Some(bundled.sha256),
                    });
                }
            } else {
                tracing::warn!(
                    "bundled Node runtime version {} does not match expected {}",
                    bundled.version,
                    NODE_VERSION
                );
            }
        }
    }
    let archive_kind = if node_target.is_windows {
        runtime_lock::ManagedRuntimeArchiveKind::Zip
    } else {
        runtime_lock::ManagedRuntimeArchiveKind::TarGz
    };
    let archive = *runtime_lock::resolve_node_runtime_archive(
        NODE_VERSION,
        node_target.dist_target,
        archive_kind,
    )?;
    let extracted_folder = archive.expected_extract_root();
    let install_folder = archive.content_scoped_install_dir_name();
    let node_root = data_root
        .join("runtimes")
        .join("node")
        .join(&install_folder);
    let (node_bin, npm_cli_js) = node_runtime_paths(&node_root, node_target.is_windows);

    if node_bin.exists()
        && npm_cli_js.exists()
        && runtime_lock::runtime_ready_metadata_matches(&node_root, &archive).await
    {
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "node",
            format!("Using existing Node runtime v{NODE_VERSION} ({target_label})"),
            None,
            None,
            None,
        )
        .await;
        return Ok(NodeRuntime {
            node_root,
            node_bin,
            npm_cli_js,
            archive_sha256: Some(archive.sha256.to_string()),
        });
    }

    let _lock = node_runtime_install_lock().lock().await;

    if node_bin.exists()
        && npm_cli_js.exists()
        && runtime_lock::runtime_ready_metadata_matches(&node_root, &archive).await
    {
        emit_install(
            state,
            install_id,
            provider_id,
            InstallEventLevel::Info,
            "node",
            format!("Using existing Node runtime v{NODE_VERSION} ({target_label})"),
            None,
            None,
            None,
        )
        .await;
        return Ok(NodeRuntime {
            node_root,
            node_bin,
            npm_cli_js,
            archive_sha256: Some(archive.sha256.to_string()),
        });
    }
    if node_root.exists() {
        tokio::fs::remove_dir_all(&node_root).await.ok();
    }

    if let Some(parent) = node_root.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = data_root.join("runtimes").join("node").join(format!(
        "{}.sha256-{}.download",
        archive.archive_name,
        archive.sha256_prefix()
    ));
    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "node_download",
        "Downloading Node runtime from ctx mirror".to_string(),
        None,
        None,
        None,
    )
    .await;
    download_to_file_with_managed_runtime_redirects(
        state,
        install_id,
        provider_id,
        "node_download",
        archive.mirror_url,
        &tmp,
    )
    .await?;

    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "node_verify",
        "Verifying Node runtime checksum".to_string(),
        None,
        None,
        None,
    )
    .await;
    let digest = sha256_file(&tmp).await?;
    if let Err(error) = validate_sha256_digest(archive.sha256, &digest) {
        tokio::fs::remove_file(&tmp).await.ok();
        return Err(error);
    }

    let extract_root = data_root
        .join("runtimes")
        .join("node")
        .join(format!("{install_folder}.extract"));
    if extract_root.exists() {
        tokio::fs::remove_dir_all(&extract_root).await.ok();
    }
    tokio::fs::create_dir_all(&extract_root).await?;

    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Info,
        "node_extract",
        "Extracting Node runtime".to_string(),
        None,
        None,
        None,
    )
    .await;

    let tmp2 = tmp.clone();
    let extract_root2 = extract_root.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if archive_kind == runtime_lock::ManagedRuntimeArchiveKind::Zip {
            extract_zip_to_dir(&tmp2, &extract_root2)?;
        } else {
            extract_tar_gz_to_dir(&tmp2, &extract_root2)?;
        }
        Ok(())
    })
    .await??;

    let extracted = extract_root.join(&extracted_folder);
    if !extracted.exists() {
        anyhow::bail!(
            "node extraction failed: missing {extracted_folder} in {}",
            extract_root.display()
        );
    }
    let (extracted_node_bin, extracted_npm_cli_js) =
        node_runtime_paths(&extracted, node_target.is_windows);
    if !extracted_node_bin.exists() || !extracted_npm_cli_js.exists() {
        anyhow::bail!(
            "node runtime incomplete after extraction (node: {}, npm: {})",
            extracted_node_bin.display(),
            extracted_npm_cli_js.display()
        );
    }
    runtime_lock::write_runtime_ready_metadata(&extracted, &archive).await?;

    if node_root.exists() {
        tokio::fs::remove_dir_all(&node_root).await.ok();
    }
    tokio::fs::rename(&extracted, &node_root).await?;
    tokio::fs::remove_dir_all(&extract_root).await.ok();
    tokio::fs::remove_file(&tmp).await.ok();

    if !node_bin.exists() || !npm_cli_js.exists() {
        anyhow::bail!(
            "node runtime incomplete after install (node: {}, npm: {})",
            node_bin.display(),
            npm_cli_js.display()
        );
    }

    emit_install(
        state,
        install_id,
        provider_id,
        InstallEventLevel::Success,
        "node_extract",
        "Node runtime ready".to_string(),
        None,
        None,
        None,
    )
    .await;

    Ok(NodeRuntime {
        node_root,
        node_bin,
        npm_cli_js,
        archive_sha256: Some(archive.sha256.to_string()),
    })
}

fn node_runtime_paths(node_root: &Path, is_windows: bool) -> (PathBuf, PathBuf) {
    if is_windows {
        (
            node_root.join("node.exe"),
            node_root
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js"),
        )
    } else {
        (
            node_root.join("bin").join("node"),
            node_root
                .join("lib")
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js"),
        )
    }
}

pub(crate) fn node_runtime_target_for_install_target(
    target: InstallTarget,
) -> Result<NodeRuntimeTarget> {
    match target {
        InstallTarget::Host => {
            node_runtime_target_for_os_arch(std::env::consts::OS, std::env::consts::ARCH)
        }
        InstallTarget::Container => {
            node_runtime_target_for_os_arch("linux", std::env::consts::ARCH)
        }
        InstallTarget::LinuxAarch64 => node_runtime_target_for_os_arch("linux", "aarch64"),
        InstallTarget::LinuxX8664 => node_runtime_target_for_os_arch("linux", "x86_64"),
    }
}

pub fn node_runtime_dependency_targets_for_install_target(
    target: InstallTarget,
    host_os: &str,
) -> Vec<InstallTarget> {
    let mut targets = vec![target];
    if matches!(target, InstallTarget::Container) && host_os != "linux" {
        targets.push(InstallTarget::Host);
    }
    targets
}

fn node_runtime_target_for_os_arch(os: &str, arch: &str) -> Result<NodeRuntimeTarget> {
    match (os, arch) {
        ("macos", "aarch64") => Ok(NodeRuntimeTarget {
            dist_target: "darwin-arm64",
            is_windows: false,
        }),
        ("macos", "x86_64") => Ok(NodeRuntimeTarget {
            dist_target: "darwin-x64",
            is_windows: false,
        }),
        ("linux", "aarch64") => Ok(NodeRuntimeTarget {
            dist_target: "linux-arm64",
            is_windows: false,
        }),
        ("linux", "x86_64") => Ok(NodeRuntimeTarget {
            dist_target: "linux-x64",
            is_windows: false,
        }),
        ("windows", "x86_64") => Ok(NodeRuntimeTarget {
            dist_target: "win-x64",
            is_windows: true,
        }),
        ("windows", "aarch64") => Ok(NodeRuntimeTarget {
            dist_target: "win-arm64",
            is_windows: true,
        }),
        _ => anyhow::bail!(
            "unsupported platform for managed node install: {os}/{arch}. Supported: macos (aarch64/x86_64), linux (aarch64/x86_64), windows (aarch64/x86_64)."
        ),
    }
}

pub fn node_runtime_dependency_id(target: InstallTarget) -> String {
    format!("runtime-node-{}", target.as_str())
}

pub fn node_runtime_dependency_metadata(
    data_root: &Path,
    node: &NodeRuntime,
    target: InstallTarget,
) -> ManagedInstallMetadata {
    let bin_dir = node
        .node_bin
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| node.node_root.clone());
    ManagedInstallMetadata {
        package: Some("node-runtime".to_string()),
        version: Some(NODE_VERSION.to_string()),
        artifact_fingerprint: Some(
            node.archive_sha256
                .as_ref()
                .map(|sha256| format!("runtime:node:{NODE_VERSION}:sha256:{sha256}"))
                .unwrap_or_else(|| format!("runtime:node:{NODE_VERSION}")),
        ),
        archive_sha256: node.archive_sha256.clone(),
        target: Some(target),
        install_dir_rel: Some(install_dir_rel(data_root, &node.node_root)),
        bin_dir_rel: Some(install_dir_rel(data_root, &bin_dir)),
        last_success_at: Some(Utc::now().to_rfc3339()),
        last_error: None,
    }
}
