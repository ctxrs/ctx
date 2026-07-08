use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, Event, EventRole, EventType, HistoryRecord};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_uuid_string,
    parse_optional_text_enum, parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::records::{record_from_row, record_select_sql};
use crate::schema::ddl::table_exists;
use crate::search::analyzer::{scriptgram_index_text, scriptgram_match_query};
use crate::{Result, Store};

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

impl Store {
    pub fn refresh_search_index(&self) -> Result<()> {
        self.rebuild_search_projection()
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        for table in [
            "ctx_history_search",
            "event_search",
            "artifact_search",
            "ctx_history_search_scriptgram",
            "event_search_scriptgram",
        ] {
            if table_exists(&self.conn, table)? {
                self.conn.execute(
                    format!("INSERT INTO {table}({table}) VALUES ('optimize')").as_str(),
                    [],
                )?;
            }
        }
        Ok(())
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(false);
        }
        Ok(table_row_count(&self.conn, "events")? > 0
            && table_row_count(&self.conn, "event_search")? == 0)
    }

    pub fn search_event_hits(&self, query: &str, limit: usize) -> Result<Vec<EventSearchHit>> {
        self.search_event_hits_page(query, limit, 0)
    }

    pub fn search_event_hits_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EventSearchHit>> {
        if !table_exists(&self.conn, "event_search")? {
            return Ok(Vec::new());
        }
        let match_query = fts_match_query(query);
        let scriptgram_query = if event_scriptgram_table_ready(&self.conn)? {
            scriptgram_match_query(query)
        } else {
            None
        };
        match (match_query, scriptgram_query) {
            (Some(match_query), Some(scriptgram_query)) => {
                let sql = format!(
                    r#"
                    WITH matches(event_id, score) AS (
                        SELECT event_search.event_id, bm25(event_search)
                        FROM event_search
                        WHERE event_search MATCH ?1
                        UNION ALL
                        SELECT event_search_scriptgram.event_id, bm25(event_search_scriptgram) + 0.35
                        FROM event_search_scriptgram
                        WHERE event_search_scriptgram MATCH ?2
                    ),
                    ranked(event_id, score) AS (
                        SELECT event_id, MIN(score)
                        FROM matches
                        GROUP BY event_id
                    )
                    {}
                    LIMIT ?3 OFFSET ?4
                    "#,
                    event_search_hit_sql(
                        "ranked JOIN event_search ON event_search.event_id = ranked.event_id",
                        "ranked.score",
                        "ORDER BY ranked.score, e.occurred_at_ms DESC, e.seq DESC, event_search.event_id",
                    )
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![
                        match_query,
                        scriptgram_query,
                        limit.max(1) as i64,
                        offset as i64
                    ],
                    event_search_hit_from_row,
                )?;
                collect_rows(rows)
            }
            (Some(match_query), None) => {
                let sql = format!(
                    "{} LIMIT ?2 OFFSET ?3",
                    event_search_hit_sql(
                        "event_search",
                        "bm25(event_search)",
                        "WHERE event_search MATCH ?1 ORDER BY search_score, e.occurred_at_ms DESC, e.seq DESC, event_search.event_id",
                    )
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![match_query, limit.max(1) as i64, offset as i64],
                    event_search_hit_from_row,
                )?;
                collect_rows(rows)
            }
            (None, Some(scriptgram_query)) => {
                let sql = format!(
                    "{} LIMIT ?2 OFFSET ?3",
                    event_search_hit_sql(
                        "event_search_scriptgram JOIN event_search ON event_search.event_id = event_search_scriptgram.event_id",
                        "bm25(event_search_scriptgram) + 0.35",
                        "WHERE event_search_scriptgram MATCH ?1 ORDER BY search_score, e.occurred_at_ms DESC, e.seq DESC, event_search.event_id",
                    )
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![scriptgram_query, limit.max(1) as i64, offset as i64],
                    event_search_hit_from_row,
                )?;
                collect_rows(rows)
            }
            (None, None) => Ok(Vec::new()),
        }
    }

    pub(crate) fn rebuild_search_projection(&self) -> Result<()> {
        rebuild_search_projection(&self.conn)
    }

    pub(crate) fn ensure_search_projection_initialized(&self) -> Result<()> {
        ensure_search_projection_initialized(&self.conn)
    }

    pub(crate) fn normalize_legacy_blob_paths(&self) -> Result<()> {
        self.conn.execute(
                "UPDATE artifacts SET blob_path = 'objects/' || substr(blob_path, 7) WHERE blob_path LIKE 'blobs/%'",
                [],
            )?;
        Ok(())
    }
}

fn event_search_hit_sql(from_sql: &str, score_sql: &str, tail_sql: &str) -> String {
    format!(
        r#"
        SELECT event_search.event_id,
               COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id),
               COALESCE(e.session_id, event_search.session_id, s.id, rs.id),
               e.run_id,
               e.seq,
               e.event_type,
               e.role,
               e.occurred_at_ms,
               event_search.preview_text,
               {score_sql} AS search_score,
               COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider),
               COALESCE(s.external_session_id, rs.external_session_id),
               COALESCE(s.parent_session_id, rs.parent_session_id),
               COALESCE(s.root_session_id, rs.root_session_id),
               COALESCE(s.agent_type, rs.agent_type),
               COALESCE(s.is_primary, rs.is_primary),
               COALESCE(event_source.cwd, session_source.cwd, run_source.cwd),
               COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path),
               e.payload_json,
               COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json),
               wr.title,
               wr.kind,
               wr.workspace
        FROM {from_sql}
        JOIN events e ON e.id = event_search.event_id
        LEFT JOIN runs r ON r.id = e.run_id
        LEFT JOIN sessions s ON s.id = COALESCE(e.session_id, event_search.session_id)
        LEFT JOIN sessions rs ON rs.id = r.session_id
        LEFT JOIN capture_sources event_source ON event_source.id = e.capture_source_id
        LEFT JOIN capture_sources session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
        LEFT JOIN capture_sources run_source ON run_source.id = r.source_id
        LEFT JOIN history_records wr ON wr.id = COALESCE(e.history_record_id, event_search.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id)
        {tail_sql}
        "#
    )
}

fn event_search_hit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventSearchHit> {
    let payload_json = row.get::<_, String>(18)?;
    let source_metadata_json = row.get::<_, Option<String>>(19)?;
    let source_identity = event_search_source_identity(source_metadata_json.as_deref())?;
    Ok(EventSearchHit {
        event_id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        run_id: parse_optional_uuid(row.get(3)?)?,
        seq: nonnegative_i64_to_u64(row.get(4)?)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
        occurred_at: ms_to_time(row.get(7)?)?,
        preview: row.get(8)?,
        score: row.get(9)?,
        provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
        session_external_session_id: row.get(11)?,
        history_source: source_identity.history_source,
        history_source_plugin: source_identity.history_source_plugin,
        provider_key: source_identity.provider_key,
        source_id: source_identity.source_id,
        source_format: source_identity.source_format,
        session_parent_session_id: parse_optional_uuid(row.get(12)?)?,
        session_root_session_id: parse_optional_uuid(row.get(13)?)?,
        agent_type: parse_optional_text_enum::<AgentType>(row.get(14)?)?,
        session_is_primary: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
        cwd: row.get(16)?,
        raw_source_path: row.get(17)?,
        cursor: event_search_cursor(&payload_json, source_metadata_json.as_deref())?,
        record_title: row.get(20)?,
        record_kind: row.get(21)?,
        record_workspace: row.get(22)?,
    })
}

pub(crate) fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    conn.execute("DELETE FROM ctx_history_search", [])?;
    let has_record_scriptgram = record_scriptgram_table_ready(conn)?;
    if has_record_scriptgram {
        conn.execute("DELETE FROM ctx_history_search_scriptgram", [])?;
    }
    let has_event_search = table_exists(conn, "event_search")?;
    if has_event_search {
        conn.execute("DELETE FROM event_search", [])?;
        if event_scriptgram_table_ready(conn)? {
            conn.execute("DELETE FROM event_search_scriptgram", [])?;
        }
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
    let mut insert_record_scriptgram = if has_record_scriptgram {
        Some(conn.prepare(
            r#"
            INSERT INTO ctx_history_search_scriptgram
            (record_id, token_text)
            VALUES (?1, ?2)
            "#,
        )?)
    } else {
        None
    };
    for record in records {
        insert_record_search.execute(params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ])?;
        if let Some(insert_record_scriptgram) = insert_record_scriptgram.as_mut() {
            let token_text = scriptgram_index_text(&record_search_scriptgram_source(&record));
            if !token_text.is_empty() {
                insert_record_scriptgram.execute(params![record.id.to_string(), token_text])?;
            }
        }
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
    if record_scriptgram_table_ready(conn)? {
        conn.execute(
            "DELETE FROM ctx_history_search_scriptgram WHERE record_id = ?1",
            params![record.id.to_string()],
        )?;
    }
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
    if record_scriptgram_table_ready(conn)? {
        let token_text = scriptgram_index_text(&record_search_scriptgram_source(record));
        if !token_text.is_empty() {
            conn.execute(
                r#"
                INSERT INTO ctx_history_search_scriptgram
                (record_id, token_text)
                VALUES (?1, ?2)
                "#,
                params![record.id.to_string(), token_text],
            )?;
        }
    }
    Ok(())
}

fn record_search_scriptgram_source(record: &HistoryRecord) -> String {
    [
        local_preview(&record.title, 512),
        local_preview(&record.body, 2048),
        local_preview(&record.tags.join(" "), 1024),
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" ")
}

pub(crate) fn record_scriptgram_table_ready(conn: &Connection) -> Result<bool> {
    fts_table_has_columns(
        conn,
        "ctx_history_search_scriptgram",
        &["record_id", "token_text"],
    )
}

pub(crate) fn event_scriptgram_table_ready(conn: &Connection) -> Result<bool> {
    fts_table_has_columns(
        conn,
        "event_search_scriptgram",
        &[
            "event_id",
            "history_record_id",
            "session_id",
            "role",
            "token_text",
            "rank_bucket",
        ],
    )
}

fn fts_table_has_columns(conn: &Connection, table: &str, required: &[&str]) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(required
        .iter()
        .all(|required| columns.iter().any(|column| column == required)))
}

fn ensure_search_projection_initialized(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
    }

    let mut projection_rows = table_row_count(conn, "ctx_history_search")?;
    if table_exists(conn, "event_search")? {
        projection_rows += table_row_count(conn, "event_search")?;
    }
    if event_scriptgram_table_ready(conn)? {
        projection_rows += table_row_count(conn, "event_search_scriptgram")?;
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

fn table_row_count(conn: &Connection, table: &str) -> Result<i64> {
    match table {
        "artifacts"
        | "artifact_search"
        | "events"
        | "event_search"
        | "event_search_scriptgram"
        | "history_records"
        | "ctx_history_search"
        | "ctx_history_search_scriptgram" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn linked_artifact_preview_count(conn: &Connection) -> Result<i64> {
    let _ = conn;
    Ok(0)
}

fn populate_event_search_projection(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               e.session_id,
               e.role,
               e.event_type,
               e.payload_json
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
        ))
    })?;
    let mut insert_event_search = conn.prepare(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )?;
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    let mut insert_event_scriptgram = if has_event_scriptgram {
        Some(conn.prepare(
            r#"
            INSERT INTO event_search_scriptgram
            (event_id, history_record_id, session_id, role, token_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?)
    } else {
        None
    };
    for row in rows {
        let (event_id, history_record_id, session_id, role, event_type, payload_json) = row?;
        let event_type = event_type.parse::<EventType>().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
        })?;
        let role = role
            .as_deref()
            .map(str::parse::<EventRole>)
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
        let preview = event_search_preview(event_type, role, &payload_json)?;
        if preview.trim().is_empty() {
            continue;
        }
        insert_event_search.execute(params![
            event_id,
            history_record_id,
            session_id,
            role.map(|role| role.as_str()),
            preview,
            event_type.as_str()
        ])?;
        if let Some(insert_event_scriptgram) = insert_event_scriptgram.as_mut() {
            let token_text = scriptgram_index_text(&preview);
            if !token_text.is_empty() {
                insert_event_scriptgram.execute(params![
                    event_id,
                    history_record_id,
                    session_id,
                    role.map(|role| role.as_str()),
                    token_text,
                    event_type.as_str()
                ])?;
            }
        }
    }
    Ok(())
}

pub(crate) fn insert_event_search_projection_for_event(
    conn: &Connection,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    insert_event_search_projection_for_event_id_with_sidecar(
        conn,
        event.id,
        event,
        has_event_scriptgram,
    )
}

pub(crate) fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    if !table_exists(conn, "event_search")? {
        return Ok(());
    }
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    conn.execute(
        "DELETE FROM event_search WHERE event_id = ?1",
        params![event_id.to_string()],
    )?;
    if has_event_scriptgram {
        conn.execute(
            "DELETE FROM event_search_scriptgram WHERE event_id = ?1",
            params![event_id.to_string()],
        )?;
    }
    insert_event_search_projection_for_event_id_with_sidecar(
        conn,
        event_id,
        event,
        has_event_scriptgram,
    )
}

fn insert_event_search_projection_for_event_id_with_sidecar(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
    has_event_scriptgram: bool,
) -> Result<()> {
    let preview = event_search_preview_from_payload(event.event_type, event.role, &event.payload);
    if preview.trim().is_empty() {
        return Ok(());
    }
    conn.prepare_cached(
        r#"
        INSERT INTO event_search
        (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
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
    if has_event_scriptgram {
        let token_text = scriptgram_index_text(&preview);
        if !token_text.is_empty() {
            conn.prepare_cached(
                r#"
                INSERT INTO event_search_scriptgram
                (event_id, history_record_id, session_id, role, token_text, rank_bucket)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?
            .execute(params![
                event_id.to_string(),
                optional_uuid_string(event.history_record_id),
                optional_uuid_string(event.session_id),
                event.role.map(|role| role.as_str()),
                token_text,
                event.event_type.as_str(),
            ])?;
        }
    }
    Ok(())
}

fn event_search_preview(
    event_type: EventType,
    role: Option<EventRole>,
    payload_json: &str,
) -> Result<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)?;
    Ok(event_search_preview_from_payload(
        event_type, role, &payload,
    ))
}

fn event_search_preview_from_payload(
    event_type: EventType,
    role: Option<EventRole>,
    payload: &serde_json::Value,
) -> String {
    let preview = match event_type {
        EventType::Message if event_role_is_searchable_conversation(role) => {
            event_payload_text_preview(payload)
        }
        EventType::Summary => event_payload_text_preview(payload),
        EventType::ToolCall | EventType::CommandStarted | EventType::CommandFinished => {
            event_tool_call_preview(payload)
        }
        EventType::ToolOutput | EventType::CommandOutput if event_output_is_failure(payload) => {
            event_failed_output_preview(payload)
        }
        _ => None,
    }
    .unwrap_or_default();
    local_preview(&preview, 2048)
}

fn local_preview(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn event_role_is_searchable_conversation(role: Option<EventRole>) -> bool {
    matches!(
        role,
        Some(EventRole::User | EventRole::Assistant | EventRole::System) | None
    )
}

fn event_payload_text_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_text_value_preview(body) {
            return Some(preview);
        }
    }
    event_text_value_preview(payload)
}

fn event_text_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in ["text", "preview", "summary", "message"] {
        if let Some(value) = object.get(key).and_then(event_preview_fragment) {
            return Some(value);
        }
    }
    None
}

fn event_tool_call_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_tool_call_preview_fields(body) {
            return Some(preview);
        }
    }
    event_tool_call_preview_fields(payload)
}

fn event_tool_call_preview_fields(payload: &serde_json::Value) -> Option<String> {
    let object = payload.as_object()?;
    if let Some(command) = object.get("command").and_then(event_preview_fragment) {
        return Some(command);
    }
    if let Some(text) = object.get("text").and_then(event_preview_fragment) {
        return Some(text);
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

fn event_failed_output_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(output_preview) = payload
        .get("output_preview")
        .and_then(event_preview_fragment)
    {
        return Some(output_preview);
    }
    if let Some(output_preview) = payload
        .get("body")
        .and_then(|body| body.get("output_preview"))
        .and_then(event_preview_fragment)
    {
        return Some(output_preview);
    }
    event_payload_text_preview(payload)
}

fn event_output_is_failure(payload: &serde_json::Value) -> bool {
    event_output_fields_indicate_failure(payload)
        || payload
            .get("body")
            .is_some_and(event_output_fields_indicate_failure)
}

fn event_output_fields_indicate_failure(payload: &serde_json::Value) -> bool {
    payload
        .get("timed_out")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || payload
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|code| code != 0)
        || payload
            .get("output_retention")
            .and_then(serde_json::Value::as_str)
            == Some("failed_preview")
        || payload
            .get("content_retention")
            .and_then(serde_json::Value::as_str)
            == Some("failed_output_preview")
}

fn event_preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub(crate) fn fts_match_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .map(|term| term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-'))
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

fn event_search_cursor(
    payload_json: &str,
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    if let Some(cursor) = payload.get("cursor").and_then(|value| value.as_str()) {
        return Ok(Some(cursor.to_owned()));
    }
    if let Some(cursor) = payload
        .get("body")
        .and_then(|body| body.get("cursor"))
        .and_then(|value| value.as_str())
    {
        return Ok(Some(cursor.to_owned()));
    }

    let Some(source_metadata_json) = source_metadata_json else {
        return Ok(None);
    };
    let metadata: serde_json::Value = serde_json::from_str(source_metadata_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    Ok(metadata
        .get("cursor")
        .and_then(|cursor| cursor.get("after"))
        .and_then(|after| after.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned))
}

#[derive(Default)]
struct EventSearchSourceIdentity {
    history_source: Option<String>,
    history_source_plugin: Option<String>,
    provider_key: Option<String>,
    source_id: Option<String>,
    source_format: Option<String>,
}

fn event_search_source_identity(
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<EventSearchSourceIdentity> {
    let Some(source_metadata_json) = source_metadata_json else {
        return Ok(EventSearchSourceIdentity::default());
    };
    let metadata: serde_json::Value = serde_json::from_str(source_metadata_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let source_metadata = metadata
        .get("source_metadata")
        .and_then(serde_json::Value::as_object);
    let plugin = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_plugin"))
        .or_else(|| metadata.get("ctx_history_plugin"))
        .and_then(serde_json::Value::as_object);
    let custom = source_metadata
        .and_then(|metadata| metadata.get("ctx_history_jsonl_v1"))
        .or_else(|| metadata.get("ctx_history_jsonl_v1"))
        .and_then(serde_json::Value::as_object);
    let plugin_name = plugin
        .and_then(|plugin| plugin.get("plugin_name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let plugin_source_id = plugin
        .and_then(|plugin| plugin.get("plugin_source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let history_source = plugin
        .and_then(|plugin| plugin.get("history_source"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            plugin_name
                .as_deref()
                .zip(plugin_source_id.as_deref())
                .map(|(plugin_name, source_id)| format!("{plugin_name}/{source_id}"))
        });
    let provider_key = custom
        .and_then(|custom| custom.get("provider_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_id = custom
        .and_then(|custom| custom.get("source_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let source_format = custom
        .and_then(|custom| custom.get("source_format"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            source_metadata
                .and_then(|metadata| metadata.get("source_format"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            metadata
                .get("source_format")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned);
    Ok(EventSearchSourceIdentity {
        history_source,
        history_source_plugin: plugin_name,
        provider_key,
        source_id,
        source_format,
    })
}
