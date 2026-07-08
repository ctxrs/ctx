use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    utc_now, AgentType, CaptureProvider, Event, EventRole, EventType, HistoryRecord,
    RedactionState, SyncState, Visibility,
};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_uuid_string,
    parse_optional_text_enum, parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::records::{record_from_row, record_select_sql};
use crate::schema::ddl::{table_exists, table_has_column};
use crate::search::analyzer::{scriptgram_index_text, scriptgram_match_query};
use crate::{Result, Store};

const SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY: &str = "semantic_searchable_lite_turn_items_v2";
const SEMANTIC_TURN_TEXT_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_LITE_TURN_RANK_BUCKET: &str = "lite_turn";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEmbeddingDocument {
    pub event_id: Uuid,
    pub history_record_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub seq: u64,
    pub occurred_at_ms: i64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub rank_bucket: String,
    pub provider: Option<CaptureProvider>,
    pub source_format: Option<String>,
    pub agent_type: Option<AgentType>,
    pub session_is_primary: Option<bool>,
    pub cwd: Option<String>,
    pub raw_source_path: Option<String>,
    pub record_title: Option<String>,
    pub record_kind: Option<String>,
    pub record_workspace: Option<String>,
    pub text: String,
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
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_event_lookup = event_search_lookup_table_ready(&self.conn)?;
        if !has_event_search && !has_event_lookup {
            return Ok(false);
        }
        let events = table_row_count(&self.conn, "events")?;
        Ok(events > 0
            && ((has_event_search && table_row_count(&self.conn, "event_search")? == 0)
                || (has_event_lookup
                    && table_row_count(&self.conn, "event_search_lookup")? == 0
                    && event_search_lookup_candidate_count(&self.conn)? > 0)))
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

    pub fn semantic_event_hits_by_id(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
    ) -> Result<Vec<EventSearchHit>> {
        if chunk_ranges.is_empty() {
            return Ok(Vec::new());
        }
        let event_ids = chunk_ranges.keys().copied().collect::<Vec<_>>();
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = semantic_lite_turn_document_select_sql(
            &format!(
                r#"
                WHERE anchor.id IN ({placeholders})
                  AND {}
                "#,
                semantic_lite_turn_anchor_eligible_predicate()
            ),
            "",
        );
        let params = event_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let event_id = parse_uuid(row.get::<_, String>(0)?)?;
            let preview_text = row.get::<_, String>(8)?;
            let source_metadata_json = row.get::<_, Option<String>>(15)?;
            let source_identity = event_search_source_identity(source_metadata_json.as_deref())?;
            let redaction_state = row.get::<_, String>(9)?;
            let assistant_preview_text = row.get::<_, Option<String>>(19)?;
            let assistant_redaction_state = row.get::<_, Option<String>>(20)?;
            let preview = chunk_ranges
                .get(&event_id)
                .map(|(start_char, end_char)| {
                    semantic_lite_turn_source_chunk(
                        &preview_text,
                        &redaction_state,
                        assistant_preview_text.as_deref(),
                        assistant_redaction_state.as_deref(),
                        *start_char,
                        *end_char,
                    )
                })
                .transpose()?
                .unwrap_or_default();
            Ok(EventSearchHit {
                event_id,
                history_record_id: parse_optional_uuid(row.get(1)?)?,
                session_id: parse_optional_uuid(row.get(2)?)?,
                run_id: parse_optional_uuid(row.get(21)?)?,
                seq: row.get::<_, i64>(3)? as u64,
                event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
                role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
                occurred_at: ms_to_time(row.get(22)?)?,
                preview,
                score: 0.0,
                provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
                session_external_session_id: row.get(23)?,
                history_source: source_identity.history_source,
                history_source_plugin: source_identity.history_source_plugin,
                provider_key: source_identity.provider_key,
                source_id: source_identity.source_id,
                source_format: source_identity.source_format,
                session_parent_session_id: parse_optional_uuid(row.get(24)?)?,
                session_root_session_id: parse_optional_uuid(row.get(25)?)?,
                agent_type: parse_optional_text_enum::<AgentType>(row.get(11)?)?,
                session_is_primary: row.get::<_, Option<i64>>(12)?.map(|value| value != 0),
                cwd: row.get(13)?,
                raw_source_path: row.get(14)?,
                cursor: event_search_cursor(&preview_text, source_metadata_json.as_deref())?,
                record_title: row.get(16)?,
                record_kind: row.get(17)?,
                record_workspace: row.get(18)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn semantic_eligible_event_ids(&self, event_ids: &[Uuid]) -> Result<HashSet<Uuid>> {
        if event_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"
            SELECT anchor.id
            FROM events AS anchor
            JOIN event_search_lookup AS anchor_search
              ON anchor_search.event_id = anchor.id
             AND length(trim(anchor_search.preview_text)) > 0
            WHERE anchor.id IN ({placeholders})
              AND {}
            "#,
            semantic_lite_turn_anchor_eligible_predicate()
        );
        let params = event_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(params))?;
        let mut eligible = HashSet::new();
        while let Some(row) = rows.next()? {
            eligible.insert(parse_uuid(row.get::<_, String>(0)?)?);
        }
        Ok(eligible)
    }

    pub fn count_event_embedding_documents(&self) -> Result<usize> {
        self.event_embedding_document_count_cached_or_exact()
    }

    pub fn count_event_embedding_documents_exact(&self) -> Result<usize> {
        semantic_searchable_item_count_exact(&self.conn)
    }

    pub fn cached_event_embedding_document_count(&self) -> Result<Option<usize>> {
        cached_semantic_searchable_item_count(&self.conn)
    }

    pub fn event_embedding_document_count_cached_or_exact(&self) -> Result<usize> {
        if let Some(count) = self.cached_event_embedding_document_count()? {
            return Ok(count);
        }
        semantic_searchable_item_count_exact(&self.conn)
    }

    pub fn refresh_event_embedding_document_count_cache(&self) -> Result<()> {
        refresh_semantic_searchable_item_stats(&self.conn).map(|_| ())
    }

    pub fn recent_event_embedding_documents(
        &self,
        before: Option<(i64, u64)>,
        limit: usize,
    ) -> Result<Vec<EventEmbeddingDocument>> {
        let sql = semantic_lite_turn_document_select_sql(
            &format!(
                r#"
                WHERE {}
                  AND (
                        ?1 IS NULL
                        OR anchor.occurred_at_ms < ?1
                        OR (anchor.occurred_at_ms = ?1 AND anchor.seq < ?2)
                  )
                ORDER BY anchor.occurred_at_ms DESC, anchor.seq DESC
                LIMIT ?3
                "#,
                semantic_lite_turn_anchor_eligible_predicate()
            ),
            "ORDER BY document_activity_at_ms DESC, seq DESC",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                before.map(|(occurred_at_ms, _)| occurred_at_ms),
                before.map(|(_, seq)| seq as i64),
                limit.max(1) as i64
            ],
            event_embedding_document_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn event_embedding_documents_matching_terms(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<EventEmbeddingDocument>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let next_user_predicate =
            semantic_lite_turn_user_eligible_predicate("next_user", "next_user_search");
        let clauses = terms
            .iter()
            .map(|_| {
                format!(
                    r#"
                (
                    lower(anchor_search.preview_text) LIKE ? ESCAPE '\'
                    OR EXISTS (
                        SELECT 1
                        FROM events AS candidate
                        JOIN event_search_lookup AS candidate_search
                          ON candidate_search.event_id = candidate.id
                         AND length(trim(candidate_search.preview_text)) > 0
                        WHERE candidate.event_type = 'message'
                          AND candidate.role = 'assistant'
                          AND candidate.deleted_at_ms IS NULL
                          AND candidate.visibility != 'withheld'
                          AND candidate.sync_state != 'withheld'
                          AND length(trim(candidate.payload_json)) > 2
                          AND lower(candidate_search.preview_text) LIKE ? ESCAPE '\'
                          AND (
                                (anchor.run_id IS NOT NULL AND candidate.run_id = anchor.run_id)
                                OR (
                                    anchor.run_id IS NULL
                                    AND anchor.session_id IS NOT NULL
                                    AND candidate.run_id IS NULL
                                    AND candidate.session_id = anchor.session_id
                                )
                          )
                          AND (
                                candidate.occurred_at_ms > anchor.occurred_at_ms
                                OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq > anchor.seq)
                                OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq = anchor.seq AND candidate.id > anchor.id)
                          )
                          AND NOT EXISTS (
                              SELECT 1
                              FROM events AS next_user
                              JOIN event_search_lookup AS next_user_search
                                ON next_user_search.event_id = next_user.id
                               AND length(trim(next_user_search.preview_text)) > 0
                              WHERE {next_user_predicate}
                                AND (
                                      (anchor.run_id IS NOT NULL AND next_user.run_id = anchor.run_id)
                                      OR (
                                          anchor.run_id IS NULL
                                          AND anchor.session_id IS NOT NULL
                                          AND next_user.run_id IS NULL
                                          AND next_user.session_id = anchor.session_id
                                      )
                                )
                                AND (
                                      next_user.occurred_at_ms > anchor.occurred_at_ms
                                      OR (next_user.occurred_at_ms = anchor.occurred_at_ms AND next_user.seq > anchor.seq)
                                      OR (next_user.occurred_at_ms = anchor.occurred_at_ms AND next_user.seq = anchor.seq AND next_user.id > anchor.id)
                                )
                                AND (
                                      next_user.occurred_at_ms < candidate.occurred_at_ms
                                      OR (next_user.occurred_at_ms = candidate.occurred_at_ms AND next_user.seq < candidate.seq)
                                      OR (next_user.occurred_at_ms = candidate.occurred_at_ms AND next_user.seq = candidate.seq AND next_user.id < candidate.id)
                                )
                          )
                    )
                )
                "#
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = semantic_lite_turn_document_select_sql(
            &format!(
                r#"
                WHERE {}
                  AND ({clauses})
                ORDER BY anchor.seq DESC
                LIMIT ?
                "#,
                semantic_lite_turn_anchor_eligible_predicate()
            ),
            "ORDER BY seq DESC",
        );
        let mut params = Vec::new();
        for term in terms {
            let pattern = format!("%{}%", escape_like_term(&term.to_lowercase()));
            params.push(pattern.clone());
            params.push(pattern);
        }
        params.push(limit.max(1).to_string());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), event_embedding_document_from_row)?;
        collect_rows(rows)
    }

    pub fn event_embedding_documents_by_ids(
        &self,
        event_ids: &[Uuid],
    ) -> Result<Vec<EventEmbeddingDocument>> {
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = semantic_lite_turn_document_select_sql(
            &format!(
                r#"
                WHERE anchor.id IN ({placeholders})
                  AND {}
                "#,
                semantic_lite_turn_anchor_eligible_predicate()
            ),
            "",
        );
        let params = event_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), event_embedding_document_from_row)?;
        collect_rows(rows)
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
    if has_event_search || has_event_lookup {
        populate_event_search_projection(
            conn,
            has_event_search,
            has_event_lookup,
            has_event_scriptgram,
        )?;
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

    refresh_semantic_searchable_item_stats(conn)?;
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
    let event_lookup_rows = if event_search_lookup_table_ready(conn)? {
        table_row_count(conn, "event_search_lookup")?
    } else {
        0
    };
    projection_rows += event_lookup_rows;
    if table_exists(conn, "artifact_search")? {
        projection_rows += table_row_count(conn, "artifact_search")?;
    }
    if projection_rows > 0 {
        if event_search_lookup_table_ready(conn)?
            && event_lookup_rows == 0
            && event_search_lookup_candidate_count(conn)? > 0
        {
            rebuild_event_search_lookup_projection(conn)?;
            return Ok(());
        }
        if cached_semantic_searchable_item_count(conn)?.is_none() {
            refresh_semantic_searchable_item_stats(conn)?;
        }
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
        | "event_search_lookup"
        | "history_records"
        | "ctx_history_search"
        | "ctx_history_search_scriptgram" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn event_search_lookup_table_ready(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")?
        && table_has_column(conn, "event_search_lookup", "history_record_id")?
        && table_has_column(conn, "event_search_lookup", "preview_text")?)
}

fn event_search_lookup_table_malformed(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")? && !event_search_lookup_table_ready(conn)?)
}

fn event_search_lookup_candidate_count(conn: &Connection) -> Result<i64> {
    if table_exists(conn, "event_search")? && table_row_count(conn, "event_search")? > 0 {
        return Ok(conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM event_search
            WHERE rank_bucket = 'message'
              AND role IN ('user', 'assistant')
            "#,
            [],
            |row| row.get::<_, i64>(0),
        )?);
    }
    if !table_exists(conn, "events")? {
        return Ok(0);
    }
    Ok(conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM events
        WHERE event_type = 'message'
          AND role IN ('user', 'assistant')
          AND deleted_at_ms IS NULL
          AND visibility != 'withheld'
          AND sync_state != 'withheld'
          AND length(trim(payload_json)) > 2
        "#,
        [],
        |row| row.get::<_, i64>(0),
    )?)
}

fn semantic_searchable_item_count_exact(conn: &Connection) -> Result<usize> {
    if !event_search_lookup_table_ready(conn)? {
        return Ok(0);
    }
    let sql = format!(
        r#"
        {}
        SELECT COUNT(*)
        FROM semantic_lite_turn_docs
        "#,
        semantic_lite_turn_cte_sql(&format!(
            "WHERE {}",
            semantic_lite_turn_anchor_eligible_predicate()
        ))
    );
    let count = conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
    Ok(count.max(0) as usize)
}

fn cached_semantic_searchable_item_count(conn: &Connection) -> Result<Option<usize>> {
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

fn refresh_semantic_searchable_item_stats(conn: &Connection) -> Result<usize> {
    ensure_search_projection_stats_table(conn)?;
    let count = semantic_searchable_item_count_exact(conn)?;
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
) -> Result<()> {
    if previous_count == current_count {
        return Ok(());
    }
    if !table_exists(conn, "search_projection_stats")? {
        return refresh_semantic_searchable_item_stats(conn).map(|_| ());
    }
    if cached_semantic_searchable_item_count(conn)?.is_none() {
        return refresh_semantic_searchable_item_stats(conn).map(|_| ());
    }
    let delta = current_count as i64 - previous_count as i64;
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

fn semantic_lite_turn_document_select_sql(anchor_tail: &str, document_tail: &str) -> String {
    format!(
        r#"
        {}
        SELECT event_id,
               history_record_id,
               session_id,
               seq,
               document_activity_at_ms,
               event_type,
               role,
               rank_bucket,
               user_payload_json,
               redaction_state,
               provider,
               agent_type,
               session_is_primary,
               cwd,
               raw_source_path,
               source_metadata_json,
               record_title,
               record_kind,
               record_workspace,
               assistant_payload_json,
               assistant_redaction_state,
               run_id,
               occurred_at_ms,
               session_external_session_id,
               session_parent_session_id,
               session_root_session_id
        FROM semantic_lite_turn_docs
        {document_tail}
        "#,
        semantic_lite_turn_cte_sql(anchor_tail)
    )
}

fn semantic_lite_turn_anchor_eligible_predicate() -> String {
    semantic_lite_turn_user_eligible_predicate("anchor", "anchor_search")
}

fn semantic_lite_turn_user_eligible_predicate(event_alias: &str, search_alias: &str) -> String {
    format!(
        r#"
    {event_alias}.event_type = 'message'
    AND {event_alias}.role = 'user'
    AND {event_alias}.deleted_at_ms IS NULL
    AND {event_alias}.visibility != 'withheld'
    AND {event_alias}.sync_state != 'withheld'
    AND length(trim({event_alias}.payload_json)) > 2
    AND trim({search_alias}.preview_text) NOT LIKE '<environment_context>%'
    AND trim({search_alias}.preview_text) NOT LIKE '<turn_aborted>%'
    AND trim({search_alias}.preview_text) NOT LIKE '<subagent_notification>%'
    AND trim({search_alias}.preview_text) NOT LIKE 'Warning: The maximum number of unified exec processes%'
    "#
    )
}

fn semantic_lite_turn_preview_is_control(preview: &str) -> bool {
    let trimmed = preview.trim();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<turn_aborted>")
        || trimmed.starts_with("<subagent_notification>")
        || trimmed.starts_with("Warning: The maximum number of unified exec processes")
}

fn semantic_lite_turn_cte_sql(anchor_tail: &str) -> String {
    let candidate_user_predicate =
        semantic_lite_turn_user_eligible_predicate("candidate_user", "candidate_user_search");
    format!(
        r#"
        WITH semantic_anchor_page AS MATERIALIZED (
            SELECT anchor.id AS event_id,
                   anchor.history_record_id AS history_record_id,
                   anchor.session_id AS session_id,
                   anchor.run_id AS run_id,
                   anchor.seq AS seq,
                   anchor.occurred_at_ms AS occurred_at_ms,
                   anchor.event_type AS event_type,
                   anchor.role AS role,
                   anchor_search.preview_text AS preview_text,
                   anchor.capture_source_id AS capture_source_id
            FROM events AS anchor
            JOIN event_search_lookup AS anchor_search
              ON anchor_search.event_id = anchor.id
             AND length(trim(anchor_search.preview_text)) > 0
            {anchor_tail}
        ),
        semantic_lite_turn_docs AS (
            SELECT anchor.event_id AS event_id,
                   COALESCE(anchor.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id) AS history_record_id,
                   COALESCE(anchor.session_id, s.id, rs.id) AS session_id,
                   anchor.run_id AS run_id,
                   anchor.seq AS seq,
                   anchor.occurred_at_ms AS occurred_at_ms,
                   COALESCE(MAX(anchor.occurred_at_ms, assistant.occurred_at_ms), anchor.occurred_at_ms) AS document_activity_at_ms,
                   anchor.event_type AS event_type,
                   anchor.role AS role,
                   '{SEMANTIC_LITE_TURN_RANK_BUCKET}' AS rank_bucket,
                   anchor.preview_text AS user_payload_json,
                   'safe_preview' AS redaction_state,
                   COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider) AS provider,
                   COALESCE(s.external_session_id, rs.external_session_id) AS session_external_session_id,
                   COALESCE(s.parent_session_id, rs.parent_session_id) AS session_parent_session_id,
                   COALESCE(s.root_session_id, rs.root_session_id) AS session_root_session_id,
                   COALESCE(s.agent_type, rs.agent_type) AS agent_type,
                   COALESCE(s.is_primary, rs.is_primary) AS session_is_primary,
                   COALESCE(event_source.cwd, session_source.cwd, run_source.cwd) AS cwd,
                   COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path) AS raw_source_path,
                   COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json) AS source_metadata_json,
                   wr.title AS record_title,
                   wr.kind AS record_kind,
                   wr.workspace AS record_workspace,
                   assistant_search.preview_text AS assistant_payload_json,
                   CASE WHEN assistant_search.event_id IS NULL THEN NULL ELSE 'safe_preview' END AS assistant_redaction_state
            FROM semantic_anchor_page AS anchor
            LEFT JOIN runs AS r ON r.id = anchor.run_id
            LEFT JOIN sessions AS s ON s.id = anchor.session_id
            LEFT JOIN sessions AS rs ON rs.id = r.session_id
            LEFT JOIN events AS next_user ON next_user.id = CASE
                WHEN anchor.run_id IS NOT NULL THEN (
                    SELECT candidate_user.id
                    FROM events AS candidate_user
                    WHERE candidate_user.run_id = anchor.run_id
                      AND candidate_user.event_type = 'message'
                      AND candidate_user.role = 'user'
                      AND candidate_user.deleted_at_ms IS NULL
                      AND candidate_user.visibility != 'withheld'
                      AND candidate_user.sync_state != 'withheld'
                      AND EXISTS (
                          SELECT 1
                          FROM event_search_lookup AS candidate_user_search
                          WHERE candidate_user_search.event_id = candidate_user.id
                            AND length(trim(candidate_user_search.preview_text)) > 0
                            AND {candidate_user_predicate}
                      )
                      AND (
                            candidate_user.occurred_at_ms > anchor.occurred_at_ms
                            OR (candidate_user.occurred_at_ms = anchor.occurred_at_ms AND candidate_user.seq > anchor.seq)
                            OR (candidate_user.occurred_at_ms = anchor.occurred_at_ms AND candidate_user.seq = anchor.seq AND candidate_user.id > anchor.event_id)
                      )
                    ORDER BY candidate_user.occurred_at_ms ASC, candidate_user.seq ASC, candidate_user.id ASC
                    LIMIT 1
                )
                WHEN COALESCE(anchor.session_id, r.session_id) IS NOT NULL THEN (
                    SELECT candidate_user.id
                    FROM events AS candidate_user
                    WHERE candidate_user.run_id IS NULL
                      AND candidate_user.session_id = COALESCE(anchor.session_id, r.session_id)
                      AND candidate_user.event_type = 'message'
                      AND candidate_user.role = 'user'
                      AND candidate_user.deleted_at_ms IS NULL
                      AND candidate_user.visibility != 'withheld'
                      AND candidate_user.sync_state != 'withheld'
                      AND EXISTS (
                          SELECT 1
                          FROM event_search_lookup AS candidate_user_search
                          WHERE candidate_user_search.event_id = candidate_user.id
                            AND length(trim(candidate_user_search.preview_text)) > 0
                            AND {candidate_user_predicate}
                      )
                      AND (
                            candidate_user.occurred_at_ms > anchor.occurred_at_ms
                            OR (candidate_user.occurred_at_ms = anchor.occurred_at_ms AND candidate_user.seq > anchor.seq)
                            OR (candidate_user.occurred_at_ms = anchor.occurred_at_ms AND candidate_user.seq = anchor.seq AND candidate_user.id > anchor.event_id)
                      )
                    ORDER BY candidate_user.occurred_at_ms ASC, candidate_user.seq ASC, candidate_user.id ASC
                    LIMIT 1
                )
            END
            LEFT JOIN events AS assistant ON assistant.id = CASE
                WHEN anchor.run_id IS NOT NULL THEN (
                    SELECT candidate.id
                    FROM events AS candidate
                    WHERE candidate.run_id = anchor.run_id
                      AND candidate.event_type = 'message'
                      AND candidate.role = 'assistant'
                      AND candidate.deleted_at_ms IS NULL
                      AND candidate.visibility != 'withheld'
                      AND candidate.sync_state != 'withheld'
                      AND length(trim(candidate.payload_json)) > 2
                      AND EXISTS (
                          SELECT 1
                          FROM event_search_lookup AS candidate_search
                          WHERE candidate_search.event_id = candidate.id
                            AND length(trim(candidate_search.preview_text)) > 0
                      )
                      AND (
                            candidate.occurred_at_ms > anchor.occurred_at_ms
                            OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq > anchor.seq)
                            OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq = anchor.seq AND candidate.id > anchor.event_id)
                      )
                      AND (
                            next_user.id IS NULL
                            OR candidate.occurred_at_ms < next_user.occurred_at_ms
                            OR (candidate.occurred_at_ms = next_user.occurred_at_ms AND candidate.seq < next_user.seq)
                            OR (candidate.occurred_at_ms = next_user.occurred_at_ms AND candidate.seq = next_user.seq AND candidate.id < next_user.id)
                      )
                    ORDER BY candidate.occurred_at_ms DESC, candidate.seq DESC, candidate.id DESC
                    LIMIT 1
                )
                WHEN COALESCE(anchor.session_id, r.session_id) IS NOT NULL THEN (
                    SELECT candidate.id
                    FROM events AS candidate
                    WHERE candidate.run_id IS NULL
                      AND candidate.session_id = COALESCE(anchor.session_id, r.session_id)
                      AND candidate.event_type = 'message'
                      AND candidate.role = 'assistant'
                      AND candidate.deleted_at_ms IS NULL
                      AND candidate.visibility != 'withheld'
                      AND candidate.sync_state != 'withheld'
                      AND length(trim(candidate.payload_json)) > 2
                      AND EXISTS (
                          SELECT 1
                          FROM event_search_lookup AS candidate_search
                          WHERE candidate_search.event_id = candidate.id
                            AND length(trim(candidate_search.preview_text)) > 0
                      )
                      AND (
                            candidate.occurred_at_ms > anchor.occurred_at_ms
                            OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq > anchor.seq)
                            OR (candidate.occurred_at_ms = anchor.occurred_at_ms AND candidate.seq = anchor.seq AND candidate.id > anchor.event_id)
                      )
                      AND (
                            next_user.id IS NULL
                            OR candidate.occurred_at_ms < next_user.occurred_at_ms
                            OR (candidate.occurred_at_ms = next_user.occurred_at_ms AND candidate.seq < next_user.seq)
                            OR (candidate.occurred_at_ms = next_user.occurred_at_ms AND candidate.seq = next_user.seq AND candidate.id < next_user.id)
                      )
                    ORDER BY candidate.occurred_at_ms DESC, candidate.seq DESC, candidate.id DESC
                    LIMIT 1
                )
            END
            LEFT JOIN event_search_lookup AS assistant_search
              ON assistant_search.event_id = assistant.id
             AND length(trim(assistant_search.preview_text)) > 0
            LEFT JOIN capture_sources AS event_source ON event_source.id = anchor.capture_source_id
            LEFT JOIN capture_sources AS session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
            LEFT JOIN capture_sources AS run_source ON run_source.id = r.source_id
            LEFT JOIN history_records AS wr ON wr.id = COALESCE(anchor.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id)
        )
        "#
    )
}

pub(crate) fn rebuild_event_search_lookup_projection(conn: &Connection) -> Result<()> {
    if !event_search_lookup_table_ready(conn)? {
        return Ok(());
    }
    conn.execute("DELETE FROM event_search_lookup", [])?;
    populate_event_search_projection(conn, false, true, false)
}

fn populate_event_search_projection(
    conn: &Connection,
    include_event_search: bool,
    include_event_lookup: bool,
    include_event_scriptgram: bool,
) -> Result<()> {
    let mut stmt = conn.prepare(
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               e.session_id,
               e.role,
               e.event_type,
               e.payload_json,
               'safe_preview'
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
        let event_type = parse_text_enum::<EventType>(event_type)?;
        let role = parse_optional_text_enum::<EventRole>(role)?;
        let redaction_state = parse_text_enum::<RedactionState>(redaction_state)?;
        let preview = event_search_preview(event_type, role, &payload_json, redaction_state)?;
        if preview.trim().is_empty() {
            continue;
        }
        let role = role.map(|role| role.as_str());
        let rank_bucket = event_type.as_str();
        if let Some(insert_event_search) = insert_event_search.as_mut() {
            insert_event_search.execute(params![
                &event_id,
                &history_record_id,
                &session_id,
                role,
                &preview,
                rank_bucket
            ])?;
        }
        if let Some(insert_event_scriptgram) = insert_event_scriptgram.as_mut() {
            let token_text = scriptgram_index_text(&preview);
            if !token_text.is_empty() {
                insert_event_scriptgram.execute(params![
                    &event_id,
                    &history_record_id,
                    &session_id,
                    role,
                    token_text,
                    rank_bucket
                ])?;
            }
        }
        if semantic_lookup_event_parts(event_type, role) {
            if let Some(insert_event_lookup) = insert_event_lookup.as_mut() {
                insert_event_lookup.execute(params![
                    &event_id,
                    &history_record_id,
                    &session_id,
                    role,
                    &preview,
                    rank_bucket
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
    insert_event_search_projection_for_event_id(conn, event.id, event)
}

pub(crate) fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    let has_event_search = table_exists(conn, "event_search")?;
    let has_event_lookup = table_exists(conn, "event_search_lookup")?;
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }
    let event_id_text = event_id.to_string();
    if has_event_search {
        conn.execute(
            "DELETE FROM event_search WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    if has_event_scriptgram {
        conn.execute(
            "DELETE FROM event_search_scriptgram WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    if has_event_lookup {
        conn.execute(
            "DELETE FROM event_search_lookup WHERE event_id = ?1",
            params![&event_id_text],
        )?;
    }
    insert_event_search_projection_for_event_id(conn, event_id, event)
}

pub(crate) fn insert_event_search_projection_for_event_id(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    let has_event_search = table_exists(conn, "event_search")?;
    let has_event_lookup = table_exists(conn, "event_search_lookup")?;
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }
    if !event_searchable_event_parts(
        &event.payload,
        RedactionState::SafePreview,
        event.event_type,
        event.role,
        event.sync.visibility,
        event.sync.sync_state,
        event.sync.deleted_at.is_some(),
    ) {
        return Ok(());
    }
    let preview = event_search_preview_from_payload(
        event.event_type,
        event.role,
        &event.payload,
        RedactionState::SafePreview,
    );
    if preview.trim().is_empty() {
        return Ok(());
    }
    let event_id = event_id.to_string();
    let history_record_id = optional_uuid_string(event.history_record_id);
    let session_id = optional_uuid_string(event.session_id);
    let role = event.role.map(|role| role.as_str());
    let rank_bucket = event.event_type.as_str();
    if has_event_search {
        conn.prepare_cached(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?
        .execute(params![
            &event_id,
            &history_record_id,
            &session_id,
            role,
            &preview,
            rank_bucket,
        ])?;
    }
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
                &event_id,
                &history_record_id,
                &session_id,
                role,
                token_text,
                rank_bucket,
            ])?;
        }
    }
    if has_event_lookup && semantic_lookup_event_parts(event.event_type, role) {
        conn.prepare_cached(
            r#"
            INSERT INTO event_search_lookup
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )?
        .execute(params![
            &event_id,
            &history_record_id,
            &session_id,
            role,
            &preview,
            rank_bucket,
        ])?;
    }
    Ok(())
}

fn semantic_lookup_event_parts(event_type: EventType, role: Option<&str>) -> bool {
    event_type == EventType::Message && matches!(role, Some("user" | "assistant"))
}

pub(crate) fn semantic_searchable_event_count_from_stored_event(
    conn: &Connection,
    event_id: Uuid,
) -> Result<usize> {
    if !table_exists(conn, "events")? {
        return Ok(0);
    }
    let row = conn
        .query_row(
            r#"
            SELECT payload_json,
                   'safe_preview' AS redaction_state,
                   event_type,
                   role,
                   visibility,
                   sync_state,
                   deleted_at_ms
            FROM events
            WHERE id = ?1
            "#,
            params![event_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        payload_json,
        redaction_state,
        event_type,
        role,
        visibility,
        sync_state,
        deleted_at_ms,
    )) = row
    else {
        return Ok(0);
    };
    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
    Ok(usize::from(semantic_searchable_event_parts(
        &payload,
        parse_text_enum::<RedactionState>(redaction_state)?,
        parse_text_enum::<EventType>(event_type)?,
        parse_optional_text_enum::<EventRole>(role)?,
        parse_text_enum::<Visibility>(visibility)?,
        parse_text_enum::<SyncState>(sync_state)?,
        deleted_at_ms.is_some(),
    )))
}

pub(crate) fn semantic_searchable_event_count_for_event(event: &Event) -> usize {
    usize::from(semantic_searchable_event_parts(
        &event.payload,
        RedactionState::SafePreview,
        event.event_type,
        event.role,
        event.sync.visibility,
        event.sync.sync_state,
        event.sync.deleted_at.is_some(),
    ))
}

pub(crate) fn semantic_searchable_document_count_from_stored_event(
    conn: &Connection,
    event_id: Uuid,
) -> Result<usize> {
    semantic_searchable_event_count_from_stored_event(conn, event_id)
}

pub(crate) fn semantic_searchable_document_count_for_event(event: &Event) -> usize {
    semantic_searchable_event_count_for_event(event)
}

fn semantic_searchable_event_parts(
    payload: &serde_json::Value,
    redaction_state: RedactionState,
    event_type: EventType,
    role: Option<EventRole>,
    visibility: Visibility,
    sync_state: SyncState,
    deleted: bool,
) -> bool {
    if event_type != EventType::Message || role != Some(EventRole::User) {
        return false;
    }
    if !event_searchable_event_parts(
        payload,
        redaction_state,
        event_type,
        role,
        visibility,
        sync_state,
        deleted,
    ) {
        return false;
    }
    let preview = event_search_preview_from_payload(event_type, role, payload, redaction_state);
    !semantic_lite_turn_preview_is_control(&preview)
}

fn event_searchable_event_parts(
    payload: &serde_json::Value,
    redaction_state: RedactionState,
    event_type: EventType,
    role: Option<EventRole>,
    visibility: Visibility,
    sync_state: SyncState,
    deleted: bool,
) -> bool {
    if deleted
        || visibility == Visibility::Withheld
        || sync_state == SyncState::Withheld
        || matches!(
            redaction_state,
            RedactionState::Raw | RedactionState::Withheld
        )
    {
        return false;
    }
    !event_search_preview_from_payload(event_type, role, payload, redaction_state)
        .trim()
        .is_empty()
}

fn event_search_preview(
    event_type: EventType,
    role: Option<EventRole>,
    payload_json: &str,
    redaction_state: RedactionState,
) -> Result<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)?;
    Ok(event_search_preview_from_payload(
        event_type,
        role,
        &payload,
        redaction_state,
    ))
}

fn event_search_preview_from_payload(
    event_type: EventType,
    role: Option<EventRole>,
    payload: &serde_json::Value,
    redaction_state: RedactionState,
) -> String {
    if matches!(
        redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return String::new();
    }
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
    payload_json_or_preview: &str,
    source_metadata_json: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload_json_or_preview) {
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

fn event_embedding_document_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<EventEmbeddingDocument> {
    let preview_text: String = row.get(8)?;
    let redaction_state: String = row.get(9)?;
    let source_metadata_json = row.get::<_, Option<String>>(15)?;
    let source_identity = event_search_source_identity(source_metadata_json.as_deref())?;
    let assistant_preview_text = row.get::<_, Option<String>>(19)?;
    let assistant_redaction_state = row.get::<_, Option<String>>(20)?;
    Ok(EventEmbeddingDocument {
        event_id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        seq: row.get::<_, i64>(3)? as u64,
        occurred_at_ms: row.get(4)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
        rank_bucket: row.get(7)?,
        provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
        source_format: source_identity.source_format,
        agent_type: parse_optional_text_enum::<AgentType>(row.get(11)?)?,
        session_is_primary: row.get::<_, Option<i64>>(12)?.map(|value| value != 0),
        cwd: row.get(13)?,
        raw_source_path: row.get(14)?,
        record_title: row.get(16)?,
        record_kind: row.get(17)?,
        record_workspace: row.get(18)?,
        text: semantic_lite_turn_source_text(
            &preview_text,
            &redaction_state,
            assistant_preview_text.as_deref(),
            assistant_redaction_state.as_deref(),
        )?,
    })
}

fn event_semantic_source_text(
    preview_text: &str,
    redaction_state: &str,
) -> rusqlite::Result<String> {
    let redaction = parse_text_enum::<RedactionState>(redaction_state.to_owned())?;
    if matches!(redaction, RedactionState::Raw | RedactionState::Withheld) {
        return Ok("raw event payload withheld".to_owned());
    }
    Ok(local_preview(preview_text, SEMANTIC_TURN_TEXT_MAX_CHARS))
}

fn semantic_lite_turn_source_text(
    user_preview_text: &str,
    user_redaction_state: &str,
    assistant_preview_text: Option<&str>,
    assistant_redaction_state: Option<&str>,
) -> rusqlite::Result<String> {
    let user_text = event_semantic_source_text(user_preview_text, user_redaction_state)?;
    let mut sections = vec![format!("user:\n{}", user_text.trim())];
    if let (Some(payload_json), Some(redaction_state)) =
        (assistant_preview_text, assistant_redaction_state)
    {
        let assistant_text = event_semantic_source_text(payload_json, redaction_state)?;
        if !assistant_text.trim().is_empty() {
            sections.push(format!("assistant:\n{}", assistant_text.trim()));
        }
    }
    Ok(local_preview(
        &sections.join("\n\n"),
        SEMANTIC_TURN_TEXT_MAX_CHARS,
    ))
}

fn semantic_lite_turn_source_chunk(
    preview_text: &str,
    redaction_state: &str,
    assistant_preview_text: Option<&str>,
    assistant_redaction_state: Option<&str>,
    start_char: usize,
    end_char: usize,
) -> rusqlite::Result<String> {
    if end_char <= start_char {
        return Ok(String::new());
    }
    let text = semantic_lite_turn_source_text(
        preview_text,
        redaction_state,
        assistant_preview_text,
        assistant_redaction_state,
    )?;
    Ok(text
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect())
}

fn escape_like_term(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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
