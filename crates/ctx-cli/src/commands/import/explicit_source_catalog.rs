use std::path::Path;

use anyhow::Result;
use ctx_history_capture::{ProviderSource, SourceBackedRouteError, SourceBackedRouteErrorKind};

use ctx_history_core::CaptureProvider;

pub(crate) use ctx_history_refresh::{
    explicit_source_for_path_with_format, relocate_explicit_source, upsert_explicit_source,
    ExplicitInputFormat, ExplicitSourceRelocationAuthority,
};

pub(crate) fn explicit_source_for_admission(
    data_root: &Path,
    path: &Path,
    provider: Option<CaptureProvider>,
    input_format: Option<ExplicitInputFormat>,
) -> Result<ProviderSource> {
    explicit_source_for_path_with_format(data_root, path, provider, input_format)
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
