use ctx_history_core::ProviderCaptureEnvelope;
use ctx_history_store::Store;
use uuid::Uuid;

use crate::{CaptureError, Result};

use super::source_relocation::{provider_import_source_id, CanonicalProviderSourceOverride};
use super::ProviderImportCaches;

pub(super) fn resolve_provider_existing_session_identity(
    store: &Store,
    line_number: usize,
    capture: &ProviderCaptureEnvelope,
    caches: &mut ProviderImportCaches,
    canonical_source: Option<&CanonicalProviderSourceOverride>,
) -> Result<(Uuid, Uuid, bool)> {
    let provider = capture.provider;
    let session = &capture.session;
    let source = &capture.source;
    let (source_id, _) = provider_import_source_id(
        store,
        provider,
        &session.provider_session_id,
        source,
        canonical_source,
    )?;
    if let Some(session_id) = caches.resolved_existing_sessions.get(&source_id) {
        return Ok((source_id, *session_id, false));
    }
    if let Some((session_id, _)) = caches.processed_sessions.iter().find(|(_, processed)| {
        processed.capture_source_id == Some(source_id)
            && processed.provider == provider
            && processed.external_session_id.as_deref()
                == Some(session.provider_session_id.as_str())
    }) {
        let session_id = *session_id;
        caches
            .resolved_existing_sessions
            .insert(source_id, session_id);
        return Ok((source_id, session_id, false));
    }
    let existing = store
        .session_by_capture_source_and_external_session(
            source_id,
            provider,
            &session.provider_session_id,
        )?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "provider event on line {line_number} references a session that is not already persisted for its exact source"
            ))
        })?;
    if existing.capture_source_id != Some(source_id)
        || existing.provider != provider
        || existing.external_session_id.as_deref() != Some(session.provider_session_id.as_str())
    {
        return Err(CaptureError::SystemInvariant(
            "exact source-scoped provider session lookup returned a mismatched session",
        ));
    }

    caches
        .resolved_existing_sessions
        .insert(source_id, existing.id);
    Ok((source_id, existing.id, true))
}
