use rusqlite::{params, Connection, OptionalExtension};

use crate::records::{record_from_row, record_select_sql};
use crate::schema::fts::create_fts_tables_if_supported;
use crate::search::analyzer::scriptgram_index_text;
use crate::{Result, StoreError};

use super::eligibility::semantic_lookup_event_parts;
use super::encoding::local_preview;
use super::prepared::PreparedEventProjection;
use super::storage::record_search_scriptgram_source;

const STORED_EVENT_PROJECTION_SCAN_QUERY: &str = r#"
    SELECT e.id,
           COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
           e.session_id,
           e.role,
           e.event_type,
           e.payload_json,
           'safe_preview',
           e.visibility,
           e.sync_state,
           e.deleted_at_ms
    FROM events e
    LEFT JOIN runs r ON r.id = e.run_id
    LEFT JOIN sessions s ON s.id = e.session_id
    LEFT JOIN sessions rs ON rs.id = r.session_id
"#;
const V47_PROJECTION_SCRATCH_TABLE: &str = "ctx_v47_projection_equivalence_scratch";
const V47_FTS_TABLES: &[(&str, &[&str])] = &[
    (
        "ctx_history_search",
        &[
            "record_id unindexed",
            "title",
            "summary",
            "primary_user_text",
            "decision_text",
            "context_text",
            "tag_text",
        ],
    ),
    (
        "event_search",
        &[
            "event_id unindexed",
            "history_record_id unindexed",
            "session_id unindexed",
            "role unindexed",
            "preview_text",
            "rank_bucket unindexed",
        ],
    ),
    (
        "artifact_search",
        &[
            "artifact_id unindexed",
            "history_record_id unindexed",
            "preview_text",
        ],
    ),
    (
        "ctx_history_search_scriptgram",
        &["record_id unindexed", "token_text"],
    ),
    (
        "event_search_scriptgram",
        &[
            "event_id unindexed",
            "history_record_id unindexed",
            "session_id unindexed",
            "role unindexed",
            "token_text",
            "rank_bucket unindexed",
        ],
    ),
];
const V47_EVENT_LOOKUP_COLUMNS: &[&str] = &[
    "event_id",
    "history_record_id",
    "session_id",
    "role",
    "preview_text",
    "rank_bucket",
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct V47SearchProjectionTables {
    fts5_available: bool,
}

pub(crate) fn prepare_v47_search_projection_tables(
    conn: &Connection,
) -> Result<V47SearchProjectionTables> {
    create_fts_tables_if_supported(conn)?;

    let mut present = 0;
    for (table, column_definitions) in V47_FTS_TABLES {
        match sqlite_object_definition(conn, table)? {
            None => {}
            Some((object_type, sql)) => {
                present += 1;
                let expected_columns = column_definitions
                    .iter()
                    .map(|definition| {
                        definition
                            .split_whitespace()
                            .next()
                            .expect("v47 FTS column definition")
                    })
                    .collect::<Vec<_>>();
                if object_type != "table"
                    || !fts5_column_definitions_are_exact(sql.as_deref(), column_definitions)
                    || table_columns(conn, table)? != expected_columns
                {
                    return Err(malformed_projection_schema(table));
                }
            }
        }
    }
    if present != 0 && present != V47_FTS_TABLES.len() {
        return Err(StoreError::UnsupportedSchemaIdentity(format!(
            "incomplete v47 FTS5 search projection ({present}/{} tables)",
            V47_FTS_TABLES.len()
        )));
    }

    let Some((object_type, sql)) = sqlite_object_definition(conn, "event_search_lookup")? else {
        return Err(malformed_projection_schema("event_search_lookup"));
    };
    if object_type != "table"
        || is_fts5_create_sql(sql.as_deref())
        || table_columns(conn, "event_search_lookup")? != V47_EVENT_LOOKUP_COLUMNS
    {
        return Err(malformed_projection_schema("event_search_lookup"));
    }

    Ok(V47SearchProjectionTables {
        fts5_available: present == V47_FTS_TABLES.len(),
    })
}

pub(crate) fn search_projection_is_exactly_canonical(
    conn: &Connection,
    tables: V47SearchProjectionTables,
) -> Result<bool> {
    let temp_store: i64 = conn.query_row("PRAGMA temp_store", [], |row| row.get(0))?;
    if temp_store != 1 {
        return Err(StoreError::UnsupportedSchemaIdentity(
            "v47 projection verifier requires disk-backed scratch storage".to_owned(),
        ));
    }
    let mut scratch_created = false;
    let result = (|| -> Result<bool> {
        if temp_object_exists(conn, V47_PROJECTION_SCRATCH_TABLE)? {
            return Err(malformed_projection_schema(V47_PROJECTION_SCRATCH_TABLE));
        }
        conn.execute_batch(
            "CREATE TEMP TABLE ctx_v47_projection_equivalence_scratch (
                 entity_id TEXT PRIMARY KEY NOT NULL,
                 row_bytes BLOB NOT NULL
             ) WITHOUT ROWID;",
        )?;
        scratch_created = true;
        search_projection_is_exactly_canonical_inner(conn, tables)
    })();
    let cleanup = if scratch_created {
        conn.execute_batch("DROP TABLE temp.ctx_v47_projection_equivalence_scratch")
    } else {
        Ok(())
    };
    match result {
        Ok(equivalent) => {
            cleanup?;
            Ok(equivalent)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

fn search_projection_is_exactly_canonical_inner(
    conn: &Connection,
    tables: V47SearchProjectionTables,
) -> Result<bool> {
    if tables.fts5_available {
        if !record_projection_is_exact(conn)? || !record_scriptgram_projection_is_exact(conn)? {
            return Ok(false);
        }
        if conn.query_row("SELECT EXISTS(SELECT 1 FROM artifact_search)", [], |row| {
            row.get(0)
        })? {
            return Ok(false);
        }
        if !event_projection_is_exact(conn, EventProjectionKind::Search)?
            || !event_projection_is_exact(conn, EventProjectionKind::Scriptgram)?
        {
            return Ok(false);
        }
    }
    event_projection_is_exact(conn, EventProjectionKind::Lookup)
}

fn record_projection_is_exact(conn: &Connection) -> Result<bool> {
    if !load_actual_projection(
        conn,
        "SELECT record_id, title, summary, primary_user_text,
                decision_text, context_text, tag_text
         FROM ctx_history_search",
        6,
    )? {
        return Ok(false);
    }
    let mut records = conn.prepare(&record_select_sql(""))?;
    let rows = records.query_map([], record_from_row)?;
    for record in rows {
        let record = record?;
        let title = local_preview(&record.title, 512);
        let body = local_preview(&record.body, 2048);
        let tags = local_preview(&record.tags.join(" "), 1024);
        if !consume_expected_projection(
            conn,
            &record.id.to_string(),
            &[
                Some(&title),
                Some(&body),
                Some(&body),
                Some(""),
                Some(""),
                Some(&tags),
            ],
        )? {
            return Ok(false);
        }
    }
    projection_scratch_is_empty(conn)
}

fn record_scriptgram_projection_is_exact(conn: &Connection) -> Result<bool> {
    if !load_actual_projection(
        conn,
        "SELECT record_id, token_text FROM ctx_history_search_scriptgram",
        1,
    )? {
        return Ok(false);
    }
    let mut records = conn.prepare(&record_select_sql(""))?;
    let rows = records.query_map([], record_from_row)?;
    for record in rows {
        let record = record?;
        let token_text = scriptgram_index_text(&record_search_scriptgram_source(&record));
        if !token_text.is_empty()
            && !consume_expected_projection(conn, &record.id.to_string(), &[Some(&token_text)])?
        {
            return Ok(false);
        }
    }
    projection_scratch_is_empty(conn)
}

#[derive(Clone, Copy)]
enum EventProjectionKind {
    Search,
    Lookup,
    Scriptgram,
}

fn event_projection_is_exact(conn: &Connection, kind: EventProjectionKind) -> Result<bool> {
    let actual_query = match kind {
        EventProjectionKind::Search => {
            "SELECT event_id, history_record_id, session_id, role, preview_text, rank_bucket
             FROM event_search"
        }
        EventProjectionKind::Lookup => {
            "SELECT event_id, history_record_id, session_id, role, preview_text, rank_bucket
             FROM event_search_lookup"
        }
        EventProjectionKind::Scriptgram => {
            "SELECT event_id, history_record_id, session_id, role, token_text, rank_bucket
             FROM event_search_scriptgram"
        }
    };
    if !load_actual_projection(conn, actual_query, 5)? {
        return Ok(false);
    }

    let mut events = conn.prepare(STORED_EVENT_PROJECTION_SCAN_QUERY)?;
    let mut rows = events.query([])?;
    while let Some(row) = rows.next()? {
        let Some(projection) = PreparedEventProjection::from_stored_row(row)? else {
            continue;
        };
        let role = projection.role.map(|role| role.as_str());
        let rank_bucket = projection.event_type.as_str();
        let indexed_text = match kind {
            EventProjectionKind::Search => Some(projection.preview.clone()),
            EventProjectionKind::Lookup
                if semantic_lookup_event_parts(projection.event_type, role) =>
            {
                Some(projection.preview.clone())
            }
            EventProjectionKind::Scriptgram => {
                let token_text = scriptgram_index_text(&projection.preview);
                (!token_text.is_empty()).then_some(token_text)
            }
            EventProjectionKind::Lookup => None,
        };
        let Some(indexed_text) = indexed_text else {
            continue;
        };
        if !consume_expected_projection(
            conn,
            &projection.event_id,
            &[
                projection.history_record_id.as_deref(),
                projection.session_id.as_deref(),
                role,
                Some(&indexed_text),
                Some(rank_bucket),
            ],
        )? {
            return Ok(false);
        }
    }
    projection_scratch_is_empty(conn)
}

fn load_actual_projection(conn: &Connection, query: &str, field_count: usize) -> Result<bool> {
    conn.execute(
        "DELETE FROM temp.ctx_v47_projection_equivalence_scratch",
        [],
    )?;
    let mut select = conn.prepare(query)?;
    let mut rows = select.query([])?;
    let mut insert = conn.prepare(
        "INSERT OR IGNORE INTO temp.ctx_v47_projection_equivalence_scratch
             (entity_id, row_bytes)
         VALUES (?1, ?2)",
    )?;
    while let Some(row) = rows.next()? {
        let entity_id = match row.get::<_, Option<String>>(0) {
            Ok(Some(entity_id)) => entity_id,
            Ok(None) | Err(_) => return Ok(false),
        };
        let mut fields = Vec::with_capacity(field_count);
        for index in 1..=field_count {
            match row.get::<_, Option<String>>(index) {
                Ok(value) => fields.push(value),
                Err(_) => return Ok(false),
            }
        }
        let row_bytes = encode_projection_row(fields.iter().map(Option::as_deref));
        if insert.execute(params![entity_id, row_bytes])? != 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn consume_expected_projection(
    conn: &Connection,
    entity_id: &str,
    fields: &[Option<&str>],
) -> Result<bool> {
    let row_bytes = encode_projection_row(fields.iter().copied());
    Ok(conn.execute(
        "DELETE FROM temp.ctx_v47_projection_equivalence_scratch
         WHERE entity_id = ?1 AND row_bytes = ?2",
        params![entity_id, row_bytes],
    )? == 1)
}

fn encode_projection_row<'a>(fields: impl IntoIterator<Item = Option<&'a str>>) -> Vec<u8> {
    let mut encoded = Vec::new();
    for field in fields {
        match field {
            None => encoded.push(0),
            Some(value) => {
                encoded.push(1);
                encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
                encoded.extend_from_slice(value.as_bytes());
            }
        }
    }
    encoded
}

fn projection_scratch_is_empty(conn: &Connection) -> Result<bool> {
    Ok(!conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM temp.ctx_v47_projection_equivalence_scratch
         )",
        [],
        |row| row.get(0),
    )?)
}

fn sqlite_object_definition(
    conn: &Connection,
    name: &str,
) -> Result<Option<(String, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT type, sql FROM sqlite_schema WHERE name = ?1",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn temp_object_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_schema WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get(1))?;
    let mut columns = Vec::new();
    for column in rows {
        columns.push(column?);
    }
    Ok(columns)
}

fn is_fts5_create_sql(sql: Option<&str>) -> bool {
    fts5_column_definitions(sql).is_some()
}

fn fts5_column_definitions_are_exact(sql: Option<&str>, expected: &[&str]) -> bool {
    fts5_column_definitions(sql).is_some_and(|actual| {
        actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
}

fn fts5_column_definitions(sql: Option<&str>) -> Option<Vec<String>> {
    let sql = sql?;
    let mut normalized = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if !normalized.starts_with("create virtual table ") {
        return None;
    }
    if normalized.ends_with(';') {
        normalized.pop();
    }
    let marker = " using fts5";
    let marker_start = normalized.find(marker)?;
    let body = normalized[(marker_start + marker.len())..]
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    Some(
        body.split(',')
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn malformed_projection_schema(table: &str) -> StoreError {
    StoreError::UnsupportedSchemaIdentity(format!("malformed v47 search projection table {table}"))
}

#[cfg(test)]
pub(crate) fn v47_projection_event_scan_query() -> &'static str {
    STORED_EVENT_PROJECTION_SCAN_QUERY
}
