use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope, ProviderSourceEnvelope};
use ctx_history_store::Store;
use uuid::Uuid;

use crate::{
    stable_capture_uuid, NormalizedProviderImportOptions, ProviderFileTouchedEnvelope,
    ProviderImportSummary, Result,
};

use super::ids::{provider_scoped_source_uuid, provider_source_identity};
use super::{import_provider_capture_line_with_canonical_source, ProviderImportCaches};

#[derive(Debug, Clone)]
pub(crate) struct CanonicalProviderSourceOverride {
    pub(super) stable_source_identity: String,
    pub(super) stable_session_identity: String,
    pub(super) machine_id: String,
    pub(super) uses_relocation_alias: bool,
}

pub(crate) fn import_provider_capture_line(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
) -> Result<ProviderImportSummary> {
    import_provider_capture_line_with_canonical_source(
        store,
        capture,
        options,
        line_number,
        caches,
        None,
    )
}

pub(super) fn provider_import_source_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source: &ProviderSourceEnvelope,
    canonical: Option<&CanonicalProviderSourceOverride>,
) -> Result<(Uuid, Option<String>)> {
    let default_identity = provider_source_identity(
        provider,
        &source.source_format,
        source.source_root.as_deref(),
        source.raw_source_path.as_deref(),
        source.idempotency_key.as_deref(),
        &source.metadata,
    );
    let Some(canonical) = canonical else {
        return Ok((
            provider_scoped_source_uuid(
                provider,
                provider_session_id,
                &source.source_format,
                source.raw_source_path.as_deref(),
            ),
            default_identity,
        ));
    };
    // An ordinary, non-relocated source keeps the same path-scoped identifier
    // that provider imports have always used. Looking it up by its canonical
    // identity first only rehydrates a large CaptureSource row once per event;
    // it cannot change the deterministic answer. Relocated sources still need
    // the lookup so an alias reuses the already-persisted source identifier.
    if !canonical.uses_relocation_alias {
        return Ok((
            provider_scoped_source_uuid(
                provider,
                provider_session_id,
                &source.source_format,
                source.raw_source_path.as_deref(),
            ),
            Some(canonical.stable_source_identity.clone()),
        ));
    }
    if let Some(existing) = store.capture_source_by_canonical_identity_session(
        provider,
        &source.source_format,
        &canonical.machine_id,
        &canonical.stable_source_identity,
        provider_session_id,
    )? {
        return Ok((existing.id, Some(canonical.stable_source_identity.clone())));
    }
    let source_id = if canonical.uses_relocation_alias {
        stable_capture_uuid(
            &serde_json::to_string(&(
                "provider-relocated-source-v1",
                provider.as_str(),
                &source.source_format,
                &canonical.stable_source_identity,
                provider_session_id,
            ))?,
            "source",
        )
    } else {
        provider_scoped_source_uuid(
            provider,
            provider_session_id,
            &source.source_format,
            source.raw_source_path.as_deref(),
        )
    };
    Ok((source_id, Some(canonical.stable_source_identity.clone())))
}

pub(super) fn provider_file_touch_source_id(
    store: &Store,
    file: &ProviderFileTouchedEnvelope,
    canonical: Option<&CanonicalProviderSourceOverride>,
) -> Result<Uuid> {
    let Some(canonical) = canonical else {
        return Ok(provider_scoped_source_uuid(
            file.provider,
            &file.provider_session_id,
            &file.source_format,
            file.raw_source_path.as_deref(),
        ));
    };
    if let Some(source) = store.capture_source_by_canonical_identity_session(
        file.provider,
        &file.source_format,
        &canonical.machine_id,
        &canonical.stable_source_identity,
        &file.provider_session_id,
    )? {
        return Ok(source.id);
    }
    if canonical.uses_relocation_alias {
        return Ok(stable_capture_uuid(
            &serde_json::to_string(&(
                "provider-relocated-source-v1",
                file.provider.as_str(),
                &file.source_format,
                &canonical.stable_source_identity,
                &file.provider_session_id,
            ))?,
            "source",
        ));
    }
    Ok(provider_scoped_source_uuid(
        file.provider,
        &file.provider_session_id,
        &file.source_format,
        file.raw_source_path.as_deref(),
    ))
}
