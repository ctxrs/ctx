use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard};

mod manifest;

use self::manifest::{
    active_bundle_dir as bundle_dir, current_arch, current_platform, read_manifest_from_root,
    read_runtime_lock_from_root, resolve_bundle_args, resolve_bundle_path, RuntimeLockComponent,
    RuntimeLockV2, AVF_LINUX_GUEST_RUNTIME_ID, AVF_REQUIRED_HELPERS,
};
#[cfg(test)]
use self::manifest::{
    manifest_path, RuntimeLockHelperSource, RuntimeLockSource, BUNDLE_ENV_MANIFEST,
    MANIFEST_FILENAME, MANIFEST_VERSION,
};
pub use self::manifest::{
    BundledAssetsManifest, BundledCommand, BundledImage, BundledProvider, BundledRuntime,
    BundledRuntimePaths, ManagedArtifactSource, ManagedRuntimeSource,
};

#[cfg(any(test, feature = "test-support"))]
fn load_manifest_for_tests() -> Option<BundledAssetsManifest> {
    if let Some((_, manifest)) = lock_test_manifest_override().clone() {
        return Some(manifest);
    }

    let root = bundle_dir()?;
    read_manifest_from_root(&root)
}

fn load_manifest() -> Option<BundledAssetsManifest> {
    #[cfg(any(test, feature = "test-support"))]
    {
        load_manifest_for_tests()
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        static MANIFEST: OnceLock<Option<BundledAssetsManifest>> = OnceLock::new();
        let res = MANIFEST.get_or_init(|| {
            let root = bundle_dir()?;
            read_manifest_from_root(&root)
        });
        res.clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
fn load_runtime_lock_for_tests() -> Option<RuntimeLockV2> {
    let root = bundle_dir()?;
    read_runtime_lock_from_root(&root)
}

fn load_runtime_lock() -> Option<RuntimeLockV2> {
    #[cfg(any(test, feature = "test-support"))]
    {
        load_runtime_lock_for_tests()
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        static LOCK: OnceLock<Option<RuntimeLockV2>> = OnceLock::new();
        let res = LOCK.get_or_init(|| {
            let root = bundle_dir()?;
            read_runtime_lock_from_root(&root)
        });
        res.clone()
    }
}

fn active_runtime_profile() -> &'static str {
    let raw = std::env::var("CTX_RUNTIME_PROFILE").unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "parity" => "parity",
        "override" => "override",
        "source-all" => "source-all",
        _ => "parity",
    }
}

fn allowed_source_types_for_profile(lock: &RuntimeLockV2) -> HashSet<String> {
    let mut out = HashSet::<String>::new();
    let profile = active_runtime_profile();
    let cfg = lock
        .profiles
        .get(profile)
        .or_else(|| lock.profiles.get("parity"));
    if let Some(cfg) = cfg {
        for source_type in &cfg.allowed_source_types {
            let trimmed = source_type.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.insert(trimmed.to_ascii_lowercase());
        }
    }
    out
}

fn select_managed_source(
    component: &RuntimeLockComponent,
    allowed_source_types: &HashSet<String>,
) -> Option<ManagedArtifactSource> {
    component.sources.iter().find_map(|source| {
        let source_type = source.source_type.trim();
        if source_type.is_empty() || source_type.eq_ignore_ascii_case("local") {
            return None;
        }
        if !allowed_source_types.is_empty()
            && !allowed_source_types.contains(&source_type.to_ascii_lowercase())
        {
            return None;
        }
        let uri = source.uri.as_ref()?.trim();
        let sha256 = source.sha256.as_ref()?.trim();
        if uri.is_empty() || sha256.is_empty() {
            return None;
        }
        if component.kind == "runtime"
            && component.id == AVF_LINUX_GUEST_RUNTIME_ID
            && (!managed_source_is_resolved(uri) || !sha256_is_resolved(sha256))
        {
            return None;
        }
        Some(ManagedArtifactSource {
            uri: uri.to_string(),
            sha256: sha256.to_string(),
        })
    })
}

fn select_managed_runtime_source(
    component: &RuntimeLockComponent,
    allowed_source_types: &HashSet<String>,
) -> Option<ManagedRuntimeSource> {
    let source = select_managed_source(component, allowed_source_types)?;
    let version = component.version.as_deref()?.trim();
    if version.is_empty() {
        return None;
    }
    let bin = component.bin.as_deref()?.trim();
    if bin.is_empty() {
        return None;
    }
    let mut helpers = HashMap::new();
    for (name, helper) in &component.helpers {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let uri = helper.uri.as_deref().unwrap_or("").trim();
        let sha256 = helper.sha256.as_deref().unwrap_or("").trim();
        if uri.is_empty() || sha256.is_empty() {
            continue;
        }
        if component.id == AVF_LINUX_GUEST_RUNTIME_ID
            && (!managed_source_is_resolved(uri) || !sha256_is_resolved(sha256))
        {
            continue;
        }
        helpers.insert(
            name.to_string(),
            ManagedArtifactSource {
                uri: uri.to_string(),
                sha256: sha256.to_string(),
            },
        );
    }
    if component.id == AVF_LINUX_GUEST_RUNTIME_ID
        && AVF_REQUIRED_HELPERS
            .iter()
            .any(|helper| !helpers.contains_key(*helper))
    {
        return None;
    }
    Some(ManagedRuntimeSource {
        uri: source.uri,
        sha256: source.sha256,
        version: version.to_string(),
        bin: bin.to_string(),
        helpers,
    })
}

fn managed_source_is_resolved(uri: &str) -> bool {
    !uri.trim().starts_with("locked://")
}

fn sha256_is_resolved(sha256: &str) -> bool {
    let trimmed = sha256.trim();
    !(trimmed.is_empty() || (trimmed.len() == 64 && trimmed.bytes().all(|byte| byte == b'0')))
}

pub fn bundled_provider_command(provider_id: &str) -> Option<BundledCommand> {
    let root = bundle_dir()?;
    let manifest = load_manifest()?;
    let (os, arch) = current_platform();
    let entry = manifest
        .providers
        .iter()
        .find(|p| p.id == provider_id && p.os == os && p.arch == arch)?;
    let command = resolve_bundle_path(&root, &entry.command)?;
    let args = resolve_bundle_args(&root, &entry.args);
    Some(BundledCommand {
        command: command.to_string_lossy().to_string(),
        args,
        version: entry.version.clone(),
    })
}

fn bundled_runtime_from_manifest_for_target(
    root: &Path,
    manifest: &BundledAssetsManifest,
    id: &str,
    version: Option<&str>,
    os: &str,
    arch: &str,
) -> Option<BundledRuntimePaths> {
    let entry = manifest.runtimes.iter().find(|r| {
        r.id == id
            && r.os == os
            && r.arch == arch
            && version.is_none_or(|expected| r.version == expected)
    })?;
    let runtime_root =
        resolve_bundle_path(root, &entry.root).unwrap_or_else(|| root.join(&entry.root));
    if !runtime_root.exists() {
        return None;
    }
    let bin = if Path::new(&entry.bin).is_absolute() {
        PathBuf::from(&entry.bin)
    } else {
        runtime_root.join(&entry.bin)
    };
    if !bin.exists() {
        return None;
    }
    let npm_cli = entry.npm_cli.as_ref().map(|raw| {
        if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            runtime_root.join(raw)
        }
    });
    if let Some(path) = npm_cli.as_ref() {
        if !path.exists() {
            return None;
        }
    }
    Some(BundledRuntimePaths {
        root: runtime_root,
        bin,
        npm_cli,
        version: entry.version.clone(),
        sha256: entry.sha256.clone(),
    })
}

fn bundled_runtime(id: &str) -> Option<BundledRuntimePaths> {
    let root = bundle_dir()?;
    let manifest = load_manifest()?;
    let (os, arch) = current_platform();
    bundled_runtime_from_manifest_for_target(&root, &manifest, id, None, os, arch)
}

pub fn bundled_runtime_for(id: &str, os: &str, arch: &str) -> Option<BundledRuntimePaths> {
    let root = bundle_dir()?;
    let manifest = load_manifest()?;
    bundled_runtime_from_manifest_for_target(&root, &manifest, id, None, os, arch)
}

pub fn bundled_node_runtime() -> Option<BundledRuntimePaths> {
    bundled_runtime("node")
}

pub fn bundled_python_runtime() -> Option<BundledRuntimePaths> {
    bundled_runtime("python")
}

pub fn bundled_python_runtime_version(version: &str) -> Option<BundledRuntimePaths> {
    let root = bundle_dir()?;
    let manifest = load_manifest()?;
    let (os, arch) = current_platform();
    bundled_runtime_from_manifest_for_target(&root, &manifest, "python", Some(version), os, arch)
}

#[cfg(any(test, feature = "test-support"))]
pub fn bundled_sandbox_cli_runtime() -> Option<BundledRuntimePaths> {
    bundled_runtime("sandbox-cli")
}

pub fn bundled_avf_linux_guest_runtime() -> Option<BundledRuntimePaths> {
    bundled_runtime("avf-linux-guest")
}

pub fn bundled_image_tar(id: &str, os: &str, arch: &str) -> Option<PathBuf> {
    let root = bundle_dir()?;
    let manifest = load_manifest()?;
    let entry = manifest
        .images
        .iter()
        .find(|img| img.id == id && img.os == os && img.arch == arch)?;
    resolve_bundle_path(&root, &entry.tar).or_else(|| {
        let direct = root.join(&entry.tar);
        direct.exists().then_some(direct)
    })
}

pub fn bundled_ctx_harness_image_tar(expected_image: &str) -> Option<PathBuf> {
    let manifest = load_manifest()?;
    let arch = current_arch();
    let entry = manifest.images.iter().find(|img| {
        img.id == "ctx-harness"
            && img.os == "linux"
            && img.arch == arch
            && img.image == expected_image
    })?;
    let root = bundle_dir()?;
    resolve_bundle_path(&root, &entry.tar).or_else(|| {
        let direct = root.join(&entry.tar);
        direct.exists().then_some(direct)
    })
}

pub fn managed_image_source(id: &str, os: &str, arch: &str) -> Option<ManagedArtifactSource> {
    let lock = load_runtime_lock()?;
    let allowed_source_types = allowed_source_types_for_profile(&lock);
    let component = lock.components.iter().find(|component| {
        let variant = component
            .variant
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        component.kind == "image"
            && component.id == id
            && component.os == os
            && component.arch == arch
            && variant == "default"
    })?;
    select_managed_source(component, &allowed_source_types)
}

pub fn managed_machine_cache_source(
    id: &str,
    os: &str,
    arch: &str,
) -> Option<ManagedArtifactSource> {
    let lock = load_runtime_lock()?;
    let allowed_source_types = allowed_source_types_for_profile(&lock);
    let component = lock.components.iter().find(|component| {
        let variant = component
            .variant
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        component.kind == "machine_cache"
            && component.id == id
            && component.os == os
            && component.arch == arch
            && variant == "default"
    })?;
    select_managed_source(component, &allowed_source_types)
}

pub fn managed_runtime_source(id: &str, os: &str, arch: &str) -> Option<ManagedRuntimeSource> {
    let lock = load_runtime_lock()?;
    let allowed_source_types = allowed_source_types_for_profile(&lock);
    let component = lock.components.iter().find(|component| {
        let variant = component
            .variant
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default");
        component.kind == "runtime"
            && component.id == id
            && component.os == os
            && component.arch == arch
            && variant == "default"
    })?;
    select_managed_runtime_source(component, &allowed_source_types)
}

pub fn managed_ctx_harness_image_source(_expected_image: &str) -> Option<ManagedArtifactSource> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(source) = lock_test_managed_ctx_harness_image_source_override().clone() {
        return Some(source);
    }

    managed_image_source("ctx-harness", "linux", current_arch())
}

#[cfg(any(test, feature = "test-support"))]
pub fn managed_sandbox_machine_cache_source() -> Option<ManagedArtifactSource> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(source) = lock_test_managed_sandbox_machine_cache_source_override().clone() {
        return Some(source);
    }

    managed_machine_cache_source(
        "sandbox-machine",
        current_platform().0,
        current_platform().1,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn test_managed_sandbox_machine_cache_source_override(
) -> &'static Mutex<Option<ManagedArtifactSource>> {
    static OVERRIDE: OnceLock<Mutex<Option<ManagedArtifactSource>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

#[cfg(any(test, feature = "test-support"))]
fn test_managed_ctx_harness_image_source_override() -> &'static Mutex<Option<ManagedArtifactSource>>
{
    static OVERRIDE: OnceLock<Mutex<Option<ManagedArtifactSource>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

#[cfg(any(test, feature = "test-support"))]
fn test_manifest_override() -> &'static Mutex<Option<(PathBuf, BundledAssetsManifest)>> {
    static OVERRIDE: OnceLock<Mutex<Option<(PathBuf, BundledAssetsManifest)>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

#[cfg(any(test, feature = "test-support"))]
fn lock_test_managed_sandbox_machine_cache_source_override(
) -> MutexGuard<'static, Option<ManagedArtifactSource>> {
    test_managed_sandbox_machine_cache_source_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-support"))]
fn lock_test_managed_ctx_harness_image_source_override(
) -> MutexGuard<'static, Option<ManagedArtifactSource>> {
    test_managed_ctx_harness_image_source_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-support"))]
fn lock_test_manifest_override() -> MutexGuard<'static, Option<(PathBuf, BundledAssetsManifest)>> {
    test_manifest_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-support"))]
pub fn bundled_assets_manifest_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestManagedSandboxMachineCacheSourceGuard {
    previous: Option<ManagedArtifactSource>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestManagedSandboxMachineCacheSourceGuard {
    fn drop(&mut self) {
        let mut guard = lock_test_managed_sandbox_machine_cache_source_override();
        *guard = self.previous.take();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestManagedCtxHarnessImageSourceGuard {
    previous: Option<ManagedArtifactSource>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestManagedCtxHarnessImageSourceGuard {
    fn drop(&mut self) {
        let mut guard = lock_test_managed_ctx_harness_image_source_override();
        *guard = self.previous.take();
    }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub struct TestBundledAssetsManifestGuard {
    previous: Option<(PathBuf, BundledAssetsManifest)>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestBundledAssetsManifestGuard {
    fn drop(&mut self) {
        let mut guard = lock_test_manifest_override();
        *guard = self.previous.take();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn override_managed_sandbox_machine_cache_source_for_test(
    source: ManagedArtifactSource,
) -> TestManagedSandboxMachineCacheSourceGuard {
    let mut guard = lock_test_managed_sandbox_machine_cache_source_override();
    let previous = guard.clone();
    *guard = Some(source);
    TestManagedSandboxMachineCacheSourceGuard { previous }
}

#[cfg(any(test, feature = "test-support"))]
pub fn override_managed_ctx_harness_image_source_for_test(
    source: ManagedArtifactSource,
) -> TestManagedCtxHarnessImageSourceGuard {
    let mut guard = lock_test_managed_ctx_harness_image_source_override();
    let previous = guard.clone();
    *guard = Some(source);
    TestManagedCtxHarnessImageSourceGuard { previous }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub fn override_bundled_assets_manifest_for_test(
    root: PathBuf,
    manifest: BundledAssetsManifest,
) -> TestBundledAssetsManifestGuard {
    let mut guard = lock_test_manifest_override();
    let previous = guard.clone();
    *guard = Some((root, manifest));
    TestBundledAssetsManifestGuard { previous }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    pub use super::{
        bundled_assets_manifest_test_lock, override_bundled_assets_manifest_for_test,
        override_managed_ctx_harness_image_source_for_test,
        override_managed_sandbox_machine_cache_source_for_test, BundledAssetsManifest,
        ManagedArtifactSource, TestBundledAssetsManifestGuard,
        TestManagedCtxHarnessImageSourceGuard, TestManagedSandboxMachineCacheSourceGuard,
    };
}

#[cfg(test)]
mod tests;
