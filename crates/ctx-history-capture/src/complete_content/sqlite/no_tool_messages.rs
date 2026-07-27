//! Exact message reopening for providers whose source exposes no separate result events.

use ctx_history_core::{CaptureProvider, EventType};
use rusqlite::Connection;

use crate::{
    compute_payload_hash,
    native_source::NativeSqliteValue,
    provider::providers::{astrbot, lingma, trae},
};

use super::{
    error, map_capture_error, native_record_id, resolved_from_event_fields,
    CompleteContentBodyDigest, CompleteContentError, CompleteContentErrorKind,
    CompleteMessageRequest, ResolvedSqliteMessage,
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
    let message = astrbot::astrbot_complete_conversation_message(&values, item_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if request.provider_session_id.as_deref() != Some(message.provider_session_id.as_str())
        || message.event_type != EventType::Message
    {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_event_fields(
        message.provider_event_index,
        message.provider_event_hash.as_deref(),
        Some(&message.cursor),
        &message.payload,
        message.text,
        &values,
    ))
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
        Some(NativeSqliteValue::Text(value)) => value.as_str(),
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
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_event_fields(
        event.provider_event_index,
        Some(&event.provider_event_hash),
        Some(&event.cursor),
        &event.payload,
        text,
        &values,
    ))
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
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let native_record_id = native_record_id(
        event.provider_event_index,
        Some(&event.provider_event_hash),
        Some(&event.cursor),
    );
    Ok(ResolvedSqliteMessage {
        text,
        provider_event_hash: Some(event.provider_event_hash),
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
