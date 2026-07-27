use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{
    platform_security::{
        create_private_directory_all, restrict_private_directory, restrict_private_file,
        verify_private_directory,
    },
    utc_now,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::semantic::{
    semantic_managed_model_snapshot_dir, semantic_runtime_cache_dir, semantic_worker_cache_dir,
};

use super::super::{
    download::DownloadedArtifact,
    metadata::{SelectedSemanticAsset, SemanticAssetMetadata, SemanticFileMetadata},
    UpgradePlan,
};
use super::archive::{extract_runtime_archive, extract_semantic_archive};
#[cfg(unix)]
use super::durability::sync_directory;
use super::durability::sync_parent;

#[derive(Debug)]
pub(super) struct StagedRuntime {
    pub(super) staged_path: PathBuf,
    pub(super) target_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct StagedSemanticPath {
    pub(super) label: &'static str,
    pub(super) backup_label: &'static str,
    pub(super) staged_path: PathBuf,
    pub(super) target_path: PathBuf,
    pub(super) is_directory: bool,
}

#[derive(Debug)]
pub(super) struct StagedSemanticInstall {
    pub(super) paths: Vec<StagedSemanticPath>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInstallManifest {
    schema_version: u32,
    manager: String,
    metadata_trust: String,
    runtime: String,
    platform: String,
    version: String,
    sha256: String,
    artifact_url: String,
    installed_at: String,
    files: Vec<SemanticFileMetadata>,
}

const RUNTIME_INSTALL_MANIFEST: &str = "ctx-runtime-install.json";

pub(super) fn stage_downloaded_runtime_artifact(
    plan: &UpgradePlan,
    artifact: &mut DownloadedArtifact,
    unique: &str,
    data_root: &Path,
) -> Result<StagedRuntime> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("ONNX Runtime artifact provided without release metadata"))?;
    artifact.verify_unchanged()?;
    let archive_path = artifact.stable_path()?;
    let runtime_root = semantic_runtime_root(data_root)?;
    ensure_private_directory(&runtime_root)?;
    let onnxruntime = runtime_root.join("onnxruntime");
    ensure_private_directory(&onnxruntime)?;
    let runtime_parent = onnxruntime.join(&runtime.version);
    ensure_private_directory(&runtime_parent)?;
    let target_path = runtime_parent.join(&plan.platform);
    let staged_path = runtime_parent.join(format!(".{}.ctx-upgrade-{unique}.new", plan.platform));

    let result = (|| -> Result<()> {
        fs::create_dir(&staged_path)
            .with_context(|| format!("create staged runtime {}", staged_path.display()))?;
        restrict_private_directory(&staged_path)?;
        verify_private_directory(&staged_path)?;
        extract_runtime_archive(
            &archive_path,
            &staged_path,
            &runtime.artifact,
            &plan.platform,
            &runtime.version,
        )?;
        artifact.verify_unchanged()?;
        write_runtime_manifest(plan, &staged_path)?;
        #[cfg(unix)]
        sync_directory(&staged_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    sync_parent(&runtime_parent);
    Ok(StagedRuntime {
        staged_path,
        target_path,
    })
}

pub(super) fn semantic_runtime_root(data_root: &Path) -> Result<PathBuf> {
    let (source, root) = match env::var_os("CTX_RUNTIME_DIR") {
        Some(value) => ("CTX_RUNTIME_DIR", PathBuf::from(value)),
        None => ("selected ctx data root", data_root.join("runtime")),
    };
    if root.as_os_str().is_empty()
        || root
            .to_str()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        return Err(anyhow!("{source} must not be empty or whitespace-padded"));
    }
    if !root.is_absolute() {
        return Err(anyhow!("{source} must be an absolute path"));
    }
    canonicalize_selected_root(&root)
        .with_context(|| format!("canonicalize {source} {}", root.display()))
}

fn canonicalize_selected_root(root: &Path) -> Result<PathBuf> {
    match fs::canonicalize(root) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = root
                .parent()
                .ok_or_else(|| anyhow!("runtime root has no parent"))?;
            let name = root
                .file_name()
                .ok_or_else(|| anyhow!("runtime root has no file name"))?;
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("create runtime directory {}", path.display()));
        }
    }
    restrict_private_directory(path)?;
    verify_private_directory(path)
        .with_context(|| format!("verify runtime directory {}", path.display()))
}

fn write_runtime_manifest(plan: &UpgradePlan, staged_path: &Path) -> Result<()> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("release metadata has no ONNX Runtime sidecar"))?;
    let body = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "metadata_trust": "signed-release-metadata",
        "runtime": "onnxruntime",
        "platform": plan.platform,
        "version": runtime.version,
        "sha256": runtime.sha256,
        "artifact_url": plan.onnxruntime_artifact_url(),
        "installed_at": utc_now(),
    });
    let manifest = staged_path.join("ctx-runtime-install.json");
    let mut file = fs::File::create(&manifest)
        .with_context(|| format!("create runtime manifest {}", manifest.display()))?;
    file.write_all(&serde_json::to_vec_pretty(&body)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub(in crate::upgrade) fn semantic_install_required(
    plan: &UpgradePlan,
    data_root: &Path,
) -> Result<bool> {
    let Some(provisioning) = &plan.semantic_provisioning else {
        return Ok(false);
    };
    for selected in &provisioning.assets {
        if !semantic_asset_installed(plan, selected, data_root)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn stage_semantic_artifacts(
    plan: &UpgradePlan,
    downloads: &mut [DownloadedArtifact],
    unique: &str,
    data_root: &Path,
) -> Result<StagedSemanticInstall> {
    let provisioning = plan
        .semantic_provisioning
        .as_ref()
        .ok_or_else(|| anyhow!("Semantic bytes provided without a signed provisioning plan"))?;
    if downloads.len() != provisioning.assets.len() {
        return Err(anyhow!(
            "Semantic download count does not match the signed provisioning plan"
        ));
    }
    let mut staged = StagedSemanticInstall { paths: Vec::new() };
    for (selected, artifact) in provisioning.assets.iter().zip(downloads.iter_mut()) {
        artifact.verify_unchanged()?;
        let archive_path = artifact.stable_path()?;
        if let Err(error) = stage_semantic_asset(
            plan,
            selected,
            &archive_path,
            unique,
            data_root,
            &mut staged,
        ) {
            staged.cleanup();
            return Err(error);
        }
        artifact.verify_unchanged()?;
    }
    Ok(staged)
}

impl StagedSemanticInstall {
    pub(super) fn cleanup(&self) {
        for path in &self.paths {
            if path.is_directory {
                let _ = fs::remove_dir_all(&path.staged_path);
            } else {
                let _ = fs::remove_file(&path.staged_path);
            }
        }
    }
}

fn stage_semantic_asset(
    plan: &UpgradePlan,
    selected: &SelectedSemanticAsset,
    archive_path: &Path,
    unique: &str,
    data_root: &Path,
    staged: &mut StagedSemanticInstall,
) -> Result<()> {
    let asset = &selected.metadata;
    prepare_semantic_roots(asset, data_root)?;
    let target = semantic_asset_target(asset, data_root)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Semantic target has no parent: {}", target.display()))?;
    create_private_directory_all(parent)
        .with_context(|| format!("create private Semantic target parent {}", parent.display()))?;
    verify_private_directory(parent)
        .with_context(|| format!("verify Semantic target parent {}", parent.display()))?;
    let target_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("Semantic target has no UTF-8 file name"))?;
    let staged_path = parent.join(format!(".{target_name}.ctx-upgrade-{unique}.new"));
    reject_existing_stage(&staged_path)?;
    let result = (|| -> Result<()> {
        fs::create_dir(&staged_path)
            .with_context(|| format!("create staged Semantic asset {}", staged_path.display()))?;
        restrict_private_directory(&staged_path)
            .with_context(|| format!("protect staged Semantic asset {}", staged_path.display()))?;
        extract_semantic_archive(archive_path, &staged_path, asset)?;
        verify_exact_files(&staged_path, &asset.files, None)?;
        if is_ort_runtime(asset) {
            write_semantic_runtime_manifest(plan, asset, &staged_path)?;
            verify_runtime_install(plan, asset, &staged_path)?;
        }
        #[cfg(unix)]
        sync_directory(&staged_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    sync_parent(parent);
    staged.paths.push(StagedSemanticPath {
        label: semantic_asset_label(asset),
        backup_label: semantic_asset_backup_label(asset),
        staged_path: staged_path.clone(),
        target_path: target.clone(),
        is_directory: true,
    });

    if asset.backend == "coreml" {
        let manifest_sha = signed_manifest_sha(asset)?;
        let marker_target = coreml_completion_marker(&target, manifest_sha)?;
        let marker_name = marker_target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("Core ML completion marker has no UTF-8 file name"))?;
        let marker_staged =
            marker_target.with_file_name(format!(".{marker_name}.ctx-upgrade-{unique}.new"));
        reject_existing_stage(&marker_staged)?;
        write_coreml_completion_marker(&marker_staged, manifest_sha)?;
        staged.paths.push(StagedSemanticPath {
            label: "Semantic Core ML completion marker",
            backup_label: "coreml-marker",
            staged_path: marker_staged,
            target_path: marker_target,
            is_directory: false,
        });
    }
    Ok(())
}

fn semantic_asset_installed(
    plan: &UpgradePlan,
    selected: &SelectedSemanticAsset,
    data_root: &Path,
) -> Result<bool> {
    let asset = &selected.metadata;
    let target = semantic_asset_target(asset, data_root)?;
    let verified = if is_ort_runtime(asset) {
        verify_runtime_install(plan, asset, &target)
    } else {
        verify_exact_files(&target, &asset.files, None)
    };
    if verified.is_err() {
        return Ok(false);
    }
    if asset.backend == "coreml" {
        let manifest_sha = signed_manifest_sha(asset)?;
        let marker = coreml_completion_marker(&target, manifest_sha)?;
        return Ok(completion_marker_matches(&marker, manifest_sha));
    }
    Ok(true)
}

fn semantic_asset_target(asset: &SemanticAssetMetadata, data_root: &Path) -> Result<PathBuf> {
    let semantic_cache_dir = semantic_cache_root(data_root)?;
    match (asset.role.as_str(), asset.backend.as_str()) {
        ("model", "onnx") => Ok(semantic_managed_model_snapshot_dir(&semantic_cache_dir)),
        ("cpu-runtime", "ort-cpu")
        | ("cpu-runtime", "windows-ml")
        | ("accelerator", "ort-cuda") => Ok(semantic_provisioning_runtime_root(data_root)?
            .join("onnxruntime")
            .join(&asset.version)
            .join(&asset.platform)),
        ("accelerator", "coreml") => {
            let manifest_sha = signed_manifest_sha(asset)?;
            Ok(semantic_cache_dir
                .join("semantic-model-bundles")
                .join("sha256")
                .join(&manifest_sha[..2])
                .join(manifest_sha))
        }
        _ => Err(anyhow!(
            "unsupported Semantic installation target {}/{}",
            asset.role,
            asset.backend
        )),
    }
}

pub(super) fn semantic_cache_root(data_root: &Path) -> Result<PathBuf> {
    let root = semantic_worker_cache_dir(data_root);
    validate_selected_root("selected Semantic cache", &root)?;
    canonicalize_selected_root(&root)
        .with_context(|| format!("canonicalize selected Semantic cache {}", root.display()))
}

pub(super) fn semantic_provisioning_runtime_root(data_root: &Path) -> Result<PathBuf> {
    let (source, root) = match env::var_os("CTX_RUNTIME_DIR") {
        Some(value) => ("CTX_RUNTIME_DIR", PathBuf::from(value)),
        None => (
            "selected Semantic cache",
            semantic_runtime_cache_dir(data_root),
        ),
    };
    validate_selected_root(source, &root)?;
    canonicalize_selected_root(&root)
        .with_context(|| format!("canonicalize {source} {}", root.display()))
}

fn prepare_semantic_roots(asset: &SemanticAssetMetadata, data_root: &Path) -> Result<()> {
    let cache_root = semantic_cache_root(data_root)?;
    ensure_private_directory(&cache_root)?;
    if is_ort_runtime(asset) {
        let runtime_root = semantic_provisioning_runtime_root(data_root)?;
        ensure_private_directory(&runtime_root)?;
    }
    Ok(())
}

fn validate_selected_root(source: &str, root: &Path) -> Result<()> {
    if root.as_os_str().is_empty()
        || root
            .to_str()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        return Err(anyhow!("{source} must not be empty or whitespace-padded"));
    }
    if !root.is_absolute() {
        return Err(anyhow!("{source} must be an absolute path"));
    }
    Ok(())
}

fn write_semantic_runtime_manifest(
    plan: &UpgradePlan,
    asset: &SemanticAssetMetadata,
    staged_path: &Path,
) -> Result<()> {
    let body = RuntimeInstallManifest {
        schema_version: 1,
        manager: "ctx-hosted-installer".to_owned(),
        metadata_trust: "signed-release-metadata".to_owned(),
        runtime: runtime_name(asset)?.to_owned(),
        platform: asset.platform.clone(),
        version: asset.version.clone(),
        sha256: asset.archive_sha256.clone(),
        artifact_url: plan.semantic_artifact_url(&asset.artifact),
        installed_at: utc_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        files: asset.files.clone(),
    };
    let manifest = staged_path.join(RUNTIME_INSTALL_MANIFEST);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest)
        .with_context(|| format!("create runtime manifest {}", manifest.display()))?;
    file.write_all(&serde_json::to_vec_pretty(&body)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    restrict_private_file(&manifest)
        .with_context(|| format!("protect runtime manifest {}", manifest.display()))?;
    Ok(())
}

fn verify_runtime_install(
    plan: &UpgradePlan,
    asset: &SemanticAssetMetadata,
    root: &Path,
) -> Result<()> {
    let manifest_path = root.join(RUNTIME_INSTALL_MANIFEST);
    let bytes = fs::read(&manifest_path)
        .with_context(|| format!("read runtime manifest {}", manifest_path.display()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(anyhow!("runtime install manifest is too large"));
    }
    let manifest: RuntimeInstallManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse runtime manifest {}", manifest_path.display()))?;
    if manifest.schema_version != 1
        || manifest.manager != "ctx-hosted-installer"
        || manifest.metadata_trust != "signed-release-metadata"
        || manifest.runtime != runtime_name(asset)?
        || manifest.platform != asset.platform
        || manifest.version != asset.version
        || manifest.sha256 != asset.archive_sha256
        || manifest.artifact_url != plan.semantic_artifact_url(&asset.artifact)
        || manifest.installed_at.trim().is_empty()
        || manifest.files != asset.files
    {
        return Err(anyhow!(
            "runtime install manifest does not match signed release metadata"
        ));
    }
    verify_exact_files(root, &asset.files, Some(RUNTIME_INSTALL_MANIFEST))
}

fn verify_exact_files(
    root: &Path,
    expected: &[SemanticFileMetadata],
    allowed_extra: Option<&str>,
) -> Result<()> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("inspect Semantic installation {}", root.display()))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "Semantic installation root is not a real directory: {}",
            root.display()
        ));
    }
    let expected_by_path = expected
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, relative_dir)) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read Semantic directory {}", directory.display()))?
        {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow!("Semantic installation contains a non-UTF-8 path"))?;
            let relative = if relative_dir.is_empty() {
                name
            } else {
                format!("{relative_dir}/{name}")
            };
            if !folded.insert(relative.to_ascii_lowercase()) {
                return Err(anyhow!(
                    "Semantic installation contains a case-colliding path: {relative}"
                ));
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(anyhow!("Semantic installation contains a link: {relative}"));
            }
            if metadata.is_dir() {
                pending.push((entry.path(), relative));
                continue;
            }
            if !metadata.is_file() {
                return Err(anyhow!(
                    "Semantic installation contains a special file: {relative}"
                ));
            }
            if allowed_extra == Some(relative.as_str()) {
                continue;
            }
            let signed = expected_by_path
                .get(relative.as_str())
                .ok_or_else(|| anyhow!("Semantic installation has unexpected file {relative}"))?;
            if metadata.len() != signed.size {
                return Err(anyhow!(
                    "Semantic file {relative} size does not match signed metadata"
                ));
            }
            if sha256_file(&entry.path())? != signed.sha256 {
                return Err(anyhow!(
                    "Semantic file {relative} checksum does not match signed metadata"
                ));
            }
            actual.insert(relative);
        }
    }
    let expected_paths = expected_by_path
        .keys()
        .map(|path| (*path).to_owned())
        .collect::<BTreeSet<_>>();
    if actual != expected_paths {
        return Err(anyhow!(
            "Semantic installed file set does not exactly match signed metadata"
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = open_nofollow(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn open_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_nofollow(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

fn signed_manifest_sha(asset: &SemanticAssetMetadata) -> Result<&str> {
    let matches = asset
        .files
        .iter()
        .filter(|file| file.path == "manifest.json")
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [manifest] => Ok(&manifest.sha256),
        [] => Err(anyhow!(
            "Core ML signed metadata has no manifest.json record"
        )),
        _ => Err(anyhow!("Core ML signed metadata repeats manifest.json")),
    }
}

fn coreml_completion_marker(target: &Path, manifest_sha: &str) -> Result<PathBuf> {
    let target_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("Core ML bundle target has no UTF-8 name"))?;
    if target_name != manifest_sha {
        return Err(anyhow!(
            "Core ML target does not match its manifest SHA-256"
        ));
    }
    Ok(target.with_file_name(format!("{manifest_sha}.complete.json")))
}

fn write_coreml_completion_marker(path: &Path, manifest_sha: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create Core ML completion marker {}", path.display()))?;
    let body = serde_json::json!({
        "schema_version": 1,
        "manifest_sha256": manifest_sha,
    });
    file.write_all(&serde_json::to_vec(&body)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn completion_marker_matches(path: &Path, manifest_sha: &str) -> bool {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Marker {
        schema_version: u32,
        manifest_sha256: String,
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return false;
    }
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
        .is_some_and(|marker| marker.schema_version == 1 && marker.manifest_sha256 == manifest_sha)
}

fn reject_existing_stage(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(anyhow!(
            "Semantic staging path already exists: {}",
            path.display()
        ));
    }
    Ok(())
}

fn is_ort_runtime(asset: &SemanticAssetMetadata) -> bool {
    asset.backend.starts_with("ort-") || asset.backend == "windows-ml"
}

fn runtime_name(asset: &SemanticAssetMetadata) -> Result<&'static str> {
    match asset.backend.as_str() {
        "ort-cpu" | "ort-cuda" => Ok("onnxruntime"),
        "windows-ml" => Ok("windows-ml"),
        _ => Err(anyhow!(
            "Semantic asset {} is not an installable runtime",
            asset.backend
        )),
    }
}

fn semantic_asset_label(asset: &SemanticAssetMetadata) -> &'static str {
    match (asset.role.as_str(), asset.backend.as_str()) {
        ("model", "onnx") => "Semantic model",
        ("cpu-runtime", "ort-cpu") => "Semantic CPU runtime",
        ("cpu-runtime", "windows-ml") => "Semantic Windows ML runtime",
        ("accelerator", "ort-cuda") => "Semantic CUDA runtime",
        ("accelerator", "coreml") => "Semantic Core ML bundle",
        _ => "Semantic asset",
    }
}

fn semantic_asset_backup_label(asset: &SemanticAssetMetadata) -> &'static str {
    match (asset.role.as_str(), asset.backend.as_str()) {
        ("model", "onnx") => "model",
        ("cpu-runtime", "ort-cpu") => "cpu-runtime",
        ("cpu-runtime", "windows-ml") => "windows-ml-runtime",
        ("accelerator", "ort-cuda") => "cuda-runtime",
        ("accelerator", "coreml") => "coreml-bundle",
        _ => "semantic",
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn selected_runtime_root_canonicalizes_a_symlink_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        fs::create_dir(&real).unwrap();
        symlink(&real, &alias).unwrap();

        let selected = canonicalize_selected_root(&alias.join("runtime")).unwrap();

        assert_eq!(selected, real.join("runtime"));
    }
}
