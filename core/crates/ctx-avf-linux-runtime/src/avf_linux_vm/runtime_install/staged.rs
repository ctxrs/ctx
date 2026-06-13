use super::*;

impl AvfLinuxGuestRuntime {
    pub(crate) fn from_source(
        data_root: &Path,
        source: &bundled_assets::ManagedRuntimeSource,
    ) -> Result<Self> {
        let runtime_root = managed_avf_linux_runtime_root(data_root, source);
        let rootfs_image = runtime_root.join(source.bin.trim());
        let kernel_path = managed_avf_linux_helper_path(&runtime_root, AVF_LINUX_KERNEL_HELPER)
            .ok_or_else(|| {
                anyhow::anyhow!("managed AVF Linux runtime is missing a kernel helper")
            })?;
        let initrd_path = managed_avf_linux_helper_path(&runtime_root, AVF_LINUX_INITRD_HELPER)
            .ok_or_else(|| {
                anyhow::anyhow!("managed AVF Linux runtime is missing an initrd helper")
            })?;
        let guest_agent_path =
            managed_avf_linux_helper_path(&runtime_root, AVF_LINUX_GUEST_AGENT_HELPER);
        let egress_proxy_path =
            managed_avf_linux_helper_path(&runtime_root, AVF_LINUX_EGRESS_PROXY_HELPER);
        let container_stack_path =
            managed_avf_linux_helper_path(&runtime_root, AVF_LINUX_CONTAINER_STACK_HELPER)
                .ok_or_else(|| {
                    anyhow::anyhow!("managed AVF Linux runtime is missing a guest container stack")
                })?;
        Ok(Self {
            runtime_root,
            rootfs_image,
            kernel_path,
            initrd_path,
            guest_agent_path,
            egress_proxy_path,
            container_stack_path,
            version: source.version.trim().to_string(),
            managed: true,
        })
    }

    fn from_runtime_root(runtime_root: PathBuf, version: String, managed: bool) -> Self {
        Self {
            kernel_path: runtime_root.join("helpers").join(AVF_LINUX_KERNEL_HELPER),
            initrd_path: runtime_root.join("helpers").join(AVF_LINUX_INITRD_HELPER),
            guest_agent_path: {
                let path = runtime_root
                    .join("helpers")
                    .join(AVF_LINUX_GUEST_AGENT_HELPER);
                path.exists().then_some(path)
            },
            egress_proxy_path: {
                let path = runtime_root
                    .join("helpers")
                    .join(AVF_LINUX_EGRESS_PROXY_HELPER);
                path.exists().then_some(path)
            },
            container_stack_path: runtime_root
                .join("helpers")
                .join(AVF_LINUX_CONTAINER_STACK_FILE),
            rootfs_image: runtime_root.join("rootfs.raw"),
            runtime_root,
            version,
            managed,
        }
    }

    fn from_bundled(paths: bundled_assets::BundledRuntimePaths) -> Self {
        let mut runtime = Self::from_runtime_root(paths.root, paths.version, false);
        runtime.rootfs_image = paths.bin;
        runtime
    }
}

pub(super) fn explicit_staged_avf_linux_guest_runtime_dir() -> Option<PathBuf> {
    let raw = std::env::var(AVF_LINUX_GUEST_RUNTIME_DIR_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn staged_runtime_version(runtime_root: &Path) -> String {
    let version_path = runtime_root.join("version.txt");
    std::fs::read_to_string(&version_path)
        .ok()
        .and_then(|contents| {
            let lines = contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            if lines.is_empty() {
                return None;
            }
            let version = lines
                .iter()
                .find_map(|line| line.strip_prefix("version=").map(str::trim))
                .filter(|line| !line.is_empty())
                .unwrap_or(lines[0]);
            Some(version.to_string())
        })
        .unwrap_or_else(|| "staged".to_string())
}

pub(crate) fn staged_avf_linux_guest_runtime() -> Result<Option<AvfLinuxGuestRuntime>> {
    let Some(runtime_root) = explicit_staged_avf_linux_guest_runtime_dir() else {
        return Ok(None);
    };
    if !runtime_root.exists() {
        bail!(
            "explicit staged AVF Linux guest runtime dir does not exist: {}",
            runtime_root.display()
        );
    }
    let version = staged_runtime_version(&runtime_root);
    let runtime = AvfLinuxGuestRuntime::from_runtime_root(runtime_root, version, false);
    if avf_linux_runtime_is_ready(&runtime) {
        return Ok(Some(runtime));
    }
    bail!(
        "explicit staged AVF Linux guest runtime dir is incomplete or not ready: {}",
        runtime.runtime_root.display()
    );
}

pub(crate) fn bundled_avf_linux_guest_runtime() -> Option<AvfLinuxGuestRuntime> {
    let runtime = bundled_assets::bundled_avf_linux_guest_runtime()?;
    let runtime = AvfLinuxGuestRuntime::from_bundled(runtime);
    if avf_linux_runtime_is_ready(&runtime) {
        Some(runtime)
    } else {
        None
    }
}
