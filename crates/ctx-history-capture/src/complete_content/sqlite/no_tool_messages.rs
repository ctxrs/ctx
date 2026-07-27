//! Exact message reopening for providers whose source exposes no separate result events.

use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use rusqlite::Connection;
use serde_json::Value;

use crate::{
    captured_batch::{CapturedSqliteValue, NativeLocator},
    compute_payload_hash,
    provider::providers::{astrbot, lingma, trae},
    CaptureError,
};

use super::{
    attach_verified_content_locator, error, map_capture_error, native_record_id,
    resolved_from_values, verified_content_profile, CompleteContentBodyDigest,
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    CompleteMessageRequest, ResolvedSqliteMessage, VerifiedContentLocatorV1, VerifiedContentRole,
};

const ASTRBOT_LOCATOR_KIND: &str = "astrbot-conversation-message-v1";
pub(super) const LINGMA_LOCATOR_KIND: &str = "lingma-chat-record-v1";
const TRAE_LOCATOR_KIND: &str = "trae-itemtable-message-v1";

pub(super) fn validate_locator(
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    match request.provider {
        CaptureProvider::AstrBot => decode_astrbot_coordinate(request).map(|_| ()),
        CaptureProvider::Lingma => decode_lingma_rowid(request).map(|_| ()),
        CaptureProvider::Trae => decode_trae_coordinate(request).map(|_| ()),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }
}

pub(super) fn resolve(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    match request.provider {
        CaptureProvider::AstrBot => resolve_astrbot(conn, request),
        CaptureProvider::Lingma => resolve_lingma(conn, request),
        CaptureProvider::Trae => resolve_trae(conn, request),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }
}

fn resolve_astrbot(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (rowid, item_index) = decode_astrbot_coordinate(request)?;
    let values = astrbot::astrbot_complete_conversation_values(conn, rowid)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let (event, text, provider_session_id) =
        astrbot::astrbot_complete_conversation_message(&values, item_index)
            .map_err(|cause| map_capture_error(request, cause))?
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if request.provider_session_id.as_deref() != Some(provider_session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_values(event, text, &values))
}

fn resolve_lingma(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_lingma_rowid(request)?;
    let values = lingma::lingma_complete_values(conn, rowid)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let session_id = match values.get(1) {
        Some(CapturedSqliteValue::Text(value)) => value.as_str(),
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
    };
    if request.provider_session_id.as_deref() != Some(session_id) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let (event, text) = lingma::lingma_complete_user_message(&values)
        .map_err(|cause| map_capture_error(request, cause))?;
    Ok(resolved_from_values(event, text, &values))
}

fn resolve_trae(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (key_index, session_index, message_index) = decode_trae_coordinate(request)?;
    let bytes = trae::trae_complete_value(conn, key_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let provider_session_id = request
        .provider_session_id
        .as_deref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    let (event, text) = trae::trae_complete_message(
        &bytes,
        key_index,
        session_index,
        message_index,
        provider_session_id,
    )
    .map_err(|cause| map_capture_error(request, cause))?
    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let native_record_id = native_record_id(&event);
    Ok(ResolvedSqliteMessage {
        text,
        provider_event_hash: event.provider_event_hash.clone(),
        normalized_payload_hash: compute_payload_hash(&event.payload).ok(),
        native_record_id,
        record_digest: CompleteContentBodyDigest::from_bytes(&bytes),
    })
}

fn locator_value<'a>(
    request: &'a CompleteMessageRequest,
    expected_kind: &str,
    expected_len: usize,
) -> Result<&'a [u8], CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != expected_kind || locator.value().len() != expected_len {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(locator.value())
}

fn decode_lingma_rowid(request: &CompleteMessageRequest) -> Result<i64, CompleteContentError> {
    let encoded = u64::from_be_bytes(
        locator_value(request, LINGMA_LOCATOR_KIND, 8)?
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((encoded ^ (1_u64 << 63)) as i64)
}

fn decode_trae_coordinate(
    request: &CompleteMessageRequest,
) -> Result<(u16, u32, u32), CompleteContentError> {
    let value = locator_value(request, TRAE_LOCATOR_KIND, 10)?;
    let key = u16::from_be_bytes(
        value[..2]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    let session = u32::from_be_bytes(
        value[2..6]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    let message = u32::from_be_bytes(
        value[6..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((key, session, message))
}

fn decode_astrbot_coordinate(
    request: &CompleteMessageRequest,
) -> Result<(i64, u32), CompleteContentError> {
    let value = locator_value(request, ASTRBOT_LOCATOR_KIND, 12)?;
    let ordered = u64::from_be_bytes(
        value[..8]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    let item = u32::from_be_bytes(
        value[8..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok(((ordered ^ (1_u64 << 63)) as i64, item))
}

pub(crate) fn attach_sqlite_native_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    record_digest: &CompleteContentBodyDigest,
    complete_text: &str,
) -> crate::Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported SQLite message route must have a verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id(event),
        record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}
