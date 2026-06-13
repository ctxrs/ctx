use super::*;

pub(super) async fn load_registry(data_root: &Path) -> Result<HarnessSourceRegistryInternal> {
    let path = registry_path(data_root);
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => serde_json::from_str(&raw)
            .with_context(|| format!("parsing harness source registry {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(HarnessSourceRegistryInternal::default())
        }
        Err(err) => {
            Err(err).with_context(|| format!("reading harness source registry {}", path.display()))
        }
    }
}

pub(super) async fn save_registry(
    data_root: &Path,
    registry: &HarnessSourceRegistryInternal,
) -> Result<()> {
    let path = registry_path(data_root);
    if let Some(parent) = path.parent() {
        ctx_fs::permissions::ensure_private_dir(parent).await?;
    }
    let payload = serde_json::to_vec_pretty(registry)?;
    ctx_fs::permissions::write_private_file_atomic(&path, &payload).await?;
    Ok(())
}

pub(super) fn registry_path(data_root: &Path) -> PathBuf {
    data_root
        .join("providers")
        .join("harness_sources")
        .join("registry.json")
}
