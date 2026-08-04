use std::path::Path;

use anyhow::Result;
use ctx_history_capture::{ProviderSource, SourceBackedRouteError, SourceBackedRouteErrorKind};

use crate::{provider_args::ImportFormatArg, ImportArgs};

pub(crate) use ctx_history_refresh::{
    relocate_explicit_source, upsert_explicit_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceRelocationAuthority,
};

pub(crate) fn explicit_source_for_import(args: &ImportArgs) -> Result<Option<ProviderSource>> {
    let Some(path) = args.path.as_deref() else {
        return Ok(None);
    };
    let provider = args.provider.map(|provider| provider.capture_provider());
    let custom_history_jsonl =
        matches!(args.input_format, Some(ImportFormatArg::CtxHistoryJsonlV1));
    ctx_history_refresh::explicit_source_for_path(path, provider, custom_history_jsonl).map(Some)
}

pub(crate) fn relocation_authority_for_import(
    data_root: &Path,
    old_path: &Path,
) -> Result<ExplicitSourceRelocationAuthority> {
    ctx_history_refresh::validate_explicit_relocation_source(old_path)?;
    crate::semantic::published_explicit_source_relocation_authority(data_root, old_path)?
        .ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unsupported,
                "relocation source is not the active exact catalog lineage/route",
            )
            .into()
        })
}

#[cfg(test)]
pub(crate) fn load_explicit_source_catalog_authority(
    _data_root: &Path,
) -> Result<ExplicitSourceCatalogAuthority> {
    Ok(ctx_history_refresh::explicit_source_catalog_authority_for_test(0))
}

#[cfg(test)]
pub(crate) fn explicit_source_catalog_authority_for_test(
    revision: u64,
) -> ExplicitSourceCatalogAuthority {
    ctx_history_refresh::explicit_source_catalog_authority_for_test(revision)
}
