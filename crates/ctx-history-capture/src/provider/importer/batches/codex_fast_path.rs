use ctx_history_core::ProviderCaptureEnvelope;
use ctx_history_store::Store;

use crate::{CaptureError, Result};

use super::super::ProviderImportCaches;
use super::write_tx::{serialized_len_or_rollback, ProviderImportTransaction};

const PROVIDER_EVENT_FIELD_JSON_BYTES: usize = br#","event":"#.len();
// An eventless Codex capture is stable for one exact source-scoped session
// except for the decimal line cursor and its RFC 3339 observation timestamp.
// This covers their maximum serialized growth while retaining a conservative
// transaction byte budget instead of serializing the identical source/session
// metadata for every event.
const CODEX_EVENTLESS_CAPTURE_VARIATION_BYTES: usize = 128;

pub(super) fn codex_existing_session_unit_bytes(
    store: &Store,
    transaction: &mut ProviderImportTransaction,
    caches: &mut ProviderImportCaches,
    source_id: uuid::Uuid,
    capture: &mut ProviderCaptureEnvelope,
) -> Result<usize> {
    let event_bytes = serialized_len_or_rollback(
        transaction,
        store,
        capture.event.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex existing-session projection lost its mandatory event",
        ))?,
    )?;
    let eventless_budget = match caches.codex_eventless_capture_byte_budgets.get(&source_id) {
        Some(bytes) => *bytes,
        None => {
            let event = capture.event.take().ok_or(CaptureError::SystemInvariant(
                "Codex existing-session projection lost its mandatory event",
            ))?;
            let eventless_result = serialized_len_or_rollback(transaction, store, capture);
            capture.event = Some(event);
            let eventless_budget = eventless_result?
                .checked_add(CODEX_EVENTLESS_CAPTURE_VARIATION_BYTES)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex eventless capture byte budget overflowed",
                ))?;
            caches
                .codex_eventless_capture_byte_budgets
                .insert(source_id, eventless_budget);
            eventless_budget
        }
    };
    let unit_bytes = eventless_budget
        .checked_add(PROVIDER_EVENT_FIELD_JSON_BYTES)
        .and_then(|bytes| bytes.checked_add(event_bytes))
        .ok_or(CaptureError::SystemInvariant(
            "Codex projected unit serialized length overflowed",
        ))?;
    #[cfg(test)]
    {
        let full_bytes = serialized_len_or_rollback(transaction, store, capture)?;
        if unit_bytes < full_bytes {
            transaction.rollback(store);
            return Err(CaptureError::SystemInvariant(
                "Codex projected unit byte accounting diverged from serialization",
            ));
        }
    }
    Ok(unit_bytes)
}
