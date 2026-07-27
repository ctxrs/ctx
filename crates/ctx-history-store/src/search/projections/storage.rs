use ctx_history_core::{utc_now, Event, HistoryRecord};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::native_path_group::BoundEncoding;
use crate::records::{record_from_row, record_select_sql};
use crate::schema::ddl::{table_exists, table_has_column};
use crate::search::analyzer::scriptgram_index_text;
use crate::{Result, Store};

use super::eligibility::{
    semantic_lite_turn_anchor_eligible_predicate, semantic_lookup_event_parts,
};
use super::encoding::local_preview;
use super::prepared::PreparedEventProjection;

const SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY: &str = "semantic_searchable_lite_turn_items_v3";
const STORED_EVENT_PROJECTION_QUERY: &str = r#"
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
    ORDER BY e.occurred_at_ms, e.seq, e.id
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventSearchProjectionCapabilities {
    event_search: bool,
    event_lookup: bool,
    event_scriptgram: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SearchProjectionCounts {
    pub(crate) history_search: usize,
    pub(crate) history_scriptgram: usize,
    pub(crate) event_search: usize,
    pub(crate) event_lookup: usize,
    pub(crate) event_scriptgram: usize,
    pub(crate) artifact_search: usize,
}

impl Store {
    pub(crate) fn initialize_event_search_projection_capabilities(&self) -> Result<()> {
        let capabilities = detect_event_search_projection_capabilities(&self.conn)?;
        self.cache_event_search_projection_capabilities(capabilities);
        Ok(())
    }

    pub(crate) fn cache_event_search_projection_capabilities(
        &self,
        capabilities: EventSearchProjectionCapabilities,
    ) {
        self.event_search_projection_capabilities
            .set(Some(capabilities));
    }

    pub(crate) fn invalidate_event_search_projection_capabilities(&self) {
        self.event_search_projection_capabilities.set(None);
    }

    pub(crate) fn event_search_projection_capabilities(
        &self,
    ) -> Result<EventSearchProjectionCapabilities> {
        if let Some(capabilities) = self.event_search_projection_capabilities.get() {
            return Ok(capabilities);
        }
        let capabilities = detect_event_search_projection_capabilities(&self.conn)?;
        self.cache_event_search_projection_capabilities(capabilities);
        Ok(capabilities)
    }

    pub fn refresh_search_index(&self) -> Result<()> {
        self.rebuild_search_projection()
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        self.merge_all_fts_tables_bounded()
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_event_lookup = event_search_lookup_table_ready(&self.conn)?;
        if !has_event_search && !has_event_lookup {
            return Ok(false);
        }
        let events = validated_table_has_rows(&self.conn, "events")?;
        Ok(events
            && ((has_event_search && !validated_table_has_rows(&self.conn, "event_search")?)
                || (has_event_lookup
                    && !validated_table_has_rows(&self.conn, "event_search_lookup")?
                    && event_search_lookup_has_candidate(&self.conn)?)))
    }

    pub(crate) fn rebuild_search_projection(&self) -> Result<()> {
        self.rebuild_search_projection_with_counts().map(|_| ())
    }

    pub(crate) fn rebuild_search_projection_with_counts(&self) -> Result<SearchProjectionCounts> {
        self.invalidate_event_search_projection_capabilities();
        let counts = rebuild_search_projection_with_counts(&self.conn)?;
        self.initialize_event_search_projection_capabilities()?;
        Ok(counts)
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

pub(crate) fn rebuild_search_projection(conn: &Connection) -> Result<()> {
    rebuild_search_projection_with_counts(conn).map(|_| ())
}

fn rebuild_search_projection_with_counts(conn: &Connection) -> Result<SearchProjectionCounts> {
    let mut counts = SearchProjectionCounts::default();
    super::super::atomic_rebuild::run(conn, || {
        counts = rebuild_search_projection_inner(conn)?;
        Ok(())
    })?;
    Ok(counts)
}

fn rebuild_search_projection_inner(conn: &Connection) -> Result<SearchProjectionCounts> {
    let mut counts = SearchProjectionCounts::default();
    let has_record_search = table_exists(conn, "ctx_history_search")?;
    if has_record_search {
        conn.execute("DELETE FROM ctx_history_search", [])?;
    }
    let has_record_scriptgram = record_scriptgram_table_ready(conn)?;
    if has_record_scriptgram {
        conn.execute("DELETE FROM ctx_history_search_scriptgram", [])?;
    }
    let has_event_search = table_exists(conn, "event_search")?;
    if event_search_lookup_table_malformed(conn)? {
        conn.execute("DROP TABLE event_search_lookup", [])?;
    }
    let has_event_lookup = event_search_lookup_table_ready(conn)?;
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    if has_event_search {
        conn.execute("DELETE FROM event_search", [])?;
    }
    if has_event_scriptgram {
        conn.execute("DELETE FROM event_search_scriptgram", [])?;
    }
    if has_event_lookup {
        conn.execute("DELETE FROM event_search_lookup", [])?;
    }
    if has_event_search || has_event_lookup || has_event_scriptgram {
        let event_counts = populate_event_search_projection(
            conn,
            has_event_search,
            has_event_lookup,
            has_event_scriptgram,
        )?;
        counts.event_search = event_counts.event_search;
        counts.event_lookup = event_counts.event_lookup;
        counts.event_scriptgram = event_counts.event_scriptgram;
    }
    if table_exists(conn, "artifact_search")? {
        conn.execute("DELETE FROM artifact_search", [])?;
    }

    if has_record_search || has_record_scriptgram {
        let mut stmt = conn.prepare(record_select_sql("ORDER BY created_at DESC").as_str())?;
        let mut rows = stmt.query_map([], record_from_row)?;
        let mut insert_record_search = if has_record_search {
            Some(conn.prepare(
                r#"
                INSERT INTO ctx_history_search
                (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
                VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
                "#,
            )?)
        } else {
            None
        };
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
        rows.try_for_each(|record| -> Result<()> {
            let record = record?;
            if let Some(insert_record_search) = insert_record_search.as_mut() {
                insert_record_search.execute(params![
                    record.id.to_string(),
                    local_preview(&record.title, 512),
                    local_preview(&record.body, 2048),
                    local_preview(&record.body, 2048),
                    "",
                    local_preview(&record.tags.join(" "), 1024),
                ])?;
                counts.history_search = counts.history_search.saturating_add(1);
            }
            if let Some(insert_record_scriptgram) = insert_record_scriptgram.as_mut() {
                let token_text = scriptgram_index_text(&record_search_scriptgram_source(&record));
                if !token_text.is_empty() {
                    insert_record_scriptgram.execute(params![record.id.to_string(), token_text])?;
                    counts.history_scriptgram = counts.history_scriptgram.saturating_add(1);
                }
            }
            Ok(())
        })?;
    }

    refresh_semantic_searchable_item_stats(conn)?;
    Ok(counts)
}

pub(crate) fn upsert_record_search_projection(
    conn: &Connection,
    record: &HistoryRecord,
) -> Result<()> {
    delete_record_search_projection(conn, record.id)?;
    if !table_exists(conn, "ctx_history_search")? {
        return Ok(());
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

pub(crate) fn delete_record_search_projection(conn: &Connection, record_id: Uuid) -> Result<()> {
    let record_id = record_id.to_string();
    if table_exists(conn, "ctx_history_search")? {
        conn.execute(
            "DELETE FROM ctx_history_search WHERE record_id = ?1",
            params![&record_id],
        )?;
    }
    if table_exists(conn, "ctx_history_search_scriptgram")? {
        conn.execute(
            "DELETE FROM ctx_history_search_scriptgram WHERE record_id = ?1",
            params![&record_id],
        )?;
    }
    Ok(())
}

pub(super) fn record_search_scriptgram_source(record: &HistoryRecord) -> String {
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

    let mut has_projection_rows = validated_table_has_rows(conn, "ctx_history_search")?;
    if !has_projection_rows && table_exists(conn, "event_search")? {
        has_projection_rows = validated_table_has_rows(conn, "event_search")?;
    }
    if !has_projection_rows && event_scriptgram_table_ready(conn)? {
        has_projection_rows = validated_table_has_rows(conn, "event_search_scriptgram")?;
    }
    let event_lookup_has_rows = if event_search_lookup_table_ready(conn)? {
        validated_table_has_rows(conn, "event_search_lookup")?
    } else {
        false
    };
    has_projection_rows |= event_lookup_has_rows;
    if !has_projection_rows && table_exists(conn, "artifact_search")? {
        has_projection_rows = validated_table_has_rows(conn, "artifact_search")?;
    }
    if has_projection_rows {
        if event_search_lookup_table_ready(conn)?
            && !event_lookup_has_rows
            && event_search_lookup_has_candidate(conn)?
        {
            rebuild_event_search_lookup_projection(conn)?;
            return Ok(());
        }
        if cached_semantic_searchable_item_count(conn)?.is_none() {
            refresh_semantic_searchable_item_stats(conn)?;
        }
        return Ok(());
    }

    if validated_table_has_rows(conn, "history_records")?
        || validated_table_has_rows(conn, "events")?
        || linked_artifact_preview_count(conn)? > 0
    {
        rebuild_search_projection(conn)?;
    }

    Ok(())
}

fn validated_table_has_rows(conn: &Connection, table: &str) -> Result<bool> {
    match table {
        "artifacts"
        | "artifact_search"
        | "events"
        | "event_search"
        | "event_search_scriptgram"
        | "event_search_lookup"
        | "history_records"
        | "ctx_history_search"
        | "ctx_history_search_scriptgram" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

pub(crate) fn event_search_lookup_table_ready(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")?
        && table_has_column(conn, "event_search_lookup", "history_record_id")?
        && table_has_column(conn, "event_search_lookup", "preview_text")?)
}

fn event_search_lookup_table_malformed(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")? && !event_search_lookup_table_ready(conn)?)
}

fn event_search_lookup_has_candidate(conn: &Connection) -> Result<bool> {
    if table_exists(conn, "event_search")? && validated_table_has_rows(conn, "event_search")? {
        return Ok(conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM event_search
                WHERE rank_bucket = 'message'
                  AND role IN ('user', 'assistant')
                LIMIT 1
            )
            "#,
            [],
            |row| row.get(0),
        )?);
    }
    if !table_exists(conn, "events")? {
        return Ok(false);
    }
    Ok(conn.query_row(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM events
            WHERE event_type = 'message'
              AND role IN ('user', 'assistant')
              AND deleted_at_ms IS NULL
              AND visibility != 'withheld'
              AND sync_state != 'withheld'
              AND length(trim(payload_json)) > 2
            LIMIT 1
        )
        "#,
        [],
        |row| row.get(0),
    )?)
}

pub(super) fn semantic_searchable_item_count_exact(conn: &Connection) -> Result<usize> {
    if !event_search_lookup_table_ready(conn)? {
        return Ok(0);
    }
    let sql = format!(
        r#"
        SELECT COUNT(*)
        FROM events AS anchor
        JOIN event_search_lookup AS anchor_search
          ON anchor_search.event_id = anchor.id
         AND length(trim(anchor_search.preview_text)) > 0
        WHERE {}
        "#,
        semantic_lite_turn_anchor_eligible_predicate()
    );
    let count = conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
    Ok(count.max(0) as usize)
}

pub(super) fn cached_semantic_searchable_item_count(conn: &Connection) -> Result<Option<usize>> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(None);
    }
    let count = conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            params![SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(count.map(|value| value.max(0) as usize))
}

fn ensure_search_projection_stats_table(conn: &Connection) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS search_projection_stats (
            key TEXT PRIMARY KEY NOT NULL,
            value INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )
        "#,
        [],
    )?;
    Ok(())
}

pub(crate) fn refresh_semantic_searchable_item_stats(conn: &Connection) -> Result<usize> {
    refresh_semantic_searchable_item_stats_accounted(conn, None)
}

fn refresh_semantic_searchable_item_stats_accounted(
    conn: &Connection,
    accounting: Option<&mut usize>,
) -> Result<usize> {
    ensure_search_projection_stats_table(conn)?;
    let count = semantic_searchable_item_count_exact(conn)?;
    if let Some(accounting) = accounting {
        let mut values = BoundEncoding::mutation();
        values.text(SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY);
        values.integer();
        values.integer();
        *accounting = accounting.saturating_add(values.finish());
    }
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
            count as i64,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(count)
}

pub(crate) fn adjust_semantic_searchable_item_stats(
    conn: &Connection,
    previous_count: usize,
    current_count: usize,
    accounting: Option<&mut usize>,
) -> Result<()> {
    if previous_count == current_count {
        return Ok(());
    }
    if !table_exists(conn, "search_projection_stats")? {
        return refresh_semantic_searchable_item_stats_accounted(conn, accounting).map(|_| ());
    }
    if cached_semantic_searchable_item_count(conn)?.is_none() {
        return refresh_semantic_searchable_item_stats_accounted(conn, accounting).map(|_| ());
    }
    let delta = current_count as i64 - previous_count as i64;
    if let Some(accounting) = accounting {
        let mut values = BoundEncoding::mutation();
        values.text(SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY);
        values.integer();
        values.integer();
        *accounting = accounting.saturating_add(values.finish());
    }
    conn.execute(
        r#"
        UPDATE search_projection_stats
        SET value = MAX(value + ?2, 0),
            updated_at_ms = ?3
        WHERE key = ?1
        "#,
        params![
            SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
            delta,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(())
}

fn linked_artifact_preview_count(conn: &Connection) -> Result<i64> {
    let _ = conn;
    Ok(0)
}

pub(crate) fn rebuild_event_search_lookup_projection(conn: &Connection) -> Result<()> {
    if !event_search_lookup_table_ready(conn)? {
        return Ok(());
    }
    conn.execute("DELETE FROM event_search_lookup", [])?;
    populate_event_search_projection(conn, false, true, false).map(|_| ())
}

fn populate_event_search_projection(
    conn: &Connection,
    include_event_search: bool,
    include_event_lookup: bool,
    include_event_scriptgram: bool,
) -> Result<SearchProjectionCounts> {
    populate_event_search_projection_from_query_with_counts(
        conn,
        STORED_EVENT_PROJECTION_QUERY,
        include_event_search,
        include_event_lookup,
        include_event_scriptgram,
    )
}

pub(crate) fn populate_event_search_projection_from_query(
    conn: &Connection,
    query: &str,
    include_event_search: bool,
    include_event_lookup: bool,
    include_event_scriptgram: bool,
) -> Result<()> {
    populate_event_search_projection_from_query_with_counts(
        conn,
        query,
        include_event_search,
        include_event_lookup,
        include_event_scriptgram,
    )
    .map(|_| ())
}

fn populate_event_search_projection_from_query_with_counts(
    conn: &Connection,
    query: &str,
    include_event_search: bool,
    include_event_lookup: bool,
    include_event_scriptgram: bool,
) -> Result<SearchProjectionCounts> {
    let mut counts = SearchProjectionCounts::default();
    let mut stmt = conn.prepare(query)?;
    let mut rows = stmt.query([])?;
    let mut insert_event_search = if include_event_search {
        Some(conn.prepare(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?)
    } else {
        None
    };
    let mut insert_event_scriptgram = if include_event_scriptgram {
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
    let mut insert_event_lookup = if include_event_lookup {
        Some(conn.prepare(
            r#"
            INSERT INTO event_search_lookup
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?)
    } else {
        None
    };
    while let Some(row) = rows.next()? {
        let Some(projection) = PreparedEventProjection::from_stored_row(row)? else {
            continue;
        };
        let role = projection.role.map(|role| role.as_str());
        let rank_bucket = projection.event_type.as_str();
        if let Some(insert_event_search) = insert_event_search.as_mut() {
            insert_event_search.execute(params![
                &projection.event_id,
                &projection.history_record_id,
                &projection.session_id,
                role,
                &projection.preview,
                rank_bucket
            ])?;
            counts.event_search = counts.event_search.saturating_add(1);
        }
        if let Some(insert_event_scriptgram) = insert_event_scriptgram.as_mut() {
            let token_text = scriptgram_index_text(&projection.preview);
            if !token_text.is_empty() {
                insert_event_scriptgram.execute(params![
                    &projection.event_id,
                    &projection.history_record_id,
                    &projection.session_id,
                    role,
                    token_text,
                    rank_bucket
                ])?;
                counts.event_scriptgram = counts.event_scriptgram.saturating_add(1);
            }
        }
        if semantic_lookup_event_parts(projection.event_type, role) {
            if let Some(insert_event_lookup) = insert_event_lookup.as_mut() {
                insert_event_lookup.execute(params![
                    &projection.event_id,
                    &projection.history_record_id,
                    &projection.session_id,
                    role,
                    &projection.preview,
                    rank_bucket
                ])?;
                counts.event_lookup = counts.event_lookup.saturating_add(1);
            }
        }
    }
    Ok(counts)
}

pub(crate) fn insert_event_search_projection_for_event(
    conn: &Connection,
    event: &Event,
    capabilities: EventSearchProjectionCapabilities,
    accounting: Option<&mut usize>,
) -> Result<()> {
    insert_event_search_projection_for_event_id(conn, event.id, event, capabilities, accounting)
}

pub(crate) fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
    capabilities: EventSearchProjectionCapabilities,
    mut accounting: Option<&mut usize>,
) -> Result<()> {
    let EventSearchProjectionCapabilities {
        event_search: has_event_search,
        event_lookup: has_event_lookup,
        event_scriptgram: has_event_scriptgram,
    } = capabilities;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }
    let event_id_text = event_id.to_string();
    if has_event_search {
        account_single_text_bind(accounting.as_deref_mut(), &event_id_text);
        conn.execute(
            "DELETE FROM event_search WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    if has_event_scriptgram {
        account_single_text_bind(accounting.as_deref_mut(), &event_id_text);
        conn.execute(
            "DELETE FROM event_search_scriptgram WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    if has_event_lookup {
        account_single_text_bind(accounting.as_deref_mut(), &event_id_text);
        conn.execute(
            "DELETE FROM event_search_lookup WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    insert_event_search_projection_for_event_id(conn, event_id, event, capabilities, accounting)
}

pub(crate) fn insert_event_search_projection_for_event_id(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
    capabilities: EventSearchProjectionCapabilities,
    mut accounting: Option<&mut usize>,
) -> Result<()> {
    let EventSearchProjectionCapabilities {
        event_search: has_event_search,
        event_lookup: has_event_lookup,
        event_scriptgram: has_event_scriptgram,
    } = capabilities;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }
    let Some(projection) = PreparedEventProjection::from_event(event_id, event) else {
        return Ok(());
    };
    let role = projection.role.map(|role| role.as_str());
    let rank_bucket = projection.event_type.as_str();
    if has_event_search {
        account_event_projection_binds(
            accounting.as_deref_mut(),
            &projection,
            role,
            &projection.preview,
            rank_bucket,
        );
        conn.prepare_cached(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?
        .execute(params![
            &projection.event_id,
            &projection.history_record_id,
            &projection.session_id,
            role,
            &projection.preview,
            rank_bucket,
        ])?;
    }
    if has_event_scriptgram {
        let token_text = scriptgram_index_text(&projection.preview);
        if !token_text.is_empty() {
            account_event_projection_binds(
                accounting.as_deref_mut(),
                &projection,
                role,
                &token_text,
                rank_bucket,
            );
            conn.prepare_cached(
                r#"
                INSERT INTO event_search_scriptgram
                (event_id, history_record_id, session_id, role, token_text, rank_bucket)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?
            .execute(params![
                &projection.event_id,
                &projection.history_record_id,
                &projection.session_id,
                role,
                token_text,
                rank_bucket,
            ])?;
        }
    }
    if has_event_lookup && semantic_lookup_event_parts(projection.event_type, role) {
        account_event_projection_binds(
            accounting,
            &projection,
            role,
            &projection.preview,
            rank_bucket,
        );
        conn.prepare_cached(
            r#"
            INSERT INTO event_search_lookup
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?
        .execute(params![
            &projection.event_id,
            &projection.history_record_id,
            &projection.session_id,
            role,
            &projection.preview,
            rank_bucket,
        ])?;
    }
    Ok(())
}

fn account_single_text_bind(accounting: Option<&mut usize>, value: &str) {
    let Some(accounting) = accounting else {
        return;
    };
    let mut values = BoundEncoding::mutation();
    values.text(value);
    *accounting = accounting.saturating_add(values.finish());
}

fn account_event_projection_binds(
    accounting: Option<&mut usize>,
    projection: &PreparedEventProjection,
    role: Option<&str>,
    indexed_text: &str,
    rank_bucket: &str,
) {
    let Some(accounting) = accounting else {
        return;
    };
    let mut values = BoundEncoding::mutation();
    values.text(&projection.event_id);
    values.optional_text(projection.history_record_id.as_deref());
    values.optional_text(projection.session_id.as_deref());
    values.optional_text(role);
    values.text(indexed_text);
    values.text(rank_bucket);
    *accounting = accounting.saturating_add(values.finish());
}

pub(crate) fn detect_event_search_projection_capabilities(
    conn: &Connection,
) -> Result<EventSearchProjectionCapabilities> {
    Ok(EventSearchProjectionCapabilities {
        event_search: table_exists(conn, "event_search")?,
        event_lookup: table_exists(conn, "event_search_lookup")?,
        event_scriptgram: event_scriptgram_table_ready(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[test]
    fn provider_projection_readiness_is_bounded_independent_of_event_cardinality() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        store
            .conn
            .execute_batch(
                "WITH RECURSIVE n(i) AS (
                     VALUES(1)
                     UNION ALL
                     SELECT i + 1 FROM n WHERE i < 65536
                 )
                 INSERT INTO events
                     (id, seq, event_type, role, occurred_at_ms, payload_json)
                 SELECT printf('00000000-0000-4000-8000-%012d', i),
                        i, 'message', 'user', i, json_object('text', i)
                 FROM n;

                 INSERT INTO event_search
                     (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                 VALUES
                     ('00000000-0000-4000-8000-000000000001',
                      NULL, NULL, 'user', 'ready', 'message');

                 INSERT INTO event_search_lookup
                     (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                 VALUES
                     ('00000000-0000-4000-8000-000000000001',
                      NULL, NULL, 'user', 'ready', 'message');",
            )
            .unwrap();

        let callbacks = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&callbacks);
        store.conn.progress_handler(
            100,
            Some(move || observed.fetch_add(1, Ordering::Relaxed) >= 100),
        );
        let readiness = store.event_search_projection_needs_backfill();
        store.conn.progress_handler(0, None::<fn() -> bool>);

        assert!(!readiness.unwrap());
        assert!(
            callbacks.load(Ordering::Relaxed) < 100,
            "readiness exceeded its bounded SQLite work allowance"
        );
        for table in ["events", "event_search", "event_search_lookup"] {
            let mut statement = store
                .conn
                .prepare(&format!(
                    "EXPLAIN SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)"
                ))
                .unwrap();
            let opcodes = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                !opcodes.iter().any(|opcode| opcode == "Count"),
                "{table} readiness regressed to exact row counting: {opcodes:?}"
            );
        }
    }
}
