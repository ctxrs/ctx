#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchHit {
    pub event_id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub session_parent_session_id: Option<Uuid>,
    pub session_root_session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub seq: u64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    pub preview: String,
    pub score: f64,
    pub provider: Option<CaptureProvider>,
    pub session_external_session_id: Option<String>,
    pub history_source: Option<String>,
    pub history_source_plugin: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub cursor: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
}

pub(crate) fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    conn.execute("DELETE FROM ctx_history_search", [])?;
    let has_event_search = table_exists(conn, "event_search")?;
    if has_event_search {
        conn.execute("DELETE FROM event_search", [])?;
        populate_event_search_projection(conn)?;
    }
    if table_exists(conn, "artifact_search")? {
        conn.execute("DELETE FROM artifact_search", [])?;
    }

    let records = {
        let mut stmt = conn.prepare(record_select_sql("ORDER BY created_at DESC").as_str())?;
        let rows = stmt.query_map([], record_from_row)?;
        collect_rows(rows)?
    };

    let mut insert_record_search = conn.prepare(
        r#"
        INSERT INTO ctx_history_search
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#,
    )?;
    for record in records {
        insert_record_search.execute(params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ])?;
    }

    Ok(())
}

pub(crate) fn upsert_record_search_projection(
    conn: &Connection,
    record: &HistoryRecord,
) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM ctx_history_search WHERE record_id = ?1",
        params![record.id.to_string()],
    )?;
    conn.execute(
        r#"
        INSERT INTO ctx_history_search
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#,
        params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ],
    )?;
    Ok(())
}

pub(crate) fn ensure_search_projection_initialized(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    let mut projection_rows = table_row_count(conn, "ctx_history_search")?;
    if table_exists(conn, "event_search")? {
        projection_rows += table_row_count(conn, "event_search")?;
    }
    if table_exists(conn, "artifact_search")? {
        projection_rows += table_row_count(conn, "artifact_search")?;
    }
    if projection_rows > 0 {
        return Ok(());
    }

    if table_row_count(conn, "history_records")? > 0
        || table_row_count(conn, "events")? > 0
        || linked_artifact_preview_count(conn)? > 0
    {
        rebuild_search_projection(conn)?;
    }

    Ok(())
}

pub(crate) fn populate_event_search_projection(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               e.session_id,
               e.role,
               e.event_type,
               e.payload_json,
               e.redaction_state
        FROM events e
        LEFT JOIN runs r ON r.id = e.run_id
        LEFT JOIN sessions s ON s.id = e.session_id
        LEFT JOIN sessions rs ON rs.id = r.session_id
        ORDER BY e.occurred_at_ms, e.seq, e.id
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut insert_event_search = conn.prepare(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?;
    for row in rows {
        let (
            event_id,
            history_record_id,
            session_id,
            role,
            event_type,
            payload_json,
            redaction_state,
        ) = row?;
        let preview = event_search_preview(&payload_json, &redaction_state)?;
        if preview.trim().is_empty() {
            continue;
        }
        insert_event_search.execute(params![
            event_id,
            history_record_id,
            session_id,
            role,
            preview,
            event_type
        ])?;
    }
    Ok(())
}

pub(crate) fn insert_event_search_projection_for_event(
    conn: &Connection,
    event: &Event,
) -> Result<()> {
    insert_event_search_projection_for_event_id(conn, event.id, event)
}

pub(crate) fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM event_search WHERE event_id = ?1",
        params![event_id.to_string()],
    )?;
    insert_event_search_projection_for_event_id(conn, event_id, event)
}

pub(crate) fn insert_event_search_projection_for_event_id(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    let preview = event_search_preview_from_payload(&event.payload, event.redaction_state);
    if preview.trim().is_empty() {
        return Ok(());
    }
    conn.prepare_cached(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, safe_preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?
    .execute(params![
        event_id.to_string(),
        optional_uuid_string(event.history_record_id),
        optional_uuid_string(event.session_id),
        event.role.map(|role| role.as_str()),
        preview,
        event.event_type.as_str(),
    ])?;
    Ok(())
}

pub(crate) fn event_search_preview(payload_json: &str, redaction_state: &str) -> Result<String> {
    if redaction_state == RedactionState::Raw.as_str() {
        return Ok("raw event payload withheld".to_owned());
    }
    let payload: serde_json::Value = serde_json::from_str(payload_json)?;
    Ok(event_search_preview_from_payload(
        &payload,
        parse_text_enum::<RedactionState>(redaction_state.to_owned())?,
    ))
}

pub(crate) fn event_search_preview_from_payload(
    payload: &serde_json::Value,
    redaction_state: RedactionState,
) -> String {
    if redaction_state == RedactionState::Raw {
        return "raw event payload withheld".to_owned();
    }
    let preview = event_payload_preview(payload)
        .or_else(|| {
            if payload.is_object() || payload.is_array() {
                Some(payload.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    local_preview(&preview, 2048)
}

pub(crate) fn migrate_to_v11(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 11;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

pub(crate) fn migrate_to_v12(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        invalidate_provider_import_indexes(conn)?;
        rebuild_search_projection(conn)?;
        conn.execute_batch("PRAGMA user_version = 12;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}
