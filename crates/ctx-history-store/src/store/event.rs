#[allow(unused_imports)]
use super::*;

pub(crate) fn event_payload_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_value_preview(body) {
            return Some(preview);
        }
    }
    event_value_preview(payload)
}

pub(crate) fn event_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object.get(key).and_then(event_preview_fragment) {
            return Some(value);
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(event_preview_fragment)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" | "))
    }
}

pub(crate) fn event_preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn reject_provider_event_hash_conflict(
    conn: &Connection,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = conn.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_tx(
    tx: &Transaction<'_>,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = tx.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_from_rows(
    dedupe_key: &str,
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>>,
) -> Result<()> {
    let Some(incoming) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    for row in rows {
        let existing_key = row?;
        let Some(existing) = parse_provider_event_dedupe_key(&existing_key) else {
            continue;
        };
        if existing.has_same_event_identity(&incoming)
            && existing.payload_hash != incoming.payload_hash
        {
            return Err(StoreError::ProviderEventConflict {
                provider: incoming.provider,
                external_session_id: incoming.external_session_id,
                provider_index: incoming.provider_index,
                existing_hash: existing.payload_hash,
                new_hash: incoming.payload_hash,
            });
        }
    }
    Ok(())
}

pub(crate) fn provider_event_dedupe_key_upper_bound(prefix: &str) -> String {
    let mut upper_bound = prefix.to_owned();
    upper_bound.push(char::MAX);
    upper_bound
}

pub(crate) fn upsert_event_tx(tx: &Transaction<'_>, event: &Event) -> Result<Uuid> {
    let event_id = if let Some(dedupe_key) = &event.dedupe_key {
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .optional()?
        {
            existing
        } else {
            event.id
        }
    } else {
        event.id
    };

    tx.execute(
        r#"
        INSERT INTO events
        (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            seq = excluded.seq,
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            run_id = excluded.run_id,
            event_type = excluded.event_type,
            role = excluded.role,
            occurred_at_ms = excluded.occurred_at_ms,
            capture_source_id = excluded.capture_source_id,
            payload_json = excluded.payload_json,
            payload_blob_id = excluded.payload_blob_id,
            dedupe_key = excluded.dedupe_key,
            visibility = excluded.visibility,
            redaction_state = excluded.redaction_state,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            event_id.to_string(),
            event.seq as i64,
            optional_uuid_string(event.history_record_id),
            optional_uuid_string(event.session_id),
            optional_uuid_string(event.run_id),
            event.event_type.as_str(),
            event.role.map(|role| role.as_str()),
            timestamp_ms(event.occurred_at),
            optional_uuid_string(event.capture_source_id),
            serde_json::to_string(&event.payload)?,
            optional_uuid_string(event.payload_blob_id),
            event.dedupe_key.as_deref(),
            event.sync.visibility.as_str(),
            event.redaction_state.as_str(),
            event.sync.fidelity.as_str(),
            event.sync.sync_state.as_str(),
            event.sync.sync_version as i64,
            optional_timestamp_ms(event.sync.deleted_at),
            serde_json::to_string(&event.sync.metadata)?,
        ],
    )?;
    Ok(event_id)
}

pub(crate) fn upsert_file_touched_tx(tx: &Transaction<'_>, file: &FileTouched) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO files_touched
        (id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            run_id = excluded.run_id,
            event_id = excluded.event_id,
            vcs_workspace_id = excluded.vcs_workspace_id,
            path = excluded.path,
            change_kind = excluded.change_kind,
            old_path = excluded.old_path,
            line_count_delta = excluded.line_count_delta,
            confidence = excluded.confidence,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            file.id.to_string(),
            optional_uuid_string(file.history_record_id),
            optional_uuid_string(file.run_id),
            optional_uuid_string(file.event_id),
            optional_uuid_string(file.vcs_workspace_id),
            file.path.as_str(),
            file.change_kind.map(|kind| kind.as_str()),
            file.old_path.as_deref(),
            file.line_count_delta,
            file.confidence.as_str(),
            timestamp_ms(file.timestamps.created_at),
            timestamp_ms(file.timestamps.updated_at),
            optional_uuid_string(file.source_id),
            file.sync.visibility.as_str(),
            file.sync.fidelity.as_str(),
            file.sync.sync_state.as_str(),
            file.sync.sync_version as i64,
            optional_timestamp_ms(file.sync.deleted_at),
            serde_json::to_string(&file.sync.metadata)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn file_touched_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTouched> {
    Ok(FileTouched {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        run_id: parse_optional_uuid(row.get(2)?)?,
        event_id: parse_optional_uuid(row.get(3)?)?,
        vcs_workspace_id: parse_optional_uuid(row.get(4)?)?,
        path: row.get(5)?,
        change_kind: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<ctx_history_core::FileChangeKind>)
            .transpose()?,
        old_path: row.get(7)?,
        line_count_delta: row.get(8)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(9)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(10)?)?,
            updated_at: ms_to_time(row.get(11)?)?,
        },
        source_id: parse_optional_uuid(row.get(12)?)?,
        sync: sync_metadata_from_row(row, 13, 14, 15, 16, 17, 18)?,
    })
}
