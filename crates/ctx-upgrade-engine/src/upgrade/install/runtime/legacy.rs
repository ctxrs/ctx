use std::{collections::BTreeSet, fs, io::Read as _, path::Path};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::{open_nofollow, semantic_runtime_root, RUNTIME_INSTALL_MANIFEST};
use crate::upgrade::UpgradePlan;

const RUNTIME_INSTALL_MANIFEST_MAX_BYTES: usize = 1024 * 1024;
const RUNTIME_VERSION_MAX_BYTES: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRuntimeInstallManifest {
    schema_version: u32,
    manager: String,
    metadata_trust: String,
    runtime: String,
    platform: String,
    version: String,
    sha256: String,
    artifact_url: String,
    installed_at: String,
}

pub(in crate::upgrade) fn install_required(plan: &UpgradePlan, data_root: &Path) -> Result<bool> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("legacy runtime repair has no signed ONNX Runtime metadata"))?;
    let runtime_root = semantic_runtime_root(data_root)?;
    match fs::symlink_metadata(&runtime_root) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(anyhow!(
                "configured runtime root is not a real directory: {}",
                runtime_root.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect runtime root {}", runtime_root.display()));
        }
    }
    let target = runtime_root
        .join("onnxruntime")
        .join(&runtime.version)
        .join(&plan.platform);
    Ok(verify_install(plan, &runtime_root, &target).is_err())
}

fn verify_install(plan: &UpgradePlan, runtime_root: &Path, target: &Path) -> Result<()> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("legacy runtime verification has no signed metadata"))?;
    let mut component = runtime_root.to_path_buf();
    for name in [
        "onnxruntime",
        runtime.version.as_str(),
        plan.platform.as_str(),
    ] {
        component.push(name);
        let metadata = fs::symlink_metadata(&component)
            .with_context(|| format!("inspect legacy runtime target {}", component.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "legacy runtime target contains a symlink: {}",
                component.display()
            ));
        }
    }

    verify_layout(target, &plan.platform, &runtime.version)?;
    let manifest_path = target.join(RUNTIME_INSTALL_MANIFEST);
    let bytes = read_bounded_regular_file(
        &manifest_path,
        RUNTIME_INSTALL_MANIFEST_MAX_BYTES,
        "runtime install manifest",
    )?;
    let manifest: LegacyRuntimeInstallManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse runtime manifest {}", manifest_path.display()))?;
    let artifact_url = plan
        .onnxruntime_artifact_url()
        .ok_or_else(|| anyhow!("legacy runtime verification has no artifact URL"))?;
    if manifest.schema_version != 1
        || manifest.manager != "ctx-hosted-installer"
        || manifest.metadata_trust != "signed-release-metadata"
        || manifest.runtime != "onnxruntime"
        || manifest.platform != plan.platform
        || manifest.version != runtime.version
        || manifest.sha256 != runtime.sha256
        || manifest.artifact_url != artifact_url
        || manifest.installed_at.trim().is_empty()
    {
        return Err(anyhow!(
            "legacy runtime install manifest does not match signed release metadata"
        ));
    }
    Ok(())
}

fn verify_layout(root: &Path, platform: &str, version: &str) -> Result<()> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("inspect legacy runtime installation {}", root.display()))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "legacy runtime installation root is not a real directory: {}",
            root.display()
        ));
    }
    // The extracted-file allowlist is kept identical to the legacy archive
    // extractor contract. The installer-owned manifest is the sole allowed
    // post-extraction addition.
    let mut expected_files = expected_files(platform)?;
    expected_files.insert(RUNTIME_INSTALL_MANIFEST.to_owned());
    let mut expected = expected_files.clone();
    expected.insert("lib".to_owned());
    let mut actual = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, relative_dir)) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow!("legacy runtime contains a non-UTF-8 path"))?;
            let relative = if relative_dir.is_empty() {
                name
            } else {
                format!("{relative_dir}/{name}")
            };
            if !folded.insert(relative.to_ascii_lowercase()) {
                return Err(anyhow!(
                    "legacy runtime contains a case-colliding path: {relative}"
                ));
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(anyhow!("legacy runtime contains a link: {relative}"));
            }
            if metadata.is_dir() {
                if relative != "lib" {
                    return Err(anyhow!(
                        "legacy runtime contains an unexpected directory: {relative}"
                    ));
                }
                pending.push((entry.path(), relative.clone()));
            } else if !metadata.is_file() || !expected_files.contains(&relative) {
                return Err(anyhow!(
                    "legacy runtime contains an unexpected file: {relative}"
                ));
            }
            actual.insert(relative);
        }
    }
    if actual != expected {
        return Err(anyhow!(
            "legacy runtime installed layout does not match the canonical extracted layout"
        ));
    }
    let version_path = root.join("VERSION_NUMBER");
    let actual_version = read_bounded_regular_file(
        &version_path,
        RUNTIME_VERSION_MAX_BYTES,
        "runtime version file",
    )?;
    if actual_version != format!("{version}\n").as_bytes() {
        return Err(anyhow!("runtime VERSION_NUMBER is not exactly {version}"));
    }
    Ok(())
}

fn expected_files(platform: &str) -> Result<BTreeSet<String>> {
    #[cfg(unix)]
    {
        let library = if platform.starts_with("macos-") {
            "libonnxruntime.dylib"
        } else if platform.starts_with("linux-") {
            "libonnxruntime.so"
        } else {
            return Err(anyhow!("unsupported legacy runtime platform {platform}"));
        };
        return Ok(BTreeSet::from([
            "LICENSE".to_owned(),
            "ThirdPartyNotices.txt".to_owned(),
            "VERSION_NUMBER".to_owned(),
            "GIT_COMMIT_ID".to_owned(),
            format!("lib/{library}"),
        ]));
    }
    #[cfg(windows)]
    {
        if platform != "windows-x64" {
            return Err(anyhow!("unsupported legacy runtime platform {platform}"));
        }
        return Ok([
            "LICENSE",
            "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
            "ThirdPartyNotices.txt",
            "VERSION_NUMBER",
            "GIT_COMMIT_ID",
            "lib/onnxruntime.dll",
            "lib/msvcp140.dll",
            "lib/msvcp140_1.dll",
            "lib/vcruntime140.dll",
            "lib/vcruntime140_1.dll",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect());
    }
    #[allow(unreachable_code)]
    Err(anyhow!("legacy runtime verification is unsupported"))
}

fn read_bounded_regular_file(path: &Path, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(anyhow!("{label} is not a regular file"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(anyhow!("{label} is too large"));
    }
    let file = open_nofollow(path).with_context(|| format!("open {label} {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(anyhow!("{label} is too large"));
    }
    Ok(bytes)
}
