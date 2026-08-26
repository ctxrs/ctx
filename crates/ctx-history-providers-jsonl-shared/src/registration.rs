use ctx_history_capture_model::ProviderSource;
use ctx_history_capture_runtime::{SourceBackedRouteSelection, SourceBackedSelectorAuthority};
use ctx_history_core::CaptureProvider;
use ctx_history_provider_runtime::{
    provider_jsonl_family_driver, ProviderJsonlRuntime, ProviderRouteRegistration,
    ProviderRuntimeBinding,
};

use crate::{provider, CaptureError, Result, CUSTOM_HISTORY_SOURCE_FORMAT};

/// Constructs the only valid Custom History route: an explicit v2 JSONL
/// source whose durable identity is supplied by the caller-owned catalog.
pub fn custom_history_explicit_route<B: ProviderRuntimeBinding>(
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> Result<ProviderRouteRegistration<B>> {
    if source.provider != CaptureProvider::Custom
        || source.source_format != CUSTOM_HISTORY_SOURCE_FORMAT
    {
        return Err(CaptureError::InvalidPayload(
            "Custom History routes require the ctx_history_jsonl_v2 source format".to_owned(),
        ));
    }

    let adapter = provider::custom_history_jsonl::custom_history_jsonl_family_adapter::<
        ProviderJsonlRuntime<B>,
    >(
        provider::custom_history_jsonl::CustomHistorySourceBackedInput::explicit(
            source.path.clone(),
            catalog_lineage,
        ),
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let driver = provider_jsonl_family_driver::<B>(adapter, source.path.clone());

    Ok(ProviderRouteRegistration {
        source,
        selection: SourceBackedRouteSelection::ExplicitManual,
        selector_authority: SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    })
}
