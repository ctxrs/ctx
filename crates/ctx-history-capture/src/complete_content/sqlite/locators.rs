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

pub(super) fn optional_integer(value: Option<i64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)
}

pub(super) fn optional_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
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
/// actually truncated. NativePath publishers call this while the exact logical
/// SQLite row and complete text are still available.
// These arguments are the persisted locator contract and should remain explicit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn attach_sqlite_complete_content_locator(
    provider: CaptureProvider,
    source_format: &str,
    native_record_id: &str,
    payload: &Value,
    metadata: &mut Value,
    locator: &NativeLocator,
    digest_values: &[NativeSqliteValue],
    complete_text: impl FnOnce() -> String,
) -> crate::Result<()> {
    if payload
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
        native_record_id.to_owned(),
        record_digest,
    )
    .ok_or(CaptureError::SystemInvariant(
        "SQLite complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(metadata, persisted).ok_or(CaptureError::SystemInvariant(
        "verified-content locator collection is malformed",
    ))?;
    Ok(())
}

pub(super) fn sqlite_logical_record_digest(
    values: &[NativeSqliteValue],
) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}
