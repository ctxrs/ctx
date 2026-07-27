//! Provider-specific message resolution from one admitted SQLite snapshot.

use super::*;

pub(super) fn resolve_one(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<CompleteMessage, CompleteContentError> {
    let resolved = match request.provider {
        CaptureProvider::Firebender => resolve_firebender(conn, request),
        CaptureProvider::AstrBot | CaptureProvider::Lingma | CaptureProvider::Trae => {
            no_tool_messages::resolve(conn, request)
        }
        CaptureProvider::KiroCli => resolve_kiro(conn, request),
        CaptureProvider::Zed => resolve_zed(conn, request),
        CaptureProvider::ForgeCode => resolve_forgecode(conn, request),
        CaptureProvider::Crush => resolve_crush(conn, request),
        CaptureProvider::Goose => resolve_goose(conn, request),
        CaptureProvider::Hermes => resolve_hermes(conn, request),
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            resolve_opencode(conn, request)
        }
        CaptureProvider::DeepAgents => deepagents::resolve_message(conn, request),
        CaptureProvider::Warp => resolve_warp_message(conn, request),
        CaptureProvider::Shelley => resolve_shelley_message(conn, request),
        _ => Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        )),
    }?;
    verify_resolved(request, &resolved)?;
    CompleteMessage::verified(request, resolved.text, SourceVerification::VERIFIED)
}

pub(super) fn resolve_nanoclaw_project(
    requests: &[CompleteMessageRequest],
) -> Result<Vec<CompleteMessage>, CompleteContentError> {
    let first = &requests[0];
    let locators = requests
        .iter()
        .map(|request| {
            let locator = request
                .source_locator
                .as_ref()
                .ok_or_else(|| error(request, CompleteContentErrorKind::HydrationUnsupported))?;
            NativeLocator::new(locator.kind(), locator.value().to_vec())
                .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let query_budget = CompleteContentSqliteQueryBudget::new();
    let project =
        first
            .source_access
            .open_nanoclaw_project(&locators, query_budget, first.event_id)?;
    let mut messages = Vec::with_capacity(requests.len());
    for (request, locator) in requests.iter().zip(&locators) {
        let record = project
            .resolve(locator)
            .map_err(|cause| map_bounded_sqlite_error(request, cause))?
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
        if request.provider_session_id.as_deref() != Some(&record.provider_session_id) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let resolved = resolved_from_values(record.event, record.text, &record.values);
        verify_resolved(request, &resolved)?;
        messages.push(CompleteMessage::verified(
            request,
            resolved.text,
            SourceVerification::VERIFIED,
        )?);
    }
    if !project
        .revalidate()
        .map_err(|cause| map_bounded_sqlite_error(first, cause))?
    {
        return Err(error(first, CompleteContentErrorKind::SourceChanged));
    }
    Ok(messages)
}

pub(super) fn resolve_opencode(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (shape, rowid) = decode_opencode_locator(request)?;
    let dialect = opencode_dialect(request.provider);
    let values =
        opencode::load_opencode_message_values(conn, dialect, shape, rowid).map_err(|cause| {
            if matches!(
                cause,
                CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
            ) {
                error(request, CompleteContentErrorKind::SourceRecordMissing)
            } else {
                map_capture_error(request, cause)
            }
        })?;
    let (session_id, event_hash, text) = opencode::opencode_complete_message(&values, dialect)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_provider_hash_and_digest(
        event_hash,
        text,
        sqlite_logical_record_digest(&values[1..]),
    ))
}

pub(super) fn opencode_dialect(
    provider: CaptureProvider,
) -> &'static opencode::OpenCodeSqliteDialect {
    match provider {
        CaptureProvider::OpenCode => &opencode::OPENCODE_SQLITE_DIALECT,
        CaptureProvider::Kilo => &opencode::KILO_SQLITE_DIALECT,
        CaptureProvider::MiMoCode => &opencode::MIMOCODE_SQLITE_DIALECT,
        _ => &opencode::OPENCODE_SQLITE_DIALECT,
    }
}

pub(super) fn resolve_hermes(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_phased_raw_rowid(request, HERMES_LOCATOR_KIND)?;
    let values = hermes::load_hermes_message_values(conn, rowid).map_err(|cause| {
        if matches!(
            cause,
            CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        ) {
            error(request, CompleteContentErrorKind::SourceRecordMissing)
        } else {
            map_capture_error(request, cause)
        }
    })?;
    let (session_id, event_hash, text) = hermes::hermes_complete_message(conn, &values)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_provider_hash(event_hash, text, &values))
}

pub(super) fn resolve_goose(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_phased_ordered_rowid(request, GOOSE_LOCATOR_KIND)?;
    let values = goose::load_goose_message_values(conn, rowid).map_err(|cause| {
        if matches!(
            cause,
            CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        ) {
            error(request, CompleteContentErrorKind::SourceRecordMissing)
        } else {
            map_capture_error(request, cause)
        }
    })?;
    let (session_id, event_hash, text) = goose::goose_complete_message(&values)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_provider_hash(event_hash, text, &values))
}

pub(super) fn validate_crush_schema(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<(), CompleteContentError> {
    crush::load_crush_message_values_schema(conn).map_err(|cause| map_capture_error(request, cause))
}

pub(super) fn resolve_crush(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_phased_ordered_rowid(request, CRUSH_LOCATOR_KIND)?;
    let values = crush::load_crush_message_values(conn, rowid).map_err(|cause| {
        if matches!(
            cause,
            CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        ) {
            error(request, CompleteContentErrorKind::SourceRecordMissing)
        } else {
            map_capture_error(request, cause)
        }
    })?;
    let (session_id, event_hash, text) = crush::crush_complete_message(&values)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_provider_hash(event_hash, text, &values))
}

pub(super) fn resolve_forgecode(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_raw_rowid(request, FORGECODE_LOCATOR_KIND)?;
    let exists = conn
        .query_row(
            "select exists(select 1 from conversations where rowid = ?1)",
            [rowid],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|cause| map_sqlite_error(request, cause))?;
    if exists == 0 {
        return Err(error(
            request,
            CompleteContentErrorKind::SourceRecordMissing,
        ));
    }
    let values = forgecode::load_forgecode_conversation_values(conn, rowid)
        .map_err(|cause| map_capture_error(request, cause))?;
    let (session_id, event_hash, text) =
        forgecode::forgecode_complete_message(&values, request.source_record_subrecord_index)
            .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_provider_hash(event_hash, text, &values))
}

pub(super) fn resolve_warp_message(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (rowid, message_index) = decode_warp_message_coordinate(request)?;
    let values = conn
        .query_row(
            "select rowid, cast(conversation_id as text), cast(task_id as text), task, \
                    cast(last_modified_at as text) \
             from agent_tasks where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    CapturedSqliteValue::Integer(row.get(0)?),
                    CapturedSqliteValue::Text(row.get(1)?),
                    CapturedSqliteValue::Text(row.get(2)?),
                    CapturedSqliteValue::Blob(row.get(3)?),
                    CapturedSqliteValue::Text(row.get(4)?),
                ])
            },
        )
        .optional()
        .map_err(|cause| map_sqlite_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let [_, CapturedSqliteValue::Text(conversation_id), CapturedSqliteValue::Text(task_id), CapturedSqliteValue::Blob(task), _] =
        values.as_slice()
    else {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    };
    if request.provider_session_id.as_deref() != Some(conversation_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let content = warp::warp_task_content_at(task, task_id, message_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if content.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(resolved_from_provider_hash(
        content.native_record_id,
        content.text,
        &values,
    ))
}

pub(super) struct ShelleyLoadedRecord {
    message: shelley::ShelleyMessageRow,
    conversation: shelley::ShelleyConversationRow,
    digest_values: Vec<CapturedSqliteValue>,
    complete_text: String,
    event: ProviderEventEnvelope,
}

pub(super) fn load_shelley_record(
    conn: &Connection,
    parent_bearing: bool,
    message_rowid: i64,
    conversation_rowid: i64,
) -> crate::Result<Option<ShelleyLoadedRecord>> {
    let message_columns = shelley::shelley_message_columns(conn)?;
    let conversation_columns = shelley::shelley_conversation_columns(conn)?;
    let message_select =
        shelley::shelley_message_select_expressions(&message_columns, "m").join(", ");
    let conversation_select =
        shelley::shelley_conversation_select_expressions(&conversation_columns, "c").join(", ");
    let message_sql = format!("select {message_select} from messages m where m.rowid = ?1");
    let conversation_sql =
        format!("select {conversation_select} from conversations c where c.rowid = ?1");
    let Some(message_values) = conn
        .query_row(
            &message_sql,
            [message_rowid],
            shelley::shelley_message_values,
        )
        .optional()?
    else {
        return Ok(None);
    };
    let Some(conversation_values) = conn
        .query_row(
            &conversation_sql,
            [conversation_rowid],
            shelley::shelley_conversation_values,
        )
        .optional()?
    else {
        return Ok(None);
    };
    let message = shelley::decode_shelley_message(&message_values)?;
    let conversation = shelley::decode_shelley_conversation(&conversation_values)?;
    if message.conversation_id != conversation.conversation_id
        || message.rowid != message_rowid
        || conversation.rowid != conversation_rowid
    {
        return Err(CaptureError::InvalidPayload(
            "Shelley compound content address no longer identifies the captured relationship"
                .to_owned(),
        ));
    }
    let complete_text = shelley::shelley_message_complete_text(&message)
        .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
    let event =
        shelley::shelley_complete_event(&message, &conversation, DateTime::<Utc>::UNIX_EPOCH);
    let digest_values =
        shelley::shelley_verified_record_values(&message, &conversation, parent_bearing);
    Ok(Some(ShelleyLoadedRecord {
        message,
        conversation,
        digest_values,
        complete_text,
        event,
    }))
}

pub(super) fn resolve_shelley_message(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (parent_bearing, message_rowid, conversation_rowid) = decode_shelley_locator(request)?;
    let record = load_shelley_record(conn, parent_bearing, message_rowid, conversation_rowid)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if request.provider_session_id.as_deref() != Some(record.conversation.conversation_id.as_str())
        || record.event.event_type != EventType::Message
    {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(resolved_from_values(
        record.event,
        record.complete_text,
        &record.digest_values,
    ))
}

pub(super) fn shelley_result_record(
    conn: &Connection,
    parent_bearing: bool,
    message_rowid: i64,
    conversation_rowid: i64,
) -> crate::Result<Option<SqliteResultRecord>> {
    let Some(record) =
        load_shelley_record(conn, parent_bearing, message_rowid, conversation_rowid)?
    else {
        return Ok(None);
    };
    if !matches!(
        record.event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return Err(CaptureError::InvalidPayload(
            "Shelley compound content address no longer identifies a result".to_owned(),
        ));
    }
    Ok(Some(SqliteResultRecord {
        values: record.digest_values,
        native_record_id: record.message.message_id,
        content: record.complete_text,
    }))
}

pub(super) struct ResolvedSqliteMessage {
    pub(super) text: String,
    pub(super) provider_event_hash: Option<String>,
    pub(super) normalized_payload_hash: Option<String>,
    pub(super) native_record_id: String,
    pub(super) record_digest: CompleteContentBodyDigest,
}

pub(super) fn resolve_firebender(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let rowid = decode_raw_rowid(request, FIREBENDER_LOCATOR_KIND)?;
    let has_deleted_at = sqlite_table_columns(conn, "chat_sessions")
        .map_err(|cause| map_capture_error(request, cause))?
        .contains("deleted_at");
    let deleted_filter = if has_deleted_at {
        " and deleted_at is null"
    } else {
        ""
    };
    let sql = format!(
        "select id, name, cast(created_at as integer), cast(updated_at as integer), \
                messages_json, metadata_json from chat_sessions where rowid = ?1{deleted_filter}"
    );
    let values = conn
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
        .optional()
        .map_err(|cause| map_sqlite_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let [CapturedSqliteValue::Text(session_id), _, CapturedSqliteValue::Integer(created_at), _, CapturedSqliteValue::Text(messages_json), _] =
        values.as_slice()
    else {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    };
    if request.provider_session_id.as_deref() != Some(session_id) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let messages = serde_json::from_str::<Value>(messages_json)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .ok_or_else(|| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let index = request.source_record_subrecord_index as usize;
    let message = messages
        .get(index)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let fallback =
        DateTime::<Utc>::from_timestamp_millis(*created_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let occurred_at = firebender::firebender_message_time(message, fallback);
    let provider_event_index = u64::try_from(index)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    let event =
        firebender::firebender_event(session_id, provider_event_index, message, occurred_at);
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    let text = firebender::firebender_message_text(message).unwrap_or_else(|| {
        format!(
            "Firebender {}",
            message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("message")
        )
    });
    Ok(resolved_from_values(event, text, &values))
}

pub(super) fn resolve_kiro(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let (table, rowid) = decode_kiro_rowid(request)?;
    let values = if table == "conversations_v2" {
        conn.query_row(
            "select rowid, cast(key as text), cast(conversation_id as text), cast(value as text), \
                    cast(created_at as integer), cast(updated_at as integer) \
             from conversations_v2 where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    CapturedSqliteValue::Integer(row.get(0)?),
                    CapturedSqliteValue::Text(row.get(1)?),
                    CapturedSqliteValue::Text(row.get(2)?),
                    CapturedSqliteValue::Text(row.get(3)?),
                    optional_integer(row.get(4)?),
                    optional_integer(row.get(5)?),
                ])
            },
        )
    } else {
        conn.query_row(
            "select rowid, cast(key as text), cast(value as text) \
             from conversations where rowid = ?1",
            [rowid],
            |row| {
                Ok(vec![
                    CapturedSqliteValue::Integer(row.get(0)?),
                    CapturedSqliteValue::Text(row.get(1)?),
                    CapturedSqliteValue::Text(row.get(2)?),
                ])
            },
        )
    }
    .optional()
    .map_err(|cause| map_sqlite_error(request, cause))?
    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let row = kiro::decode_kiro_conversation_for_complete(table, &values)
        .map_err(|cause| map_capture_error(request, cause))?;
    let value: Value = serde_json::from_str(&row.value)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentVerificationFailed))?;
    let provider_session_id = kiro::kiro_provider_session_id(&row, &value);
    if request.provider_session_id.as_deref() != Some(provider_session_id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let started_at = kiro::kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
    let target_index = usize::try_from(request.source_record_subrecord_index)
        .map_err(|_| error(request, CompleteContentErrorKind::ContentTooLarge))?;
    let decoded = kiro::kiro_history_events(&row, &provider_session_id, &value, started_at)
        .nth(target_index)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let text = decoded.complete_text();
    let event = decoded.event;
    if event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(resolved_from_values(event, text, &values))
}

pub(super) fn resolve_zed(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<ResolvedSqliteMessage, CompleteContentError> {
    let values = zed_values(conn, request)?;
    let row = zed::decode_zed_thread_for_complete(&values)
        .map_err(|cause| map_capture_error(request, cause))?;
    if request.provider_session_id.as_deref() != Some(row.id.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let decoded =
        zed::decode_zed_thread_events(&row).map_err(|cause| map_capture_error(request, cause))?;
    let event_index = usize::try_from(request.source_record_subrecord_index)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    let decoded_event = decoded
        .event_at(&row.id, event_index)
        .map_err(|cause| map_capture_error(request, cause))?
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))?;
    if decoded_event.event.event_type != EventType::Message {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(resolved_from_values(
        decoded_event.event,
        decoded_event.complete_text,
        &values,
    ))
}

pub(super) fn zed_values(
    conn: &Connection,
    request: &CompleteMessageRequest,
) -> Result<Vec<CapturedSqliteValue>, CompleteContentError> {
    let rowid = decode_raw_rowid(request, ZED_LOCATOR_KIND)?;
    let columns =
        sqlite_table_columns(conn, "threads").map_err(|cause| map_capture_error(request, cause))?;
    let parent_id = optional_column(&columns, "parent_id");
    let folder_paths = optional_column(&columns, "folder_paths");
    let folder_paths_order = optional_column(&columns, "folder_paths_order");
    let created_at = optional_column(&columns, "created_at");
    let sql = format!(
        "select rowid, cast(id as text), cast({parent_id} as text), \
                cast({folder_paths} as text), cast({folder_paths_order} as text), \
                cast(summary as text), cast(updated_at as text), cast(data_type as text), data, \
                cast({created_at} as text) from threads where rowid = ?1"
    );
    conn.query_row(&sql, [rowid], |row| {
        Ok(vec![
            CapturedSqliteValue::Integer(row.get(0)?),
            CapturedSqliteValue::Text(row.get(1)?),
            optional_text(row.get(2)?),
            optional_text(row.get(3)?),
            optional_text(row.get(4)?),
            CapturedSqliteValue::Text(row.get(5)?),
            CapturedSqliteValue::Text(row.get(6)?),
            CapturedSqliteValue::Text(row.get(7)?),
            CapturedSqliteValue::Blob(row.get(8)?),
            optional_text(row.get(9)?),
        ])
    })
    .optional()
    .map_err(|cause| map_sqlite_error(request, cause))?
    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceRecordMissing))
}

pub(super) fn verify_resolved(
    request: &CompleteMessageRequest,
    resolved: &ResolvedSqliteMessage,
) -> Result<(), CompleteContentError> {
    if request.expected_native_record_id.as_deref() != Some(&resolved.native_record_id)
        || request.expected_record_digest.as_ref() != Some(&resolved.record_digest)
    {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let actual_event_hash = match request.expected_hash_authority {
        CompleteContentHashAuthority::ProviderSupplied => resolved.provider_event_hash.clone(),
        CompleteContentHashAuthority::NormalizedPayloadFallback => {
            resolved.normalized_payload_hash.clone()
        }
    };
    if actual_event_hash.as_deref() != Some(request.expected_provider_event_hash.as_str()) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    Ok(())
}

pub(super) fn resolved_from_values(
    event: ProviderEventEnvelope,
    text: String,
    values: &[CapturedSqliteValue],
) -> ResolvedSqliteMessage {
    let native_record_id = native_record_id(&event);
    ResolvedSqliteMessage {
        text,
        provider_event_hash: event.provider_event_hash.clone(),
        normalized_payload_hash: compute_payload_hash(&event.payload).ok(),
        native_record_id,
        record_digest: sqlite_logical_record_digest(values),
    }
}

pub(super) fn resolved_from_provider_hash(
    provider_event_hash: String,
    text: String,
    values: &[CapturedSqliteValue],
) -> ResolvedSqliteMessage {
    ResolvedSqliteMessage {
        text,
        native_record_id: provider_event_hash.clone(),
        provider_event_hash: Some(provider_event_hash),
        normalized_payload_hash: None,
        record_digest: sqlite_logical_record_digest(values),
    }
}

pub(super) fn resolved_from_provider_hash_and_digest(
    provider_event_hash: String,
    text: String,
    record_digest: CompleteContentBodyDigest,
) -> ResolvedSqliteMessage {
    ResolvedSqliteMessage {
        text,
        native_record_id: provider_event_hash.clone(),
        provider_event_hash: Some(provider_event_hash),
        normalized_payload_hash: None,
        record_digest,
    }
}

pub(super) fn native_record_id(event: &ProviderEventEnvelope) -> String {
    event
        .provider_event_hash
        .clone()
        .or_else(|| event.cursor.clone())
        .unwrap_or_else(|| format!("event-index:{}", event.provider_event_index))
}
