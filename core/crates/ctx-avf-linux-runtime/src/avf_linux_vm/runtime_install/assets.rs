use super::*;
use ctx_bundled_assets as bundled_assets;
use sha2::Digest;
use std::collections::BTreeMap;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ManagedAvfLinuxRuntimeReadyMetadata {
    schema_version: u32,
    runtime_id: String,
    source_identity_sha256: String,
    source_version: String,
    source_sha256: String,
    helper_sha256: BTreeMap<String, String>,
    installed_at_ms: u64,
}

impl ManagedAvfLinuxRuntimeReadyMetadata {
    const SCHEMA_VERSION: u32 = 1;
}

pub(super) fn emit_runtime_install_info(
    observer: Option<&dyn HarnessSetupObserver>,
    message: &str,
) {
    tracing::info!(component = "avf_runtime_install", "{message}");
    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        message,
    );
}

pub(crate) fn managed_avf_linux_guest_source() -> Option<bundled_assets::ManagedRuntimeSource> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(source) = lock_test_runtime_source_override().clone() {
        return Some(source);
    }
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    bundled_assets::managed_runtime_source(AVF_LINUX_GUEST_RUNTIME_ID, os, arch)
}

fn managed_artifact_extension(uri: &str) -> &'static str {
    let path = url::Url::parse(uri)
        .ok()
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|| uri.to_string());
    let path_lc = path.to_ascii_lowercase();
    if path_lc.ends_with(".tar.gz") {
        "tar.gz"
    } else if path_lc.ends_with(".zst") {
        "zst"
    } else if path_lc.ends_with(".tgz") {
        "tgz"
    } else if path_lc.ends_with(".tar") {
        "tar"
    } else {
        "zip"
    }
}

pub(in crate::avf_linux_vm) fn managed_avf_linux_archive_path(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> PathBuf {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let ext = managed_artifact_extension(&source.uri);
    data_root
        .join("managed")
        .join("downloads")
        .join(AVF_LINUX_GUEST_RUNTIME_ID)
        .join(os)
        .join(arch)
        .join(format!(
            "sha256-{}.{}",
            source.sha256.trim().to_ascii_lowercase(),
            ext
        ))
}

pub(in crate::avf_linux_vm) fn managed_avf_linux_runtime_source_identity(
    source: &bundled_assets::ManagedRuntimeSource,
) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-avf-linux-runtime-source-v1\n");
    hasher.update(source.version.trim().as_bytes());
    hasher.update(b"\n");
    hasher.update(source.uri.trim().as_bytes());
    hasher.update(b"\n");
    hasher.update(source.sha256.trim().to_ascii_lowercase().as_bytes());
    hasher.update(b"\n");
    hasher.update(source.bin.trim().as_bytes());
    let helpers = source.helpers.iter().collect::<BTreeMap<_, _>>();
    for (name, artifact) in helpers {
        hasher.update(b"\nhelper:");
        hasher.update(name.trim().as_bytes());
        hasher.update(b"\n");
        hasher.update(artifact.uri.trim().as_bytes());
        hasher.update(b"\n");
        hasher.update(artifact.sha256.trim().to_ascii_lowercase().as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub(super) fn managed_avf_linux_runtime_root(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> PathBuf {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let source_identity = managed_avf_linux_runtime_source_identity(source);
    data_root
        .join("managed")
        .join("runtimes")
        .join(AVF_LINUX_GUEST_RUNTIME_ID)
        .join(os)
        .join(arch)
        .join(format!(
            "{AVF_LINUX_GUEST_RUNTIME_ID}-{}-source-sha256-{}",
            source.version.trim(),
            source_identity,
        ))
}

pub(super) fn managed_avf_linux_helper_path(
    runtime_root: &Path,
    helper_name: &str,
) -> Option<PathBuf> {
    let helper = helper_name.trim();
    if helper.is_empty() {
        return None;
    }
    let file_name = if helper == AVF_LINUX_CONTAINER_STACK_HELPER {
        AVF_LINUX_CONTAINER_STACK_FILE
    } else {
        helper
    };
    Some(runtime_root.join("helpers").join(file_name))
}

pub(in crate::avf_linux_vm) fn managed_avf_linux_runtime_ready_marker_path(
    runtime_root: &Path,
) -> PathBuf {
    runtime_root.join(AVF_LINUX_RUNTIME_READY_MARKER)
}

pub(crate) fn avf_linux_runtime_is_ready(runtime: &AvfLinuxGuestRuntime) -> bool {
    let guest_agent_ready = runtime
        .guest_agent_path
        .as_ref()
        .is_some_and(|path| path.exists());
    let egress_proxy_ready = runtime
        .egress_proxy_path
        .as_ref()
        .is_some_and(|path| path.exists());
    let container_stack_ready = runtime.container_stack_path.exists();
    let managed_ready = if runtime.managed {
        managed_avf_linux_runtime_ready_metadata(&runtime.runtime_root).is_some_and(|meta| {
            let root_identity_matches = runtime
                .runtime_root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&meta.source_identity_sha256));
            meta.schema_version == ManagedAvfLinuxRuntimeReadyMetadata::SCHEMA_VERSION
                && meta.runtime_id == AVF_LINUX_GUEST_RUNTIME_ID
                && root_identity_matches
                && meta.source_version == runtime.version
                && !meta.source_identity_sha256.trim().is_empty()
                && !meta.source_sha256.trim().is_empty()
        })
    } else {
        true
    };
    runtime.rootfs_image.exists()
        && runtime.kernel_path.exists()
        && runtime.initrd_path.exists()
        && guest_agent_ready
        && egress_proxy_ready
        && container_stack_ready
        && managed_ready
}

pub(super) fn managed_avf_linux_install_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn managed_avf_linux_runtime_ready_metadata(
    runtime_root: &Path,
) -> Option<ManagedAvfLinuxRuntimeReadyMetadata> {
    let marker = managed_avf_linux_runtime_ready_marker_path(runtime_root);
    let bytes = std::fs::read(marker).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) async fn mark_managed_avf_linux_runtime_ready(
    runtime_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> Result<()> {
    let marker = managed_avf_linux_runtime_ready_marker_path(runtime_root);
    let helper_sha256 = source
        .helpers
        .iter()
        .map(|(name, artifact)| (name.clone(), artifact.sha256.clone()))
        .collect::<BTreeMap<_, _>>();
    let metadata = ManagedAvfLinuxRuntimeReadyMetadata {
        schema_version: ManagedAvfLinuxRuntimeReadyMetadata::SCHEMA_VERSION,
        runtime_id: AVF_LINUX_GUEST_RUNTIME_ID.to_string(),
        source_identity_sha256: managed_avf_linux_runtime_source_identity(source),
        source_version: source.version.trim().to_string(),
        source_sha256: source.sha256.trim().to_ascii_lowercase(),
        helper_sha256,
        installed_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0),
    };
    let bytes = serde_json::to_vec_pretty(&metadata).context("serializing AVF runtime metadata")?;
    fs::write(&marker, bytes)
        .await
        .with_context(|| format!("writing {}", marker.display()))
}
