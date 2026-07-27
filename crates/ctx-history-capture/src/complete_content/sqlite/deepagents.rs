//! DeepAgents compound write/message recovery within the shared SQLite snapshot broker.

use ctx_history_core::EventType;
use rusqlite::Connection;

use crate::complete_content::{
    CompleteContentError, CompleteContentErrorKind, CompleteMessageRequest, ResolvedResultContent,
    ResultContentRequest, SourceVerification, COMPLETE_CONTENT_MAX_BODY_BYTES,
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
        native_record_id: native_record_id(&resolved.event),
        provider_event_hash,
        normalized_payload_hash,
        record_digest: resolved.record_digest,
    })
}

pub(super) fn resolve_result(
    conn: &Connection,
    request: &ResultContentRequest,
    address: &provider::DeepAgentsContentAddress,
) -> Result<ResolvedResultContent, CompleteContentError> {
    let resolved = provider::resolve_deepagents_content(conn, address)
        .map_err(|cause| map_result_capture_error(request, cause))?
        .ok_or_else(|| {
            CompleteContentError::new(
                CompleteContentErrorKind::SourceRecordMissing,
                request.event_id,
            )
        })?;
    if !matches!(
        resolved.event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) || native_record_id(&resolved.event) != request.expected_native_record_id
        || resolved.record_digest != request.expected_record_digest
        || !request
            .expected_content_ref
            .verifies(resolved.text.as_bytes())
        || resolved.text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentVerificationFailed,
            request.event_id,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content: resolved.text,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
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

fn map_result_capture_error(
    request: &ResultContentRequest,
    cause: crate::CaptureError,
) -> CompleteContentError {
    let kind = match cause {
        crate::CaptureError::Io(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            CompleteContentErrorKind::SourceMissing
        }
        crate::CaptureError::Io(_) | crate::CaptureError::InvalidProviderTranscriptPath { .. } => {
            CompleteContentErrorKind::SourceUnreadable
        }
        crate::CaptureError::SourceChangedDuringCapture => CompleteContentErrorKind::SourceChanged,
        crate::CaptureError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(
                failure.code,
                rusqlite::ErrorCode::TooBig | rusqlite::ErrorCode::OperationInterrupted
            ) =>
        {
            CompleteContentErrorKind::ContentTooLarge
        }
        _ => CompleteContentErrorKind::ContentVerificationFailed,
    };
    CompleteContentError::new(kind, request.event_id)
}
