use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    cuda_dependencies, semantic_onnxruntime_default_cache_dir,
    semantic_onnxruntime_selected_data_root, LoadedOnnxRuntime, OnnxRuntimeFlavor,
    SemanticOnnxRuntimeEnv, SEMANTIC_ONNXRUNTIME_DYLIB,
};

const RUNTIME_INSTALL_MANIFEST: &str = "ctx-runtime-install.json";

#[derive(Debug, Clone, Deserialize)]
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
    files: Vec<RuntimeFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFile {
    path: String,
    size: u64,
    sha256: String,
}

pub(in crate::semantic::model_runtime) fn installed_accelerator_runtime_identity(
    model_cache_dir: &Path,
    flavor: OnnxRuntimeFlavor,
) -> Result<Option<String>> {
    let mut first_error = None;
    for path in verified_accelerator_runtime_candidates(model_cache_dir, flavor)? {
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

pub(in crate::semantic::model_runtime) fn revalidate_loaded_accelerator_runtime(
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

pub(super) fn verified_accelerator_runtime_candidates(
    model_cache_dir: &Path,
    flavor: OnnxRuntimeFlavor,
) -> Result<Vec<PathBuf>> {
    if flavor == OnnxRuntimeFlavor::Cpu {
        return Err(anyhow!("CPU runtime is not an accelerator candidate"));
    }
    let environment = SemanticOnnxRuntimeEnv::current();
    let mut roots = Vec::new();
    if let Some(root) = environment.cache_dir {
        roots.push(root);
    }
    if let Some(root) = environment.runtime_dir {
        roots.push(root);
    }
    if let Some(root) = semantic_onnxruntime_selected_data_root(model_cache_dir) {
        roots.push(root.join("runtime"));
    }
    roots.push(semantic_onnxruntime_default_cache_dir(model_cache_dir));
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
    ctx_history_core::platform_security::verify_private_directory(root)
        .with_context(|| format!("verify private runtime directory {}", root.display()))?;
    let manifest_path = root.join(RUNTIME_INSTALL_MANIFEST);
    let bytes = read_runtime_file_nofollow(&manifest_path, 16 * 1024)
        .with_context(|| format!("read runtime manifest {}", manifest_path.display()))?;
    let manifest: RuntimeInstallManifest =
        serde_json::from_slice(&bytes).context("parse runtime installer manifest")?;
    let expected_platform = flavor.platform_dir()?;
    if manifest.schema_version != 1
        || manifest.manager != "ctx-hosted-installer"
        || manifest.metadata_trust != "signed-release-metadata"
        || manifest.runtime != flavor.runtime_name()
        || manifest.platform != expected_platform
        || manifest.version != flavor.version()
        || !is_sha256(&manifest.sha256)
        || manifest.artifact_url.trim().is_empty()
        || manifest.installed_at.trim().is_empty()
    {
        return Err(anyhow!(
            "runtime installer manifest does not match the signed {expected_platform} {} contract",
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

fn validate_installed_runtime_files(
    root: &Path,
    flavor: OnnxRuntimeFlavor,
    declared: &[RuntimeFile],
) -> Result<()> {
    let mut expected = expected_accelerator_runtime_files(flavor);
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

fn expected_accelerator_runtime_files(flavor: OnnxRuntimeFlavor) -> Vec<&'static str> {
    match flavor {
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
        OnnxRuntimeFlavor::Cpu => Vec::new(),
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

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_safe_relative_file(path: &str) -> bool {
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

fn sha256_runtime_file(path: &Path) -> Result<String> {
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
        let cuda = expected_accelerator_runtime_files(OnnxRuntimeFlavor::Cuda);
        assert!(cuda.contains(&"lib/libonnxruntime_providers_cuda.so"));
        assert!(cuda.contains(&"lib/libcudnn.so.9"));
        assert!(cuda.contains(&"NVIDIA-CUDA-LICENSE.txt"));
        let windows = expected_accelerator_runtime_files(OnnxRuntimeFlavor::WindowsMl);
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
        ctx_history_core::platform_security::restrict_private_directory(root).unwrap();
        let mut files = Vec::new();
        for relative in expected_accelerator_runtime_files(OnnxRuntimeFlavor::Cuda) {
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
}
