use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    cuda_dependencies, LoadedOnnxRuntime, OnnxRuntimeFlavor, SEMANTIC_ONNXRUNTIME_DYLIB,
    SEMANTIC_ONNXRUNTIME_LIB_ENTRY,
};
use crate::configuration::{SemanticModelPaths, SemanticOnnxRuntimePaths};

pub(super) const RUNTIME_INSTALL_MANIFEST: &str = "ctx-runtime-install.json";

/// Provenance tiers ctx accepts for an installed runtime. The
/// manager and the metadata trust are validated as a pair: a hosted install
/// carries release-signed metadata, a local operator install carries a digest
/// the operator pinned by hand. Mixing them would let an unsigned local tree
/// claim release provenance.
pub(super) const HOSTED_INSTALLER_MANAGER: &str = "ctx-hosted-installer";
pub(super) const HOSTED_INSTALLER_METADATA_TRUST: &str = "signed-release-metadata";
pub(super) const LOCAL_OPERATOR_MANAGER: &str = "ctx-local-operator";
pub(super) const LOCAL_OPERATOR_METADATA_TRUST: &str = "operator-pinned-digest";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeInstallManifest {
    pub(super) schema_version: u32,
    pub(super) manager: String,
    pub(super) metadata_trust: String,
    pub(super) runtime: String,
    pub(super) platform: String,
    pub(super) version: String,
    pub(super) sha256: String,
    pub(super) artifact_url: String,
    pub(super) installed_at: String,
    pub(super) files: Vec<RuntimeFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeFile {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

pub(in crate::model_runtime) fn installed_accelerator_runtime_identity(
    paths: &SemanticModelPaths,
    flavor: OnnxRuntimeFlavor,
) -> Result<Option<String>> {
    let mut first_error = None;
    for path in verified_runtime_candidates(paths.onnx_runtime(), flavor)? {
        if !path.exists() {
            continue;
        }
        match validate_runtime_candidate(&path, flavor) {
            Ok(identity) => return Ok(Some(identity)),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

pub(in crate::model_runtime) fn revalidate_loaded_accelerator_runtime(
    runtime: &LoadedOnnxRuntime,
    flavor: OnnxRuntimeFlavor,
) -> Result<()> {
    if runtime.flavor != flavor {
        return Err(anyhow!(
            "loaded runtime flavor {:?} does not match requested {flavor:?}",
            runtime.flavor
        ));
    }
    let identity = validate_runtime_candidate(&runtime.path, flavor)?;
    if identity != runtime.artifact_identity {
        return Err(anyhow!(
            "accelerator runtime identity changed during session initialization"
        ));
    }
    Ok(())
}

/// Every flavor, the CPU runtime included, is provisioned into the same
/// `<root>/onnxruntime/<version>/<platform>/lib/<library>` layout, so one walk
/// serves the accelerator loader and the CPU loader alike.
pub(super) fn verified_runtime_candidates(
    runtime_paths: &SemanticOnnxRuntimePaths,
    flavor: OnnxRuntimeFlavor,
) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    if let Some(root) = runtime_paths.cache_dir.as_ref() {
        roots.push(root.clone());
    }
    if let Some(root) = runtime_paths.installed_runtime_dir.as_ref() {
        roots.push(root.clone());
    }
    if let Some(root) = runtime_paths.selected_data_root_runtime_dir.as_ref() {
        roots.push(root.clone());
    }
    roots.push(runtime_paths.default_runtime_cache_dir.clone());
    let platform = flavor.platform_dir()?;
    let mut candidates = Vec::new();
    for root in roots {
        if !root.is_absolute() {
            continue;
        }
        let candidate = root
            .join("onnxruntime")
            .join(flavor.version())
            .join(platform)
            .join("lib")
            .join(SEMANTIC_ONNXRUNTIME_DYLIB);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

pub(super) fn validate_runtime_candidate(path: &Path, flavor: OnnxRuntimeFlavor) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect runtime {}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!("runtime library is not a regular file"));
    }
    let lib = path
        .parent()
        .ok_or_else(|| anyhow!("runtime library has no parent directory"))?;
    if lib.file_name().and_then(|name| name.to_str()) != Some("lib") {
        return Err(anyhow!(
            "runtime library is not inside its canonical lib directory"
        ));
    }
    let root = lib
        .parent()
        .ok_or_else(|| anyhow!("runtime lib directory has no parent"))?;
    ctx_history_platform::platform_security::verify_private_directory(root)
        .with_context(|| format!("verify private runtime directory {}", root.display()))?;
    let manifest = read_runtime_install_manifest(root)?;
    let expected_platform = flavor.platform_dir()?;
    if !matches!(
        (manifest.manager.as_str(), manifest.metadata_trust.as_str()),
        (HOSTED_INSTALLER_MANAGER, HOSTED_INSTALLER_METADATA_TRUST)
            | (LOCAL_OPERATOR_MANAGER, LOCAL_OPERATOR_METADATA_TRUST)
    ) {
        return Err(anyhow!(
            "runtime installer manifest pairs manager {:?} with metadata trust {:?}; ctx accepts only {HOSTED_INSTALLER_MANAGER} with {HOSTED_INSTALLER_METADATA_TRUST} or {LOCAL_OPERATOR_MANAGER} with {LOCAL_OPERATOR_METADATA_TRUST}",
            manifest.manager,
            manifest.metadata_trust,
        ));
    }
    if manifest.schema_version != 1
        || manifest.runtime != flavor.runtime_name()
        || manifest.platform != expected_platform
        || manifest.version != flavor.version()
        || !is_sha256(&manifest.sha256)
        || manifest.artifact_url.trim().is_empty()
        || manifest.installed_at.trim().is_empty()
    {
        return Err(anyhow!(
            "runtime installer manifest does not match the verified {expected_platform} {} contract",
            flavor.runtime_name()
        ));
    }
    validate_installed_runtime_files(root, flavor, &manifest.files)?;
    let mut records = manifest
        .files
        .iter()
        .map(|file| format!("{}:{}:{}", file.path, file.size, file.sha256))
        .collect::<Vec<_>>();
    records.sort();
    let files_identity = format!("{:x}", Sha256::digest(records.join("|").as_bytes()));
    Ok(format!(
        "{}|platform={}|version={}|sha256={}|files_sha256={}|artifact_url={}|manager={}|metadata_trust={}",
        manifest.runtime,
        manifest.platform,
        manifest.version,
        manifest.sha256,
        files_identity,
        manifest.artifact_url,
        manifest.manager,
        manifest.metadata_trust,
    ))
}

pub(super) fn read_runtime_install_manifest(root: &Path) -> Result<RuntimeInstallManifest> {
    let manifest_path = root.join(RUNTIME_INSTALL_MANIFEST);
    let bytes = read_runtime_file_nofollow(&manifest_path, 16 * 1024)
        .with_context(|| format!("read runtime manifest {}", manifest_path.display()))?;
    serde_json::from_slice(&bytes).context("parse runtime installer manifest")
}

/// How a runtime directory presents its provenance before verification.
///
/// The direct-release installers (`scripts/dev-install-from-metadata.sh`,
/// `scripts/install.ps1`) write a `ctx-runtime-install.json` of their own that
/// names neither accepted tier and carries no per-file records at all, and the
/// CPU loader has always loaded those installs. So only a manifest that claims
/// ctx's own provenance is held to the verified contract: anything else stays
/// invisible to verification rather than becoming a load failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeInstallClaim {
    /// No manifest: a hand-extracted or pre-manifest sidecar.
    Absent,
    /// A manifest from an installer whose provenance ctx does not verify.
    Unverified,
    /// A manifest claiming hosted-installer or local-operator provenance, which
    /// must then satisfy `validate_runtime_candidate` in full.
    CtxProvenance,
}

/// Deliberately loose: this only reads the provenance claim, so a manifest ctx
/// did not write is classified instead of failing to deserialize.
#[derive(Deserialize)]
struct RuntimeProvenanceClaim {
    #[serde(default)]
    manager: String,
    #[serde(default)]
    metadata_trust: String,
}

pub(super) fn runtime_install_claim(root: &Path) -> RuntimeInstallClaim {
    let manifest_path = root.join(RUNTIME_INSTALL_MANIFEST);
    if fs::symlink_metadata(&manifest_path).is_err() {
        return RuntimeInstallClaim::Absent;
    }
    let Ok(bytes) = read_runtime_file_nofollow(&manifest_path, 16 * 1024) else {
        return RuntimeInstallClaim::Unverified;
    };
    let Ok(claim) = serde_json::from_slice::<RuntimeProvenanceClaim>(&bytes) else {
        return RuntimeInstallClaim::Unverified;
    };
    if matches!(
        claim.manager.as_str(),
        HOSTED_INSTALLER_MANAGER | LOCAL_OPERATOR_MANAGER
    ) || matches!(
        claim.metadata_trust.as_str(),
        HOSTED_INSTALLER_METADATA_TRUST | LOCAL_OPERATOR_METADATA_TRUST
    ) {
        // Half a claim still counts: a manifest that names one side of a tier
        // and not the other is exactly the mixed pair the validator rejects.
        return RuntimeInstallClaim::CtxProvenance;
    }
    RuntimeInstallClaim::Unverified
}

fn validate_installed_runtime_files(
    root: &Path,
    flavor: OnnxRuntimeFlavor,
    declared: &[RuntimeFile],
) -> Result<()> {
    let mut expected = expected_runtime_files(flavor);
    expected.sort_unstable();
    let mut declared_paths = declared
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    declared_paths.sort_unstable();
    declared_paths.dedup();
    if declared_paths.len() != declared.len() || declared_paths != expected {
        return Err(anyhow!(
            "runtime installer manifest file allowlist does not match the {flavor:?} contract"
        ));
    }
    let mut actual = Vec::new();
    collect_runtime_files(root, root, &mut actual)?;
    actual.retain(|path| path != RUNTIME_INSTALL_MANIFEST);
    actual.sort();
    if actual.iter().map(String::as_str).collect::<Vec<_>>() != expected {
        return Err(anyhow!(
            "runtime directory contains missing or unexpected files"
        ));
    }
    for file in declared {
        if !is_safe_relative_file(&file.path) || file.size == 0 || !is_sha256(&file.sha256) {
            return Err(anyhow!("runtime manifest contains an invalid file record"));
        }
        let path = root.join(&file.path);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect runtime file {}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.len() != file.size {
            return Err(anyhow!(
                "runtime file {} size/type does not match verified manifest",
                file.path
            ));
        }
        if sha256_runtime_file(&path)? != file.sha256 {
            return Err(anyhow!(
                "runtime file {} SHA-256 does not match verified manifest",
                file.path
            ));
        }
    }
    Ok(())
}

/// The in-binary file contract for every runtime flavor.
///
/// The CPU list mirrors `CPU_RUNTIME_FILES` in
/// `scripts/semantic_release_assets/contracts.py` plus the one platform library
/// the sidecar builder places under `lib/`, so the published sidecar and this
/// allowlist have to be changed together.
pub(super) fn expected_runtime_files(flavor: OnnxRuntimeFlavor) -> Vec<&'static str> {
    match flavor {
        OnnxRuntimeFlavor::Cpu => vec![
            "GIT_COMMIT_ID",
            "LICENSE",
            "ThirdPartyNotices.txt",
            "VERSION_NUMBER",
            SEMANTIC_ONNXRUNTIME_LIB_ENTRY,
        ],
        OnnxRuntimeFlavor::WindowsMl => vec![
            "LICENSE",
            "ThirdPartyNotices.txt",
            "lib/DirectML.dll",
            "lib/Microsoft.Windows.AI.MachineLearning.dll",
            "lib/onnxruntime.dll",
        ],
        OnnxRuntimeFlavor::Cuda => {
            let mut files = vec![
                "GIT_COMMIT_ID",
                "LICENSE",
                "ThirdPartyNotices.txt",
                "VERSION_NUMBER",
                "lib/libonnxruntime.so",
                "lib/libonnxruntime_providers_cuda.so",
                "lib/libonnxruntime_providers_shared.so",
            ];
            files.extend(cuda_dependencies::DOCUMENTS.iter().copied());
            files.extend(cuda_dependencies::FILES.iter().copied());
            files
        }
    }
}

fn collect_runtime_files(root: &Path, directory: &Path, files: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read runtime directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "runtime directory contains symbolic link {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_runtime_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| anyhow!("runtime file escaped runtime root"))?;
            files.push(relative.to_string_lossy().replace('\\', "/"));
        } else {
            return Err(anyhow!(
                "runtime directory contains unsupported entry {}",
                path.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn is_safe_relative_file(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn read_runtime_file_nofollow(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum as u64 {
        return Err(anyhow!("runtime file is not a bounded regular file"));
    }
    let mut file = open_runtime_file_nofollow(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(anyhow!("runtime file exceeds its size limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_runtime_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_runtime_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_runtime_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

pub(super) fn sha256_runtime_file(path: &Path) -> Result<String> {
    let mut file = open_runtime_file_nofollow(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_absolute_path(path: &str) -> PathBuf {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\ctx-test")
        } else {
            PathBuf::from("/tmp/ctx-test")
        };
        root.join(path)
    }

    #[test]
    fn accelerator_runtime_contracts_are_pinned_and_self_contained() {
        assert_eq!(OnnxRuntimeFlavor::Cuda.version(), "1.27.0");
        assert_eq!(OnnxRuntimeFlavor::WindowsMl.version(), "2.1.74");
        assert_eq!(
            OnnxRuntimeFlavor::Cuda.asset_name(),
            "ctx-onnxruntime-linux-x64-cuda12.tar.zst"
        );
        assert_eq!(
            OnnxRuntimeFlavor::WindowsMl.asset_name(),
            "ctx-windowsml-windows-x64.zip"
        );
        let cuda = expected_runtime_files(OnnxRuntimeFlavor::Cuda);
        assert!(cuda.contains(&"lib/libonnxruntime_providers_cuda.so"));
        assert!(cuda.contains(&"lib/libcudnn.so.9"));
        assert!(cuda.contains(&"NVIDIA-CUDA-LICENSE.txt"));
        let windows = expected_runtime_files(OnnxRuntimeFlavor::WindowsMl);
        assert_eq!(
            windows,
            [
                "LICENSE",
                "ThirdPartyNotices.txt",
                "lib/DirectML.dll",
                "lib/Microsoft.Windows.AI.MachineLearning.dll",
                "lib/onnxruntime.dll",
            ]
        );
    }

    #[test]
    fn bundled_cuda_dependencies_resolve_next_to_runtime_library() {
        let runtime = test_absolute_path("runtime/lib/libonnxruntime.so");
        let paths = cuda_dependencies::paths(&runtime).unwrap();
        assert_eq!(paths.len(), cuda_dependencies::FILES.len());
        assert_eq!(paths[0], test_absolute_path("runtime/lib/libcudart.so.12"));
        assert_eq!(
            paths.last().unwrap(),
            &test_absolute_path("runtime/lib/libcudnn_ops.so.9")
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn signed_accelerator_runtime_manifest_is_exact_and_tamper_evident() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        ctx_history_platform::platform_security::restrict_private_directory(root).unwrap();
        let mut files = Vec::new();
        for relative in expected_runtime_files(OnnxRuntimeFlavor::Cuda) {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = relative.as_bytes();
            fs::write(&path, bytes).unwrap();
            files.push(serde_json::json!({
                "path": relative,
                "size": bytes.len(),
                "sha256": format!("{:x}", Sha256::digest(bytes)),
            }));
        }
        files.sort_by(|left, right| {
            left["path"]
                .as_str()
                .unwrap()
                .cmp(right["path"].as_str().unwrap())
        });
        fs::write(
            root.join(RUNTIME_INSTALL_MANIFEST),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "metadata_trust": "signed-release-metadata",
                "runtime": "onnxruntime",
                "platform": "linux-x64-cuda12",
                "version": "1.27.0",
                "sha256": "a".repeat(64),
                "artifact_url": "https://cli.ctx.rs/runtime",
                "installed_at": "2026-07-24T00:00:00Z",
                "files": files,
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = root.join("lib/libonnxruntime.so");
        assert!(validate_runtime_candidate(&runtime, OnnxRuntimeFlavor::Cuda).is_ok());

        fs::write(&runtime, b"tampered").unwrap();
        assert!(
            validate_runtime_candidate(&runtime, OnnxRuntimeFlavor::Cuda)
                .unwrap_err()
                .to_string()
                .contains("size/type")
        );
    }

    #[test]
    fn cpu_runtime_contract_mirrors_the_published_sidecar() {
        assert_eq!(OnnxRuntimeFlavor::Cpu.version(), "1.27.0");
        assert_eq!(OnnxRuntimeFlavor::Cpu.runtime_name(), "onnxruntime");
        let library = format!("lib/{SEMANTIC_ONNXRUNTIME_DYLIB}");
        assert_eq!(SEMANTIC_ONNXRUNTIME_LIB_ENTRY, library);
        assert_eq!(
            expected_runtime_files(OnnxRuntimeFlavor::Cpu),
            [
                "GIT_COMMIT_ID",
                "LICENSE",
                "ThirdPartyNotices.txt",
                "VERSION_NUMBER",
                library.as_str(),
            ]
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn verified_cpu_runtime_manifest_is_exact_and_tamper_evident() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        ctx_history_platform::platform_security::restrict_private_directory(root).unwrap();
        let mut records = Vec::new();
        for relative in expected_runtime_files(OnnxRuntimeFlavor::Cpu) {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = format!("ctx-cpu-runtime::{relative}").into_bytes();
            fs::write(&path, &bytes).unwrap();
            records.push(serde_json::json!({
                "path": relative,
                "size": bytes.len(),
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }));
        }
        let manifest = serde_json::json!({
            "schema_version": 1,
            "manager": LOCAL_OPERATOR_MANAGER,
            "metadata_trust": LOCAL_OPERATOR_METADATA_TRUST,
            "runtime": "onnxruntime",
            "platform": OnnxRuntimeFlavor::Cpu.platform_dir().unwrap(),
            "version": "1.27.0",
            "sha256": "b".repeat(64),
            "artifact_url": "file:///tmp/ctx-onnxruntime-linux-x64.tar.zst",
            "installed_at": "2026-09-08T00:00:00Z",
            "files": records,
        });
        let write = |value: &serde_json::Value| {
            fs::write(
                root.join(RUNTIME_INSTALL_MANIFEST),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        };
        let library = root.join(SEMANTIC_ONNXRUNTIME_LIB_ENTRY);
        let reject = |value: &serde_json::Value, needle: &str| {
            write(value);
            let error = validate_runtime_candidate(&library, OnnxRuntimeFlavor::Cpu)
                .unwrap_err()
                .to_string();
            assert!(error.contains(needle), "{error}");
        };

        write(&manifest);
        let identity = validate_runtime_candidate(&library, OnnxRuntimeFlavor::Cpu).unwrap();
        assert!(
            identity.starts_with("onnxruntime|platform=linux-x64|version=1.27.0|"),
            "{identity}"
        );
        assert!(
            identity.contains("manager=ctx-local-operator|metadata_trust=operator-pinned-digest"),
            "{identity}"
        );
        assert_eq!(
            runtime_install_claim(root),
            RuntimeInstallClaim::CtxProvenance
        );

        let mut wrong_hash = manifest.clone();
        wrong_hash["files"][0]["sha256"] = serde_json::json!("c".repeat(64));
        reject(&wrong_hash, "SHA-256 does not match verified manifest");

        let mut wrong_size = manifest.clone();
        wrong_size["files"][0]["size"] = serde_json::json!(4096);
        reject(&wrong_size, "size/type does not match verified manifest");

        let mut extra_file = manifest.clone();
        extra_file["files"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "path": "EXTRA",
                "size": 3,
                "sha256": format!("{:x}", Sha256::digest(b"abc")),
            }));
        reject(&extra_file, "file allowlist does not match");

        let mut missing_file = manifest.clone();
        missing_file["files"].as_array_mut().unwrap().pop();
        reject(&missing_file, "file allowlist does not match");

        for (manager, metadata_trust) in [
            (LOCAL_OPERATOR_MANAGER, HOSTED_INSTALLER_METADATA_TRUST),
            (HOSTED_INSTALLER_MANAGER, LOCAL_OPERATOR_METADATA_TRUST),
        ] {
            let mut mixed = manifest.clone();
            mixed["manager"] = serde_json::json!(manager);
            mixed["metadata_trust"] = serde_json::json!(metadata_trust);
            reject(&mixed, "pairs manager");
            // Half a tier is still a claim on ctx provenance, so the loader has
            // to hold it to the contract instead of ignoring the manifest.
            assert_eq!(
                runtime_install_claim(root),
                RuntimeInstallClaim::CtxProvenance
            );
        }

        write(&manifest);
        let stray = root.join("STRAY");
        fs::write(&stray, b"stray").unwrap();
        let error = validate_runtime_candidate(&library, OnnxRuntimeFlavor::Cpu)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing or unexpected files"), "{error}");
        fs::remove_file(&stray).unwrap();

        fs::write(&library, b"tampered").unwrap();
        let error = validate_runtime_candidate(&library, OnnxRuntimeFlavor::Cpu)
            .unwrap_err()
            .to_string();
        assert!(error.contains("size/type"), "{error}");
    }

    /// The direct-release installers write their own manifest with a manager ctx
    /// does not verify and no per-file records; classifying it as verified would
    /// turn every existing CPU install into a load failure.
    #[test]
    fn explicit_metadata_installer_manifest_is_not_a_ctx_provenance_claim() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        assert_eq!(runtime_install_claim(root), RuntimeInstallClaim::Absent);
        fs::write(
            root.join(RUNTIME_INSTALL_MANIFEST),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "manager": "ctx-explicit-metadata-installer",
                "metadata_trust": "explicit-unsigned",
                "runtime": "onnxruntime",
                "platform": "linux-x64",
                "version": "1.27.0",
                "sha256": "d".repeat(64),
                "artifact_url": "https://cli.ctx.rs/ctx-onnxruntime-linux-x64.tar.gz",
                "installed_at": "2026-09-08T00:00:00Z",
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(runtime_install_claim(root), RuntimeInstallClaim::Unverified);

        fs::write(root.join(RUNTIME_INSTALL_MANIFEST), b"{not-json").unwrap();
        assert_eq!(runtime_install_claim(root), RuntimeInstallClaim::Unverified);
    }
}
