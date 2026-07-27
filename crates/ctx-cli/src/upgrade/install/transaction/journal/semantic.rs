use std::path::Path;

use anyhow::{anyhow, Result};

use super::{
    one_path, optional_one_path, transaction_backup_path, InstallTransactionJournal, JournalPath,
    JournalPathKind,
};

pub(super) fn validate_paths(
    journal: &InstallTransactionJournal,
    semantic: &[&JournalPath],
    platform: &str,
) -> Result<()> {
    if !matches!(semantic.len(), 2 | 4) {
        return Err(anyhow!(
            "signed Semantic install transaction must contain exactly two or four paths"
        ));
    }
    let model = one_path(journal, "Semantic model")?;
    validate_path(journal, model, "model", platform)?;
    let coreml = optional_one_path(journal, "Semantic Core ML bundle")?;
    let coreml_marker = optional_one_path(journal, "Semantic Core ML completion marker")?;
    if coreml.is_some() || coreml_marker.is_some() {
        if platform != "macos-arm64" || semantic.len() != 4 {
            return Err(anyhow!(
                "Semantic Core ML transaction requires the complete macOS arm64 fallback composition"
            ));
        }
        let cpu = one_path(journal, "Semantic CPU runtime")?;
        let bundle =
            coreml.ok_or_else(|| anyhow!("Semantic Core ML transaction is missing its bundle"))?;
        let marker = coreml_marker
            .ok_or_else(|| anyhow!("Semantic Core ML transaction is missing its marker"))?;
        validate_path(journal, cpu, "cpu-runtime", platform)?;
        validate_path(journal, bundle, "coreml-bundle", platform)?;
        validate_path(journal, marker, "coreml-marker", platform)?;
        let manifest_sha = bundle
            .target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Semantic Core ML bundle has no manifest identity"))?;
        if marker.target
            != bundle
                .target
                .with_file_name(format!("{manifest_sha}.complete.json"))
        {
            return Err(anyhow!(
                "Semantic Core ML completion marker does not match its bundle"
            ));
        }
        return Ok(());
    }

    if semantic.len() != 2 {
        return Err(anyhow!(
            "Semantic model transaction must contain exactly one signed runtime"
        ));
    }
    let runtimes = [
        ("Semantic CPU runtime", "cpu-runtime"),
        ("Semantic Windows ML runtime", "windows-ml-runtime"),
        ("Semantic CUDA runtime", "cuda-runtime"),
    ]
    .into_iter()
    .filter_map(|(label, backup)| {
        journal
            .paths
            .iter()
            .find(|path| path.label == label)
            .map(|path| (path, backup))
    })
    .collect::<Vec<_>>();
    if runtimes.len() != 1 {
        return Err(anyhow!(
            "Semantic model transaction must contain exactly one signed runtime"
        ));
    }
    validate_path(journal, runtimes[0].0, runtimes[0].1, platform)
}

fn validate_path(
    journal: &InstallTransactionJournal,
    path: &JournalPath,
    backup_label: &str,
    platform: &str,
) -> Result<()> {
    let name = path
        .target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("Semantic transaction target has no file name"))?;
    let expected_kind = if path.label == "Semantic Core ML completion marker" {
        JournalPathKind::File
    } else {
        JournalPathKind::Directory
    };
    if path.kind != expected_kind
        || path.staged
            != path
                .target
                .with_file_name(format!(".{name}.ctx-upgrade-{}.new", journal.attempt_id))
        || path.backup != transaction_backup_path(&path.target, &journal.attempt_id, backup_label)
    {
        return Err(anyhow!("install transaction has invalid Semantic paths"));
    }
    let cache_root = journal
        .semantic_cache_root
        .as_deref()
        .ok_or_else(|| anyhow!("Semantic install transaction has no cache root"))?;
    match path.label.as_str() {
        "Semantic model" => {
            if path.target != crate::semantic::semantic_managed_model_snapshot_dir(cache_root) {
                return Err(anyhow!(
                    "Semantic model transaction target does not match the runtime cache contract"
                ));
            }
        }
        "Semantic CPU runtime" => validate_runtime_target(journal, path, "1.27.0", platform)?,
        "Semantic Windows ML runtime" => {
            validate_runtime_target(journal, path, "2.1.74", "windows-x64")?
        }
        "Semantic CUDA runtime" => {
            validate_runtime_target(journal, path, "1.27.0", "linux-x64-cuda12")?
        }
        "Semantic Core ML bundle" => validate_coreml_target(cache_root, &path.target, false)?,
        "Semantic Core ML completion marker" => {
            validate_coreml_target(cache_root, &path.target, true)?
        }
        _ => return Err(anyhow!("install transaction has an unknown Semantic path")),
    }
    Ok(())
}

fn validate_runtime_target(
    journal: &InstallTransactionJournal,
    path: &JournalPath,
    version: &str,
    platform: &str,
) -> Result<()> {
    let expected = journal
        .runtime_root
        .join("onnxruntime")
        .join(version)
        .join(platform);
    if path.target != expected {
        return Err(anyhow!(
            "Semantic runtime transaction target does not match its signed identity"
        ));
    }
    Ok(())
}

fn validate_coreml_target(cache_root: &Path, target: &Path, marker: bool) -> Result<()> {
    let expected_root = cache_root.join("semantic-model-bundles").join("sha256");
    let relative = target
        .strip_prefix(&expected_root)
        .map_err(|_| anyhow!("Semantic Core ML target is outside the selected cache root"))?;
    let components = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("Semantic Core ML target is not UTF-8")),
            _ => Err(anyhow!("Semantic Core ML target is not canonical")),
        })
        .collect::<Result<Vec<_>>>()?;
    if components.len() != 2 {
        return Err(anyhow!("Semantic Core ML target has an invalid layout"));
    }
    let identity = if marker {
        components[1]
            .strip_suffix(".complete.json")
            .ok_or_else(|| anyhow!("Semantic Core ML marker has an invalid name"))?
    } else {
        components[1].as_str()
    };
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || components[0] != identity[..2]
    {
        return Err(anyhow!("Semantic Core ML target has an invalid identity"));
    }
    Ok(())
}
