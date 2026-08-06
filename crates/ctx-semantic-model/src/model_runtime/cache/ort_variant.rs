use super::*;

pub(crate) fn semantic_ort_cache_snapshot(
    cache_dir: &Path,
    variant: SemanticOrtModelVariant,
) -> Result<PathBuf> {
    if variant == SemanticOrtModelVariant::CpuFp32 {
        return semantic_cpu_cache_snapshot(cache_dir);
    }
    let mut repairable_error = None;
    for model_root in semantic_model_cache_roots(cache_dir) {
        let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
        match fs::metadata(&snapshot) {
            Ok(metadata) if metadata.is_dir() => {
                match verify_semantic_ort_snapshot(&snapshot, variant) {
                    Ok(()) => return Ok(snapshot),
                    Err(error) if semantic_cpu_cache_repairable(&error) => {
                        repairable_error.get_or_insert(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(_) => {
                repairable_error.get_or_insert_with(|| {
                    SemanticCpuModelIntegrityError(format!(
                        "semantic ONNX model snapshot {} is not a directory",
                        snapshot.display()
                    ))
                    .into()
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect semantic model cache {}", snapshot.display())
                });
            }
        }
    }
    Err(repairable_error.unwrap_or_else(|| {
        SemanticCpuModelCacheMissing(format!(
            "semantic {} model cache is incomplete at {}",
            variant.as_str(),
            cache_dir.display()
        ))
        .into()
    }))
}

fn verify_semantic_ort_snapshot(snapshot: &Path, variant: SemanticOrtModelVariant) -> Result<()> {
    for expected in variant.required_files() {
        verify_semantic_cpu_file(&snapshot.join(expected.path), expected)?;
    }
    Ok(())
}

pub(crate) fn read_semantic_ort_model_file(
    snapshot: &Path,
    relative: &str,
    variant: SemanticOrtModelVariant,
) -> Result<Vec<u8>> {
    let expected = variant
        .required_files()
        .find(|file| file.path == relative)
        .ok_or_else(|| anyhow!("semantic model file {relative:?} is not in the pinned contract"))?;
    let path = snapshot.join(relative);
    verify_semantic_cpu_file(&path, expected)?;
    fs::read(&path).with_context(|| format!("read semantic model file {}", path.display()))
}
