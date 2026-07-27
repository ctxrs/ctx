use ctx_history_core::CaptureProvider;
use ctx_history_store::{Store, StoreError};
use uuid::Uuid;

use crate::{CaptureError, Result};

mod commands;
mod cursors;
mod identity;
mod ids;
mod legacy_identity;

pub(crate) use crate::pro_output::{OutputCommandContext, OutputOutcomeMetadata};
pub(crate) use commands::{compact_provider_result_payload, provider_command_run};
#[cfg(test)]
pub(crate) use cursors::released_jsonl_initial_position_for_test;
pub(crate) use cursors::{
    certified_provider_sync_cursor, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CertifiedProviderCursor,
};
#[cfg(test)]
pub(crate) use identity::provider_event_import_identity;
pub(crate) use identity::{
    avoid_provider_source_event_seq_collision,
    provider_event_import_identity_with_exact_legacy_source, provider_file_touch_event_id,
    provider_file_touch_import_id, provider_native_event_import_identity_migrating_legacy_hash,
    provider_source_event_import_identity, ExactLegacySourceEventCandidate,
    ProviderEventImportIdentity,
};
#[cfg(test)]
pub(crate) use ids::provider_source_root_identity;
pub(crate) use ids::{
    provider_edge_uuid, provider_scoped_source_identity_key, provider_scoped_source_uuid,
    provider_session_uuid, provider_source_edge_uuid, provider_source_identity,
    provider_source_root, provider_source_session_uuid, provider_sync_metadata, timestamps,
};
#[cfg(test)]
pub(crate) use ids::{provider_source_event_seq, provider_source_event_uuid};
use legacy_identity::legacy_session_matches_source;

pub(crate) fn provider_import_session_uuid(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    source_identity: Option<&str>,
) -> Result<Uuid> {
    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    let Some(source_identity) = source_identity else {
        return Ok(legacy_session_id);
    };
    if provider == CaptureProvider::Custom {
        return Ok(legacy_session_id);
    }

    if let Some(existing) = store.session_by_capture_source_and_external_session(
        source_id,
        provider,
        provider_session_id,
    )? {
        return Ok(existing.id);
    }

    let source_session_id = provider_source_session_uuid(source_identity, provider_session_id);
    match store.get_session(source_session_id) {
        Ok(_) => return Ok(source_session_id),
        Err(StoreError::NotFound(_)) => {}
        Err(err) => return Err(CaptureError::Store(err)),
    }

    match store.get_session(legacy_session_id) {
        Ok(existing)
            if legacy_session_matches_source(store, &existing, source_id, source_identity)? =>
        {
            Ok(legacy_session_id)
        }
        Ok(_) => Ok(source_session_id),
        Err(StoreError::NotFound(_)) => Ok(source_session_id),
        Err(err) => Err(CaptureError::Store(err)),
    }
}
