use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde_json::json;

use super::super::UpgradePlan;
use super::archive::extract_runtime_archive;
#[cfg(unix)]
use super::durability::sync_directory;
use super::durability::sync_parent;

#[derive(Debug)]
pub(super) struct StagedRuntime {
    pub(super) staged_path: PathBuf,
    pub(super) target_path: PathBuf,
}

pub(super) fn stage_runtime_artifact(
    plan: &UpgradePlan,
    bytes: &[u8],
    unique: &str,
    data_root: &Path,
) -> Result<StagedRuntime> {
    let runtime = plan
        .metadata
        .onnxruntime
        .as_ref()
        .ok_or_else(|| anyhow!("ONNX Runtime bytes provided without release metadata"))?;
    let runtime_root = semantic_runtime_root(data_root)?;
    let runtime_parent = runtime_root.join("onnxruntime").join(&runtime.version);
    let target_path = runtime_parent.join(&plan.platform);
    let staged_path = runtime_parent.join(format!(".{}.ctx-upgrade-{unique}.new", plan.platform));
    let archive_path = runtime_parent.join(format!(".ctx-runtime-{unique}.download"));
    fs::create_dir_all(&runtime_parent)?;
    let result = (|| -> Result<()> {
        let mut archive = fs::File::create(&archive_path)
            .with_context(|| format!("create staged runtime {}", archive_path.display()))?;
        archive.write_all(bytes)?;
        archive.sync_all()?;
        fs::create_dir(&staged_path)
            .with_context(|| format!("create staged runtime {}", staged_path.display()))?;
        extract_runtime_archive(
            &archive_path,
            &staged_path,
            &runtime.artifact,
            &plan.platform,
            &runtime.version,
        )?;
        write_runtime_manifest(plan, &staged_path)?;
        #[cfg(unix)]
        sync_directory(&staged_path)?;
        Ok(())
    })();
    let _ = fs::remove_file(&archive_path);
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
    validate_runtime_root(source, &root)?;
    Ok(root)
}

fn validate_runtime_root(source: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .to_str()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        return Err(anyhow!("{source} must not be empty or whitespace-padded"));
    }
    if !path.is_absolute() {
        return Err(anyhow!("{source} must be an absolute path"));
    }
    Ok(())
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
