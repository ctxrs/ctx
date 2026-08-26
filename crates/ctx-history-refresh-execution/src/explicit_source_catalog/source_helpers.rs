use super::*;

pub(super) fn custom_provider_source(path: PathBuf, exists: bool) -> Result<ProviderSource> {
    if exists {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("inspect Custom History source {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "Custom History source must be one regular JSONL file: {}",
                path.display()
            );
        }
    }
    Ok(ProviderSource {
        provider: CaptureProvider::Custom,
        path,
        exists,
        source_format: CUSTOM_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        unsupported_reason: None,
        route_provenance: Default::default(),
    })
}

pub(super) fn goose_platform_root(database: &Path) -> Result<PathBuf> {
    let sessions = database.parent().ok_or_else(|| {
        anyhow!(
            "Goose database has no sessions directory: {}",
            database.display()
        )
    })?;
    sessions.parent().map(Path::to_path_buf).ok_or_else(|| {
        anyhow!(
            "Goose database has no platform root: {}",
            database.display()
        )
    })
}
