use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const BUNDLE_ENV_DIR: &str = "CTX_BUNDLE_DIR";
pub(crate) const BUNDLE_ENV_MANIFEST: &str = "CTX_BUNDLE_MANIFEST";
pub(crate) const MANIFEST_FILENAME: &str = "manifest.json";
const EFFECTIVE_MANIFEST_FILENAME: &str = "runtime_manifest.effective.json";
pub(crate) const MANIFEST_VERSION: u32 = 1;
const RUNTIME_LOCK_FILENAME: &str = "runtime_lock.v2.json";
pub(crate) const RUNTIME_LOCK_VERSION: u32 = 2;
pub(crate) const AVF_LINUX_GUEST_RUNTIME_ID: &str = "avf-linux-guest";
pub(crate) const AVF_REQUIRED_HELPERS: &[&str] = &[
    "kernel",
    "initrd",
    "guest-agent",
    "egress-proxy",
    "container-stack",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledAssetsManifest {
    pub version: u32,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub providers: Vec<BundledProvider>,
    #[serde(default)]
    pub runtimes: Vec<BundledRuntime>,
    #[serde(default)]
    pub images: Vec<BundledImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledProvider {
    pub id: String,
    pub protocol: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub sha256: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledRuntime {
    pub id: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub sha256: String,
    pub root: String,
    pub bin: String,
    #[serde(default)]
    pub npm_cli: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledImage {
    pub id: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub sha256: String,
    pub tar: String,
    pub image: String,
}

#[derive(Debug, Clone)]
pub struct BundledCommand {
    pub command: String,
    pub args: Vec<String>,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct BundledRuntimePaths {
    pub root: PathBuf,
    pub bin: PathBuf,
    pub npm_cli: Option<PathBuf>,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ManagedArtifactSource {
    pub uri: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ManagedRuntimeSource {
    pub uri: String,
    pub sha256: String,
    pub version: String,
    pub bin: String,
    pub helpers: HashMap<String, ManagedArtifactSource>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeLockProfile {
    #[serde(default)]
    pub(crate) allowed_source_types: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeLockSource {
    pub(crate) source_type: String,
    #[serde(default)]
    pub(crate) uri: Option<String>,
    #[serde(default)]
    pub(crate) sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeLockHelperSource {
    #[serde(default)]
    pub(crate) uri: Option<String>,
    #[serde(default)]
    pub(crate) sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeLockComponent {
    pub(crate) kind: String,
    pub(crate) id: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    #[serde(default)]
    pub(crate) variant: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) bin: Option<String>,
    #[serde(default)]
    pub(crate) helpers: HashMap<String, RuntimeLockHelperSource>,
    #[serde(default)]
    pub(crate) sources: Vec<RuntimeLockSource>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuntimeLockV2 {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) profiles: HashMap<String, RuntimeLockProfile>,
    #[serde(default)]
    pub(crate) components: Vec<RuntimeLockComponent>,
}

pub(crate) fn active_bundle_dir() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some((root, _)) = super::lock_test_manifest_override().clone() {
        return Some(root);
    }

    let raw = std::env::var(BUNDLE_ENV_DIR).ok()?;
    let path = PathBuf::from(raw.trim());
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }
    Some(path)
}

pub(crate) fn manifest_path(root: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var(BUNDLE_ENV_MANIFEST) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let candidate = PathBuf::from(trimmed);
            if candidate.is_absolute() {
                return candidate;
            }
            return root.join(candidate);
        }
    }
    let effective_manifest = root.join(EFFECTIVE_MANIFEST_FILENAME);
    if effective_manifest.exists() {
        return effective_manifest;
    }
    root.join(MANIFEST_FILENAME)
}

pub(crate) fn runtime_lock_path(root: &Path) -> PathBuf {
    let manifest = manifest_path(root);
    let sibling = manifest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf())
        .join(RUNTIME_LOCK_FILENAME);
    if sibling.exists() {
        return sibling;
    }
    root.join(RUNTIME_LOCK_FILENAME)
}

pub(crate) fn current_platform() -> (&'static str, &'static str) {
    (std::env::consts::OS, std::env::consts::ARCH)
}

pub(crate) fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

pub(crate) fn resolve_bundle_path(root: &Path, value: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(value);
    if candidate.is_absolute() {
        return if candidate.exists() {
            Some(candidate)
        } else {
            None
        };
    }

    let direct = root.join(value);
    if direct.exists() {
        return Some(direct);
    }

    let bin = root.join("bin").join(value);
    if bin.exists() {
        return Some(bin);
    }

    None
}

pub(crate) fn resolve_bundle_args(root: &Path, args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| {
            if arg.starts_with('-') {
                return arg.clone();
            }
            if Path::new(arg).is_absolute() {
                return arg.clone();
            }
            if let Some(resolved) = resolve_bundle_path(root, arg) {
                return resolved.to_string_lossy().to_string();
            }
            arg.clone()
        })
        .collect()
}

pub(crate) fn read_manifest_from_root(root: &Path) -> Option<BundledAssetsManifest> {
    let path = manifest_path(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: BundledAssetsManifest = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(
                "failed to parse bundled assets manifest {}: {err}",
                path.display()
            );
            return None;
        }
    };
    if parsed.version != MANIFEST_VERSION {
        tracing::warn!(
            "unsupported bundled assets manifest version {} (expected {})",
            parsed.version,
            MANIFEST_VERSION
        );
        return None;
    }
    Some(parsed)
}

pub(crate) fn read_runtime_lock_from_root(root: &Path) -> Option<RuntimeLockV2> {
    let path = runtime_lock_path(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: RuntimeLockV2 = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!("failed to parse runtime lock {}: {err}", path.display());
            return None;
        }
    };
    if parsed.version != RUNTIME_LOCK_VERSION {
        tracing::warn!(
            "unsupported runtime lock version {} (expected {})",
            parsed.version,
            RUNTIME_LOCK_VERSION
        );
        return None;
    }
    Some(parsed)
}
