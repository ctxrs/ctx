//! Path-free SQLite locator codecs, verified identities, and capture attachment.

use super::*;

pub(super) fn decode_raw_rowid(
    request: &CompleteMessageRequest,
    expected_kind: &str,
) -> Result<i64, CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != expected_kind || locator.value().len() != 8 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let bytes: [u8; 8] = locator
        .value()
        .try_into()
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    Ok(i64::from_be_bytes(bytes))
}

pub(super) fn decode_phased_ordered_rowid(
    request: &CompleteMessageRequest,
    expected_kind: &str,
) -> Result<i64, CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != expected_kind || locator.value().len() != 9 || locator.value()[0] != 2 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let encoded = u64::from_be_bytes(
        locator.value()[1..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((encoded ^ (1_u64 << 63)) as i64)
}

pub(super) fn decode_phased_raw_rowid(
    request: &CompleteMessageRequest,
    expected_kind: &str,
) -> Result<i64, CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != expected_kind || locator.value().len() != 9 || locator.value()[0] != 2 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let bytes: [u8; 8] = locator.value()[1..]
        .try_into()
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    Ok(i64::from_be_bytes(bytes))
}

pub(super) fn decode_opencode_locator(
    request: &CompleteMessageRequest,
) -> Result<(opencode::OpenCodeCapturedShape, i64), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != OPENCODE_LOCATOR_KIND {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let native = NativeLocator::new(locator.kind(), locator.value().to_vec())
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    opencode::decode_opencode_message_locator(&native)
        .map_err(|cause| map_capture_error(request, cause))
}

pub(super) fn decode_warp_message_coordinate(
    request: &CompleteMessageRequest,
) -> Result<(i64, usize), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    let value = locator.value();
    if locator.kind() != WARP_LOCATOR_KIND || value.len() != 12 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let rowid = i64::from_be_bytes(
        value[..8]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    let index = u32::from_be_bytes(
        value[8..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((rowid, index as usize))
}

pub(super) fn decode_shelley_locator(
    request: &CompleteMessageRequest,
) -> Result<(bool, i64, i64), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != SHELLEY_LOCATOR_KIND {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    decode_shelley_coordinate(locator.value())
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))
}

pub(super) fn decode_shelley_coordinate(value: &[u8]) -> Option<(bool, i64, i64)> {
    if value.len() != 17 {
        return None;
    }
    let parent_bearing = match value[0] {
        1 => true,
        2 => false,
        _ => return None,
    };
    Some((
        parent_bearing,
        decode_ordered_i64(&value[1..9])?,
        decode_ordered_i64(&value[9..17])?,
    ))
}

pub(super) fn decode_nanoclaw_locator(
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    let native = NativeLocator::new(locator.kind(), locator.value().to_vec())
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    nanoclaw::decode_nanoclaw_message_locator(&native)
        .map(|_| ())
        .map_err(|cause| map_capture_error(request, cause))
}

pub(super) fn decode_kiro_rowid(
    request: &CompleteMessageRequest,
) -> Result<(&'static str, i64), CompleteContentError> {
    let locator = request
        .source_locator
        .as_ref()
        .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
    if locator.kind() != KIRO_LOCATOR_KIND || locator.value().len() != 9 {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let table = match locator.value()[0] {
        1 => "conversations_v2",
        2 => "conversations",
        _ => {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
    };
    let encoded = u64::from_be_bytes(
        locator.value()[1..]
            .try_into()
            .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?,
    );
    Ok((table, (encoded ^ (1_u64 << 63)) as i64))
}

pub(super) fn optional_integer(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

pub(super) fn optional_text(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

pub(super) fn optional_column<'a>(
    columns: &std::collections::BTreeSet<String>,
    name: &'a str,
) -> &'a str {
    if columns.contains(name) {
        name
    } else {
        "NULL"
    }
}

/// Adds the bounded local-only locator only when the canonical message text was
/// actually truncated. Provider projectors call this while the exact logical
/// SQLite row and complete text are still available.
pub(crate) fn attach_sqlite_complete_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    values: &[CapturedSqliteValue],
    complete_text: impl FnOnce() -> String,
) -> crate::Result<()> {
    attach_sqlite_complete_content_locator_with_digest_values(
        event,
        provider,
        source_format,
        locator,
        values,
        complete_text,
    )
}

pub(crate) fn attach_sqlite_complete_content_locator_with_digest_values(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    digest_values: &[CapturedSqliteValue],
    complete_text: impl FnOnce() -> String,
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
    let complete_text = complete_text();
    let record_digest = sqlite_logical_record_digest(digest_values);
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
        record_digest,
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

/// Attaches a source-backed result locator and one shared content identity.
/// Provider projectors call this while the exact logical row and full result
/// body are transiently available; no result bytes are persisted.
pub(crate) fn attach_sqlite_result_content_locator(
    event: &mut ProviderEventEnvelope,
    provider: CaptureProvider,
    source_format: &str,
    locator: &NativeLocator,
    values: &[CapturedSqliteValue],
    complete_content: Option<String>,
) -> crate::Result<()> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Ok(());
    }
    let Some(complete_content) = complete_content else {
        return Ok(());
    };
    if complete_content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_content.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite result length exceeds ContentRef bounds"),
    )?;
    let mut payload = event.payload.clone();
    let payload_object = payload
        .as_object_mut()
        .ok_or(CaptureError::SystemInvariant(
            "provider result payload must be an object",
        ))?;
    payload_object.insert(
        "result_content_ref".to_owned(),
        serde_json::to_value(&content_ref).map_err(CaptureError::Json)?,
    );
    let profile =
        sqlite_result_profile(provider, source_format).ok_or(CaptureError::SystemInvariant(
            "supported SQLite result route must have a verified-content profile",
        ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::ResultBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        native_record_id(event),
        sqlite_logical_record_digest(values),
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite result locator exceeds the bounded canonical schema",
    ))?;
    let mut metadata = event.metadata.clone();
    attach_verified_content_locator(&mut metadata, persisted).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    event.payload = payload;
    event.metadata = metadata;
    Ok(())
}

/// Attaches Shelley message/result addresses while the exact message and its
/// relationship parent are both available.
pub(crate) fn attach_shelley_content_locator(
    event: &mut ProviderEventEnvelope,
    message: &shelley::ShelleyMessageRow,
    conversation: &shelley::ShelleyConversationRow,
    parent_bearing: bool,
    complete_text: &str,
) -> crate::Result<()> {
    let mut coordinate = Vec::with_capacity(17);
    coordinate.push(if parent_bearing { 1 } else { 2 });
    coordinate.extend_from_slice(&(message.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    coordinate.extend_from_slice(&(conversation.rowid as u64 ^ (1_u64 << 63)).to_be_bytes());
    let locator = NativeLocator::new(SHELLEY_LOCATOR_KIND, coordinate).map_err(|_| {
        CaptureError::SystemInvariant("Shelley content coordinate exceeds native locator bounds")
    })?;
    let values = shelley::shelley_verified_record_values(message, conversation, parent_bearing);
    if event.event_type == EventType::Message {
        attach_sqlite_complete_content_locator_with_digest_values(
            event,
            CaptureProvider::Shelley,
            crate::SHELLEY_SQLITE_SOURCE_FORMAT,
            &locator,
            &values,
            || complete_text.to_owned(),
        )
    } else {
        attach_sqlite_result_content_locator(
            event,
            CaptureProvider::Shelley,
            crate::SHELLEY_SQLITE_SOURCE_FORMAT,
            &locator,
            &values,
            Some(complete_text.to_owned()),
        )
    }
}

pub(super) fn sqlite_logical_record_digest(
    values: &[CapturedSqliteValue],
) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            CapturedSqliteValue::Null => digest.update([0]),
            CapturedSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            CapturedSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            CapturedSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            CapturedSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}
