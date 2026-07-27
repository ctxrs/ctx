//! Result-body address decoding, row recovery, and content verification.

use super::*;

#[derive(Debug)]
pub(crate) struct SqliteResultRecord {
    pub(crate) values: Vec<CapturedSqliteValue>,
    pub(crate) native_record_id: String,
    pub(crate) content: String,
}

pub(super) fn firebender_result_record(
    conn: &Connection,
    rowid: i64,
    subrecord_index: u32,
) -> crate::Result<Option<SqliteResultRecord>> {
    let has_deleted_at = sqlite_table_columns(conn, "chat_sessions")?.contains("deleted_at");
    let deleted_filter = if has_deleted_at {
        " and deleted_at is null"
    } else {
        ""
    };
    let sql = format!(
        "select id, name, cast(created_at as integer), cast(updated_at as integer), \
                messages_json, metadata_json from chat_sessions where rowid = ?1{deleted_filter}"
    );
    let Some(values) = conn
        .query_row(&sql, [rowid], |row| {
            Ok(vec![
                CapturedSqliteValue::Text(row.get(0)?),
                CapturedSqliteValue::Text(row.get(1)?),
                CapturedSqliteValue::Integer(row.get(2)?),
                CapturedSqliteValue::Integer(row.get(3)?),
                CapturedSqliteValue::Text(row.get(4)?),
                CapturedSqliteValue::Text(row.get(5)?),
            ])
        })
        .optional()?
    else {
        return Ok(None);
    };
    let [CapturedSqliteValue::Text(session_id), _, CapturedSqliteValue::Integer(created_at), _, CapturedSqliteValue::Text(messages_json), _] =
        values.as_slice()
    else {
        return Err(CaptureError::InvalidPayload(
            "Firebender result row has an invalid logical shape".to_owned(),
        ));
    };
    let messages = serde_json::from_str::<Value>(messages_json)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Firebender result row no longer has a message array".to_owned(),
            )
        })?;
    let index = subrecord_index as usize;
    let message = messages.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload("Firebender result message is missing".to_owned())
    })?;
    let fallback =
        DateTime::<Utc>::from_timestamp_millis(*created_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let occurred_at = firebender::firebender_message_time(message, fallback);
    let event =
        firebender::firebender_event(session_id, u64::from(subrecord_index), message, occurred_at);
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Err(CaptureError::InvalidPayload(
            "Firebender row is no longer a supported result".to_owned(),
        ));
    }
    let content = firebender::firebender_result_content(message).ok_or_else(|| {
        CaptureError::InvalidPayload("Firebender result has no normalized content".to_owned())
    })?;
    Ok(Some(SqliteResultRecord {
        values,
        native_record_id: native_record_id(&event),
        content,
    }))
}

pub(super) fn resolve_one_result(
    conn: &Connection,
    request: &ResultContentRequest,
) -> Result<ResolvedResultContent, CompleteContentError> {
    let coordinate = decode_result_locator(request)?;
    if matches!(coordinate, SqliteResultCoordinate::Zed) {
        return resolve_zed_result(conn, request);
    }
    let record = match coordinate {
        SqliteResultCoordinate::DeepAgents(address) => {
            return deepagents::resolve_result(conn, request, &address);
        }
        SqliteResultCoordinate::Warp {
            rowid,
            message_index,
        } => {
            return warp_result::resolve_result(conn, request, rowid, message_index);
        }
        SqliteResultCoordinate::Shelley {
            parent_bearing,
            message_rowid,
            conversation_rowid,
        } => shelley_result_record(conn, parent_bearing, message_rowid, conversation_rowid),
        SqliteResultCoordinate::Firebender(rowid) => {
            firebender_result_record(conn, rowid, request.source_record_subrecord_index)
        }
        SqliteResultCoordinate::Hermes(rowid) => hermes::hermes_result_record(conn, rowid),
        SqliteResultCoordinate::ForgeCode(rowid) => {
            forgecode::forgecode_result_record(conn, rowid, request.source_record_subrecord_index)
        }
        SqliteResultCoordinate::OpenCode { shape, rowid } => {
            opencode::opencode_result_record(conn, shape, rowid)
        }
        SqliteResultCoordinate::Crush(rowid) => crush::crush_result_record(conn, rowid),
        SqliteResultCoordinate::Goose(rowid) => goose::goose_result_record(conn, rowid),
        SqliteResultCoordinate::Zed => unreachable!("Zed is resolved from its compound row"),
    }
    .map_err(|cause| map_result_capture_error(request, cause))?
    .ok_or_else(|| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if record.content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentTooLarge,
        ));
    }
    if record.native_record_id != request.expected_native_record_id
        || sqlite_logical_record_digest(&record.values) != request.expected_record_digest
        || !request
            .expected_content_ref
            .verifies(record.content.as_bytes())
    {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content: record.content,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

#[derive(Debug, Clone)]
pub(super) enum SqliteResultCoordinate {
    DeepAgents(crate::provider::providers::deepagents::DeepAgentsContentAddress),
    Warp {
        rowid: i64,
        message_index: usize,
    },
    Shelley {
        parent_bearing: bool,
        message_rowid: i64,
        conversation_rowid: i64,
    },
    Firebender(i64),
    Hermes(i64),
    ForgeCode(i64),
    OpenCode {
        shape: u8,
        rowid: i64,
    },
    Crush(i64),
    Goose(i64),
    Zed,
}

pub(super) fn decode_result_locator(
    request: &ResultContentRequest,
) -> Result<SqliteResultCoordinate, CompleteContentError> {
    let locator = &request.source_locator;
    let invalid = || result_error(request, CompleteContentErrorKind::ContentVerificationFailed);
    match (request.provider, locator.kind(), locator.value()) {
        (CaptureProvider::Warp, WARP_LOCATOR_KIND, bytes) if bytes.len() == 12 => {
            let rowid = decode_i64(&bytes[..8]).ok_or_else(invalid)?;
            let message_index =
                u32::from_be_bytes(bytes[8..].try_into().map_err(|_| invalid())?) as usize;
            Ok(SqliteResultCoordinate::Warp {
                rowid,
                message_index,
            })
        }
        (CaptureProvider::Shelley, SHELLEY_LOCATOR_KIND, bytes) => {
            let (parent_bearing, message_rowid, conversation_rowid) =
                decode_shelley_coordinate(bytes).ok_or_else(invalid)?;
            Ok(SqliteResultCoordinate::Shelley {
                parent_bearing,
                message_rowid,
                conversation_rowid,
            })
        }
        (
            CaptureProvider::DeepAgents,
            crate::provider::providers::deepagents::DEEPAGENTS_CONTENT_LOCATOR_KIND,
            bytes,
        ) => crate::provider::providers::deepagents::decode_deepagents_content_address(bytes)
            .map(SqliteResultCoordinate::DeepAgents)
            .ok_or_else(invalid),
        (CaptureProvider::Firebender, FIREBENDER_LOCATOR_KIND, bytes) if bytes.len() == 8 => Ok(
            SqliteResultCoordinate::Firebender(decode_i64(bytes).ok_or_else(invalid)?),
        ),
        (CaptureProvider::Hermes, HERMES_LOCATOR_KIND, [2, bytes @ ..])
            if bytes.len() == 8 && request.source_record_subrecord_index == 0 =>
        {
            Ok(SqliteResultCoordinate::Hermes(
                decode_i64(bytes).ok_or_else(invalid)?,
            ))
        }
        (CaptureProvider::ForgeCode, FORGECODE_LOCATOR_KIND, bytes) if bytes.len() == 8 => Ok(
            SqliteResultCoordinate::ForgeCode(decode_i64(bytes).ok_or_else(invalid)?),
        ),
        (
            CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode,
            OPENCODE_LOCATOR_KIND,
            [shape @ 1..=4, bytes @ ..],
        ) if bytes.len() == 9
            && bytes.last() == Some(&2)
            && request.source_record_subrecord_index == 0 =>
        {
            Ok(SqliteResultCoordinate::OpenCode {
                shape: *shape,
                rowid: decode_ordered_i64(&bytes[..8]).ok_or_else(invalid)?,
            })
        }
        (CaptureProvider::Crush, CRUSH_LOCATOR_KIND, [2, bytes @ ..])
            if bytes.len() == 8 && request.source_record_subrecord_index == 0 =>
        {
            Ok(SqliteResultCoordinate::Crush(
                decode_ordered_i64(bytes).ok_or_else(invalid)?,
            ))
        }
        (CaptureProvider::Goose, GOOSE_LOCATOR_KIND, [2, bytes @ ..])
            if bytes.len() == 8 && request.source_record_subrecord_index == 0 =>
        {
            Ok(SqliteResultCoordinate::Goose(
                decode_ordered_i64(bytes).ok_or_else(invalid)?,
            ))
        }
        (CaptureProvider::Zed, ZED_LOCATOR_KIND, bytes) if bytes.len() == 8 => {
            decode_i64(bytes).ok_or_else(invalid)?;
            Ok(SqliteResultCoordinate::Zed)
        }
        _ => Err(invalid()),
    }
}

pub(super) fn resolve_zed_result(
    conn: &Connection,
    request: &ResultContentRequest,
) -> Result<ResolvedResultContent, CompleteContentError> {
    let shim = result_request_shim(request);
    let values = zed_values(conn, &shim)?;
    if sqlite_logical_record_digest(&values) != request.expected_record_digest {
        return Err(result_error(
            request,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    let row = zed::decode_zed_thread_for_complete(&values)
        .map_err(|cause| map_capture_error(&shim, cause))?;
    let decoded =
        zed::decode_zed_thread_events(&row).map_err(|cause| map_capture_error(&shim, cause))?;
    let event_index = usize::try_from(request.source_record_subrecord_index)
        .map_err(|_| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let decoded_event = decoded
        .event_at(&row.id, event_index)
        .map_err(|cause| map_capture_error(&shim, cause))?
        .ok_or_else(|| result_error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if !matches!(
        decoded_event.event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) || native_record_id(&decoded_event.event) != request.expected_native_record_id
    {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let content = zed::zed_result_content(decoded_event.message).ok_or_else(|| {
        result_error(request, CompleteContentErrorKind::ContentVerificationFailed)
    })?;
    if content.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentTooLarge,
        ));
    }
    if !request.expected_content_ref.verifies(content.as_bytes()) {
        return Err(result_error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(ResolvedResultContent {
        event_id: request.event_id,
        content,
        content_ref: request.expected_content_ref.clone(),
        verification: SourceVerification::VERIFIED,
    })
}

pub(super) fn decode_i64(bytes: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(bytes.try_into().ok()?))
}

pub(super) fn decode_ordered_i64(bytes: &[u8]) -> Option<i64> {
    Some((u64::from_be_bytes(bytes.try_into().ok()?) ^ (1_u64 << 63)) as i64)
}

pub(super) fn sqlite_result_profile(
    provider: CaptureProvider,
    source_format: &str,
) -> Option<&'static str> {
    verified_content_profile(
        provider,
        source_format,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::ResultBody,
    )
}

pub(super) fn result_request_shim(request: &ResultContentRequest) -> CompleteMessageRequest {
    CompleteMessageRequest {
        event_id: request.event_id,
        provider: request.provider,
        source_format: request.source_format.clone(),
        source_access: request.source_access.clone(),
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: request.content_profile.clone(),
        source_locator: Some(request.source_locator.clone()),
        provider_session_id: None,
        source_record_ordinal: request.source_record_ordinal,
        source_record_subrecord_index: request.source_record_subrecord_index,
        expected_provider_event_hash: String::new(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(request.expected_native_record_id.clone()),
        expected_record_digest: Some(request.expected_record_digest.clone()),
        expected_content_ref: Some(request.expected_content_ref.clone()),
        indexed_text: String::new(),
        indexed_limit_chars: 0,
    }
}
