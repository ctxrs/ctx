//! DeepAgents compound write/message recovery within the shared SQLite snapshot broker.

use ctx_history_core::EventType;
use rusqlite::Connection;

use crate::complete_content::{
    CompleteContentError, CompleteContentErrorKind, CompleteMessageRequest,
};
use crate::provider::providers::deepagents as provider;

use super::{error, map_capture_error, native_record_id, ResolvedSqliteMessage};

pub(super) fn validate_message_request(
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    decode_address(request).map(|_| ())
}

pub(super) fn validate_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    provider::validate_deepagents_content_schema(conn)
        .map_err(|cause| map_capture_error(request, cause))
}

pub(super) fn resolve_message(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let address = decode_address(request)?;
    if request.provider_session_id.as_deref() != Some(address.thread_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let resolved = provider::resolve_deepagents_content(conn, &address)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if resolved.event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    let provider_event_hash = resolved.event.provider_event_hash.clone();
    let normalized_payload_hash = crate::compute_payload_hash(&resolved.event.payload).ok();
    Ok(ResolvedSqliteMessage {
        text: resolved.text,
        native_record_id: native_record_id(
            resolved.event.provider_event_index,
            resolved.event.provider_event_hash.as_deref(),
            Some(resolved.event.cursor.as_str()),
        ),
        provider_event_hash,
        normalized_payload_hash,
        record_digest: resolved.record_digest,
    })
}

fn decode_address(
    request: &CompleteMessageRequest,
) -> Result<provider::DeepAgentsContentAddress, CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != provider::DEEPAGENTS_CONTENT_LOCATOR_KIND {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    provider::decode_deepagents_content_address(locator.value())
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))
}
