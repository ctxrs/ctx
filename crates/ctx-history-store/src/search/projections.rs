use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    utc_now, AgentType, CaptureProvider, Event, EventRole, EventType, HistoryRecord,
    RedactionState, SyncState, Visibility,
};
use rusqlite::{params, params_from_iter, types::Value, Connection, ErrorCode, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_uuid_string,
    parse_optional_text_enum, parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::schema::ddl::{table_exists, table_has_column};
use crate::search::analyzer::{
    lexical_query_terms, scriptgram_index_text, scriptgram_match_clauses,
};
use crate::search::event_query::{
    event_search_hit_sql, event_search_score, lexical_event_search_query,
};
use crate::{sqlite_amplifying_write_estimate, Result, Store, StoreError};

const SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY: &str = "semantic_searchable_lite_turn_items_v3";
const SEMANTIC_TURN_TEXT_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_LITE_TURN_RANK_BUCKET: &str = "lite_turn";
const SEARCH_PROJECTION_INIT_COMPLETE_KEY: &str = "search_projection_init_v2:complete";
const SEARCH_PROJECTION_INIT_STAGE_KEY: &str = "search_projection_init_v2:stage";
const SEARCH_PROJECTION_INIT_CURSOR_KEY: &str = "search_projection_init_v2:cursor";
const SEARCH_PROJECTION_INIT_REVISION_KEY: &str = "search_projection_init_v2:revision";
const SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY: &str = "search_projection_init_v2:semantic_items";
const SEARCH_PROJECTION_CLEANUP_CURSOR_KEY: &str = "search_projection_cleanup_v1:cursor";
const SEARCH_PROJECTION_INTEGRITY_VERSION_KEY: &str = "search_projection_integrity_generation";
const SEARCH_PROJECTION_INTEGRITY_VERSION: i64 = 2;
const SEARCH_PROJECTION_CERTIFIED_REVISION_KEY: &str =
    "search_projection_integrity_certified_revision";
const SEARCH_PROJECTION_REPAIR_REQUEST_SUFFIX: &str = ".search-projection-repair-request";
const SEMANTIC_CONTENT_REVISION_STAT_KEY: &str = "semantic_content_revision_v1";
const SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION_KEY: &str =
    "semantic_content_revision_triggers_version";
const SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION: i64 = 2;
const SEARCH_PROJECTION_INIT_CLEAN_STAGE: i64 = 1;
const SEARCH_PROJECTION_INIT_RECORDS_STAGE: i64 = 2;
const SEARCH_PROJECTION_INIT_EVENTS_STAGE: i64 = 3;
const SEARCH_PROJECTION_INIT_PUBLISH_STAGE: i64 = 4;
const SEARCH_PROJECTION_INIT_BATCH_ROWS: usize = 64;
const SEARCH_PROJECTION_ACTIVE_TABLES: [&str; 6] = [
    "ctx_history_search",
    "ctx_history_search_scriptgram",
    "event_search",
    "event_search_scriptgram",
    "event_search_lookup",
    "artifact_search",
];
const SEARCH_PROJECTION_CLEAN_TABLES: [&str; 6] = [
    "ctx_history_search_rebuild",
    "ctx_history_search_scriptgram_rebuild",
    "event_search_rebuild",
    "event_search_scriptgram_rebuild",
    "event_search_lookup_rebuild",
    "artifact_search_rebuild",
];
const SEARCH_PROJECTION_TABLE_PAIRS: [(&str, &str, &str); 6] = [
    (
        "ctx_history_search",
        "ctx_history_search_rebuild",
        "ctx_history_search_publishing",
    ),
    (
        "ctx_history_search_scriptgram",
        "ctx_history_search_scriptgram_rebuild",
        "ctx_history_search_scriptgram_publishing",
    ),
    (
        "event_search",
        "event_search_rebuild",
        "event_search_publishing",
    ),
    (
        "event_search_scriptgram",
        "event_search_scriptgram_rebuild",
        "event_search_scriptgram_publishing",
    ),
    (
        "event_search_lookup",
        "event_search_lookup_rebuild",
        "event_search_lookup_publishing",
    ),
    (
        "artifact_search",
        "artifact_search_rebuild",
        "artifact_search_publishing",
    ),
];

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
        self.schedule_search_projection_refresh()?;
        while self.run_search_projection_maintenance_slice()? {}
        Ok(())
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        self.merge_all_fts_tables_bounded()
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        if self.search_projection_maintenance_pending()? {
            return Ok(false);
        }
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
        self.search_event_hits_page_with_ranking(query, limit, offset, false)
    }

    pub fn search_event_hits_page_prefer_conversation(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EventSearchHit>> {
        self.search_event_hits_page_with_ranking(query, limit, offset, true)
    }

    fn search_event_hits_page_with_ranking(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        prefer_conversation: bool,
    ) -> Result<Vec<EventSearchHit>> {
        self.search_event_hits_page_with_ranking_after_ready(
            query,
            limit,
            offset,
            prefer_conversation,
            || {},
        )
    }

    fn search_event_hits_page_with_ranking_after_ready(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        prefer_conversation: bool,
        after_ready: impl FnOnce(),
    ) -> Result<Vec<EventSearchHit>> {
        let result = self.with_read_snapshot(|| {
            self.search_event_hits_page_with_ranking_snapshot(
                query,
                limit,
                offset,
                prefer_conversation,
                after_ready,
            )
        });
        result.map_err(|error| self.handle_search_projection_query_error(error))
    }

    fn search_event_hits_page_with_ranking_snapshot(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        prefer_conversation: bool,
        after_ready: impl FnOnce(),
    ) -> Result<Vec<EventSearchHit>> {
        if !self.search_projection_ready()? {
            return Err(StoreError::SearchProjectionMaintenancePending);
        }
        if !table_exists(&self.conn, "event_search")? {
            return Err(StoreError::SearchProjectionUnavailable);
        }
        after_ready();
        let match_clauses = fts_match_clauses(query);
        let scriptgram_clauses = if event_scriptgram_table_ready(&self.conn)? {
            scriptgram_match_clauses(query)
        } else {
            Vec::new()
        };
        if match_clauses.is_empty() && scriptgram_clauses.is_empty() {
            return Ok(Vec::new());
        }

        if scriptgram_clauses.is_empty() {
            return self.search_event_hits_page_lexical(
                match_clauses,
                limit,
                offset,
                prefer_conversation,
            );
        }

        let mut selects = Vec::new();
        let mut values = Vec::<Value>::new();
        for (term_index, clause) in match_clauses.into_iter().enumerate() {
            values.push(Value::Text(clause));
            selects.push(format!(
                r#"SELECT event_search.event_id, {term_index}, bm25(event_search)
                   FROM event_search
                   WHERE event_search MATCH ?{}"#,
                values.len()
            ));
        }
        for (term_index, clause) in scriptgram_clauses {
            values.push(Value::Text(clause));
            selects.push(format!(
                r#"SELECT event_search_scriptgram.event_id, {term_index},
                          bm25(event_search_scriptgram) + 0.35
                   FROM event_search_scriptgram
                   WHERE event_search_scriptgram MATCH ?{}"#,
                values.len()
            ));
        }
        values.push(Value::Integer(limit.max(1) as i64));
        let limit_parameter = values.len();
        values.push(Value::Integer(offset as i64));
        let offset_parameter = values.len();
        let sql = format!(
            r#"
            WITH matches(event_id, term_index, score) AS MATERIALIZED (
                {}
            ),
            term_matches(event_id, term_index, score) AS (
                SELECT event_id, term_index, MIN(score)
                FROM matches
                GROUP BY event_id, term_index
            ),
            ranked(event_id, matched_terms, score) AS (
                SELECT event_id, COUNT(*), SUM(score)
                FROM term_matches
                GROUP BY event_id
            )
            {}
            LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}
            "#,
            selects.join(" UNION ALL "),
            event_search_hit_sql(
                "ranked JOIN event_search ON event_search.event_id = ranked.event_id",
                &event_search_score("ranked.score", prefer_conversation),
                "ORDER BY ranked.matched_terms DESC, search_score, e.occurred_at_ms DESC, e.seq DESC, event_search.event_id",
            )
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), event_search_hit_from_row)?;
        collect_rows(rows)
    }

    fn search_event_hits_page_lexical(
        &self,
        match_clauses: Vec<String>,
        limit: usize,
        offset: usize,
        prefer_conversation: bool,
    ) -> Result<Vec<EventSearchHit>> {
        let (sql, values) =
            lexical_event_search_query(match_clauses, limit, offset, prefer_conversation);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), event_search_hit_from_row)?;
        collect_rows(rows)
    }

    pub fn semantic_event_hits_by_id(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
    ) -> Result<Vec<EventSearchHit>> {
        if chunk_ranges.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_search_projection_ready_for_semantic()?;
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
        self.ensure_search_projection_ready_for_semantic()?;
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
        self.ensure_search_projection_ready_for_semantic()?;
        if let Some(count) = self.cached_event_embedding_document_count()? {
            return Ok(count);
        }
        semantic_searchable_item_count_exact(&self.conn)
    }

    pub fn refresh_event_embedding_document_count_cache(&self) -> Result<()> {
        self.with_write_transaction(|| {
            refresh_semantic_searchable_item_stats(&self.conn).map(|_| ())
        })
    }

    pub fn semantic_content_revision(&self) -> Result<u64> {
        Ok(semantic_content_revision_i64(&self.conn)?.max(0) as u64)
    }

    fn ensure_search_projection_ready_for_semantic(&self) -> Result<()> {
        if self.search_projection_ready()? {
            Ok(())
        } else {
            Err(StoreError::SearchProjectionMaintenancePending)
        }
    }

    pub fn recent_event_embedding_documents(
        &self,
        before: Option<(i64, u64)>,
        limit: usize,
    ) -> Result<Vec<EventEmbeddingDocument>> {
        self.ensure_search_projection_ready_for_semantic()?;
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
        self.ensure_search_projection_ready_for_semantic()?;
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
        self.ensure_search_projection_ready_for_semantic()?;
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

    pub fn semantic_event_snapshot(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
    ) -> Result<(Vec<EventEmbeddingDocument>, Vec<EventSearchHit>)> {
        self.semantic_event_snapshot_inner(chunk_ranges, || {})
    }

    #[cfg(test)]
    pub(crate) fn semantic_event_snapshot_after_documents(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
        after_documents: impl FnOnce(),
    ) -> Result<(Vec<EventEmbeddingDocument>, Vec<EventSearchHit>)> {
        self.semantic_event_snapshot_inner(chunk_ranges, after_documents)
    }

    fn semantic_event_snapshot_inner(
        &self,
        chunk_ranges: &HashMap<Uuid, (usize, usize)>,
        after_documents: impl FnOnce(),
    ) -> Result<(Vec<EventEmbeddingDocument>, Vec<EventSearchHit>)> {
        if chunk_ranges.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.conn.execute_batch("BEGIN DEFERRED")?;
        }
        let result = (|| {
            let event_ids = chunk_ranges.keys().copied().collect::<Vec<_>>();
            let documents = self.event_embedding_documents_by_ids(&event_ids)?;
            after_documents();
            let hits = self.semantic_event_hits_by_id(chunk_ranges)?;
            Ok((documents, hits))
        })();
        if !owns_transaction {
            return result;
        }
        match result {
            Ok(snapshot) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(snapshot)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(crate) fn ensure_search_projection_initialized(&self) -> Result<()> {
        let stats_exist = table_exists(&self.conn, "search_projection_stats")?;
        let maintenance_pending = stats_exist
            && search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some();
        let initialized = stats_exist
            && (search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY)?.is_some()
                || maintenance_pending);
        let triggers_current = initialized
            && search_projection_stat(&self.conn, SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION_KEY)?
                == Some(SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION);
        let integrity_current = initialized
            && search_projection_stat(&self.conn, SEARCH_PROJECTION_INTEGRITY_VERSION_KEY)?
                == Some(SEARCH_PROJECTION_INTEGRITY_VERSION)
            && search_projection_stat(&self.conn, SEARCH_PROJECTION_CERTIFIED_REVISION_KEY)?
                == Some(semantic_content_revision_i64(&self.conn)?);
        let active_tables_ready = active_search_projection_tables_ready(&self.conn)?;
        if triggers_current && integrity_current && active_tables_ready {
            return Ok(());
        }
        let estimated =
            sqlite_amplifying_write_estimate(&self.path, 2, crate::WAL_TRUNCATE_MIN_BYTES)?;
        self.ensure_disk_headroom(estimated, "search projection schema repair")?;
        self.with_write_transaction(|| {
            self.ensure_disk_headroom(estimated, "search projection schema repair")?;
            ensure_search_projection_stats_table(&self.conn)?;
            ensure_semantic_content_revision_tracking(&self.conn)?;
            let result = if maintenance_pending {
                ensure_search_projection_rebuild_tables(&self.conn)
            } else if initialized && active_tables_ready && integrity_current {
                Ok(())
            } else {
                self.initialize_search_projection_maintenance(true)
            };
            result?;
            self.ensure_disk_headroom(estimated, "search projection schema repair")
        })
    }

    pub(crate) fn consume_search_projection_repair_request(&self) -> Result<()> {
        let path = self.search_projection_repair_request_path();
        if !path.exists() {
            return Ok(());
        }
        self.schedule_search_projection_refresh()?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn handle_search_projection_query_error(&self, error: StoreError) -> StoreError {
        if !search_projection_error_requires_repair(&error) {
            return error;
        }
        if let Err(request_error) = self.persist_search_projection_repair_request() {
            return request_error;
        }
        if self.indexing_work_class().is_some() && self.schedule_search_projection_refresh().is_ok()
        {
            let _ = fs::remove_file(self.search_projection_repair_request_path());
        }
        StoreError::SearchProjectionMaintenancePending
    }

    fn search_projection_repair_request_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push(SEARCH_PROJECTION_REPAIR_REQUEST_SUFFIX);
        PathBuf::from(path)
    }

    fn persist_search_projection_repair_request(&self) -> Result<()> {
        let path = self.search_projection_repair_request_path();
        if path.exists() {
            return Ok(());
        }
        let temporary = path.with_extension(format!("repair-request-{}.tmp", std::process::id()));
        let _ = fs::remove_file(&temporary);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(b"ctx-search-projection-repair-v1\n")?;
        file.sync_all()?;
        drop(file);
        match fs::rename(&temporary, &path) {
            Ok(()) => Ok(()),
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(temporary);
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(error.into())
            }
        }
    }

    pub(crate) fn schedule_search_projection_refresh(&self) -> Result<()> {
        let estimated =
            sqlite_amplifying_write_estimate(&self.path, 2, crate::WAL_TRUNCATE_MIN_BYTES)?;
        self.ensure_disk_headroom(estimated, "search projection refresh scheduling")?;
        self.with_write_transaction(|| {
            self.ensure_disk_headroom(estimated, "search projection refresh scheduling")?;
            self.initialize_search_projection_maintenance(true)?;
            self.ensure_disk_headroom(estimated, "search projection refresh scheduling")
        })
    }

    fn initialize_search_projection_maintenance(&self, force: bool) -> Result<()> {
        ensure_search_projection_stats_table(&self.conn)?;
        ensure_semantic_content_revision_tracking(&self.conn)?;
        ensure_search_projection_rebuild_tables(&self.conn)?;
        if !force
            && (search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY)?.is_some()
                || search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some())
        {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM search_projection_stats WHERE key IN (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                SEARCH_PROJECTION_INIT_COMPLETE_KEY,
                SEARCH_PROJECTION_INIT_STAGE_KEY,
                SEARCH_PROJECTION_INIT_CURSOR_KEY,
                SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
                SEARCH_PROJECTION_INIT_REVISION_KEY,
                SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY,
                SEARCH_PROJECTION_INTEGRITY_VERSION_KEY,
                SEARCH_PROJECTION_CERTIFIED_REVISION_KEY,
            ],
        )?;
        if !table_has_rows(&self.conn, "history_records")?
            && !table_has_rows(&self.conn, "events")?
            && !search_projection_tables_have_rows(&self.conn)?
            && active_search_projection_tables_ready(&self.conn)?
        {
            set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY, 1)?;
            set_search_projection_stat(&self.conn, SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY, 0)?;
            set_search_projection_stat(
                &self.conn,
                SEARCH_PROJECTION_INTEGRITY_VERSION_KEY,
                SEARCH_PROJECTION_INTEGRITY_VERSION,
            )?;
            set_search_projection_stat(
                &self.conn,
                SEARCH_PROJECTION_CERTIFIED_REVISION_KEY,
                semantic_content_revision_i64(&self.conn)?,
            )?;
            return Ok(());
        }
        set_search_projection_stat(
            &self.conn,
            SEARCH_PROJECTION_INIT_STAGE_KEY,
            SEARCH_PROJECTION_INIT_CLEAN_STAGE,
        )?;
        set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, 0)?;
        set_search_projection_stat(
            &self.conn,
            SEARCH_PROJECTION_INIT_REVISION_KEY,
            semantic_content_revision_i64(&self.conn)?,
        )?;
        set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY, 0)
    }

    pub fn search_projection_ready(&self) -> Result<bool> {
        if search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
            return Ok(false);
        }
        Ok(
            search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY)?.is_some()
                && search_projection_stat(&self.conn, SEARCH_PROJECTION_INTEGRITY_VERSION_KEY)?
                    == Some(SEARCH_PROJECTION_INTEGRITY_VERSION)
                && search_projection_stat(&self.conn, SEARCH_PROJECTION_CERTIFIED_REVISION_KEY)?
                    == Some(semantic_content_revision_i64(&self.conn)?)
                && active_search_projection_tables_ready(&self.conn)?,
        )
    }

    pub(crate) fn search_projection_maintenance_pending(&self) -> Result<bool> {
        Ok(
            search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some()
                || search_projection_stat(&self.conn, SEARCH_PROJECTION_CLEANUP_CURSOR_KEY)?
                    .is_some(),
        )
    }

    pub(crate) fn run_search_projection_maintenance_slice(&self) -> Result<bool> {
        if !self.search_projection_maintenance_pending()? {
            return Ok(false);
        }
        let growth_estimate =
            if search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
                let estimated =
                    sqlite_amplifying_write_estimate(&self.path, 2, crate::WAL_TRUNCATE_MIN_BYTES)?;
                self.ensure_disk_headroom(estimated, "search projection rebuild")?;
                Some(estimated)
            } else {
                None
            };
        self.begin_immediate_batch()?;
        if let Some(estimated) = growth_estimate {
            if let Err(error) = self.ensure_disk_headroom(estimated, "search projection rebuild") {
                let _ = self.rollback_batch();
                return Err(error);
            }
        }
        let slice = match self.begin_indexing_slice() {
            Ok(slice) => slice,
            Err(error) => {
                let _ = self.rollback_batch();
                return Err(error);
            }
        };
        let result = self.run_search_projection_maintenance_transaction(&slice);
        if let Err(error) = result {
            let _ = self.rollback_batch();
            return Err(error);
        }
        if let Some(estimated) = growth_estimate {
            if let Err(error) = self.ensure_disk_headroom(estimated, "search projection rebuild") {
                let _ = self.rollback_batch();
                return Err(error);
            }
        }
        if let Err(error) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(error);
        }
        self.finish_indexing_slice(slice)?;
        self.search_projection_maintenance_pending()
    }

    fn run_search_projection_maintenance_transaction(
        &self,
        slice: &crate::IndexingSlice,
    ) -> Result<()> {
        let mut remaining = SEARCH_PROJECTION_INIT_BATCH_ROWS;
        while remaining > 0 {
            let Some(stage) = search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?
            else {
                return cleanup_published_search_projection(&self.conn, slice, &mut remaining);
            };
            let cursor =
                search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY)?.unwrap_or(0);
            if stage == SEARCH_PROJECTION_INIT_PUBLISH_STAGE {
                let target_revision =
                    search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_REVISION_KEY)?
                        .unwrap_or(0);
                if semantic_content_revision_i64(&self.conn)? != target_revision {
                    restart_search_projection_repair(&self.conn)?;
                    return Ok(());
                }
                if !search_projection_rebuild_integrity_valid(&self.conn)? {
                    restart_search_projection_repair(&self.conn)?;
                    return Ok(());
                }
                publish_search_projection(&self.conn)?;
                let semantic_items =
                    search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY)?
                        .unwrap_or(0);
                self.conn.execute(
                    "DELETE FROM search_projection_stats WHERE key IN (?1, ?2, ?3, ?4)",
                    params![
                        SEARCH_PROJECTION_INIT_STAGE_KEY,
                        SEARCH_PROJECTION_INIT_CURSOR_KEY,
                        SEARCH_PROJECTION_INIT_REVISION_KEY,
                        SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY,
                    ],
                )?;
                set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY, 1)?;
                set_search_projection_stat(
                    &self.conn,
                    SEARCH_PROJECTION_INTEGRITY_VERSION_KEY,
                    SEARCH_PROJECTION_INTEGRITY_VERSION,
                )?;
                set_search_projection_stat(
                    &self.conn,
                    SEARCH_PROJECTION_CERTIFIED_REVISION_KEY,
                    target_revision,
                )?;
                set_search_projection_stat(
                    &self.conn,
                    SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
                    semantic_items,
                )?;
                set_search_projection_stat(&self.conn, SEARCH_PROJECTION_CLEANUP_CURSOR_KEY, 0)?;
                return Ok(());
            }
            if stage == SEARCH_PROJECTION_INIT_CLEAN_STAGE {
                let table_index = usize::try_from(cursor)
                    .unwrap_or_else(|_| unreachable!("invalid search projection clean cursor"));
                let Some(table) = SEARCH_PROJECTION_CLEAN_TABLES.get(table_index) else {
                    set_search_projection_stat(
                        &self.conn,
                        SEARCH_PROJECTION_INIT_STAGE_KEY,
                        SEARCH_PROJECTION_INIT_RECORDS_STAGE,
                    )?;
                    set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, 0)?;
                    continue;
                };
                let deleted = clean_search_projection_rebuild_rows(&self.conn, table, remaining)?;
                if deleted < remaining {
                    set_search_projection_stat(
                        &self.conn,
                        SEARCH_PROJECTION_INIT_CURSOR_KEY,
                        cursor + 1,
                    )?;
                }
                remaining -= deleted;
                if remaining == 0 || (deleted > 0 && self.indexing_slice_should_rotate(slice)?) {
                    return Ok(());
                }
                continue;
            }
            let table = match stage {
                SEARCH_PROJECTION_INIT_RECORDS_STAGE => "history_records",
                SEARCH_PROJECTION_INIT_EVENTS_STAGE => "events",
                _ => unreachable!("invalid search projection initialization stage {stage}"),
            };
            let rows = projection_source_rows(&self.conn, table, cursor, remaining)?;
            if rows.is_empty() {
                if stage == SEARCH_PROJECTION_INIT_RECORDS_STAGE {
                    set_search_projection_stat(
                        &self.conn,
                        SEARCH_PROJECTION_INIT_STAGE_KEY,
                        SEARCH_PROJECTION_INIT_EVENTS_STAGE,
                    )?;
                    set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, 0)?;
                    continue;
                }
                set_search_projection_stat(
                    &self.conn,
                    SEARCH_PROJECTION_INIT_STAGE_KEY,
                    SEARCH_PROJECTION_INIT_PUBLISH_STAGE,
                )?;
                set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, 0)?;
                continue;
            }

            for (rowid, id) in rows {
                match stage {
                    SEARCH_PROJECTION_INIT_RECORDS_STAGE => {
                        let record = self.get_record(parse_uuid(id)?)?;
                        upsert_record_search_projection_in_tables(
                            &self.conn,
                            &record,
                            "ctx_history_search_rebuild",
                            "ctx_history_search_scriptgram_rebuild",
                        )?;
                    }
                    SEARCH_PROJECTION_INIT_EVENTS_STAGE => {
                        let event_id = parse_uuid(id)?;
                        let event = self.get_event(event_id)?;
                        upsert_event_search_projection_for_event_in_tables(
                            &self.conn,
                            event_id,
                            &event,
                            EventProjectionTables::REBUILD,
                        )?;
                        if semantic_searchable_document_count_for_event(&event) > 0 {
                            increment_search_projection_stat(
                                &self.conn,
                                SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY,
                                1,
                            )?;
                        }
                    }
                    _ => unreachable!(),
                }
                set_search_projection_stat(&self.conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, rowid)?;
                remaining -= 1;
                if remaining == 0 || self.indexing_slice_should_rotate(slice)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn normalize_legacy_blob_paths(&self) -> Result<()> {
        self.conn.execute(
                "UPDATE artifacts SET blob_path = 'objects/' || substr(blob_path, 7) WHERE blob_path LIKE 'blobs/%'",
                [],
            )?;
        Ok(())
    }
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

pub(crate) fn upsert_record_search_projection(
    conn: &Connection,
    record: &HistoryRecord,
) -> Result<()> {
    upsert_record_search_projection_in_tables(
        conn,
        record,
        "ctx_history_search",
        "ctx_history_search_scriptgram",
    )?;
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
        upsert_record_search_projection_in_tables(
            conn,
            record,
            "ctx_history_search_rebuild",
            "ctx_history_search_scriptgram_rebuild",
        )?;
    }
    certify_active_search_projection_revision(conn)
}

pub(crate) fn delete_record_search_projection(conn: &Connection, record_id: &str) -> Result<()> {
    for (search_table, scriptgram_table) in [
        ("ctx_history_search", "ctx_history_search_scriptgram"),
        (
            "ctx_history_search_rebuild",
            "ctx_history_search_scriptgram_rebuild",
        ),
    ] {
        if table_exists(conn, search_table)? {
            conn.execute(
                &format!("DELETE FROM {search_table} WHERE record_id = ?1"),
                params![record_id],
            )?;
        }
        if table_exists(conn, scriptgram_table)? {
            conn.execute(
                &format!("DELETE FROM {scriptgram_table} WHERE record_id = ?1"),
                params![record_id],
            )?;
        }
    }
    certify_active_search_projection_revision(conn)
}

fn upsert_record_search_projection_in_tables(
    conn: &Connection,
    record: &HistoryRecord,
    search_table: &str,
    scriptgram_table: &str,
) -> Result<()> {
    let valid_tables = [
        ("ctx_history_search", "ctx_history_search_scriptgram"),
        (
            "ctx_history_search_rebuild",
            "ctx_history_search_scriptgram_rebuild",
        ),
    ];
    if !valid_tables.contains(&(search_table, scriptgram_table)) {
        unreachable!("invalid record search projection tables");
    }
    if !table_exists(conn, search_table)? {
        return Ok(());
    }
    let has_scriptgram =
        fts_table_has_columns(conn, scriptgram_table, &["record_id", "token_text"])?;
    conn.execute(
        &format!("DELETE FROM {search_table} WHERE record_id = ?1"),
        params![record.id.to_string()],
    )?;
    if has_scriptgram {
        conn.execute(
            &format!("DELETE FROM {scriptgram_table} WHERE record_id = ?1"),
            params![record.id.to_string()],
        )?;
    }
    conn.execute(
        &format!(
            r#"
        INSERT INTO {search_table}
        (record_id, title, summary, primary_user_text, decision_text, context_text, tag_text)
        VALUES (?1, ?2, ?3, ?4, '', ?5, ?6)
        "#
        ),
        params![
            record.id.to_string(),
            local_preview(&record.title, 512),
            local_preview(&record.body, 2048),
            local_preview(&record.body, 2048),
            "",
            local_preview(&record.tags.join(" "), 1024),
        ],
    )?;
    if has_scriptgram {
        let token_text = scriptgram_index_text(&record_search_scriptgram_source(record));
        if !token_text.is_empty() {
            conn.execute(
                &format!(
                    r#"
                INSERT INTO {scriptgram_table}
                (record_id, token_text)
                VALUES (?1, ?2)
                "#
                ),
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
        | "ctx_history_search_scriptgram"
        | "artifact_search_rebuild"
        | "event_search_rebuild"
        | "event_search_scriptgram_rebuild"
        | "event_search_lookup_rebuild"
        | "ctx_history_search_rebuild"
        | "ctx_history_search_scriptgram_rebuild" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn table_has_rows(conn: &Connection, table: &str) -> Result<bool> {
    match table {
        "events" | "history_records" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)");
    Ok(conn.query_row(&sql, [], |row| row.get(0))?)
}

fn search_projection_tables_have_rows(conn: &Connection) -> Result<bool> {
    for table in SEARCH_PROJECTION_ACTIVE_TABLES {
        if !table_exists(conn, table)? {
            continue;
        }
        let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} LIMIT 1)");
        if conn.query_row(&sql, [], |row| row.get::<_, bool>(0))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn clean_search_projection_rebuild_rows(
    conn: &Connection,
    table: &str,
    limit: usize,
) -> Result<usize> {
    if !SEARCH_PROJECTION_CLEAN_TABLES.contains(&table) {
        unreachable!("invalid search projection table {table}");
    }
    if !table_exists(conn, table)? {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} ORDER BY rowid LIMIT ?1)"
    );
    Ok(conn.execute(&sql, params![limit as i64])?)
}

fn projection_source_rows(
    conn: &Connection,
    table: &str,
    after_rowid: i64,
    limit: usize,
) -> Result<Vec<(i64, String)>> {
    match table {
        "events" | "history_records" => {}
        _ => unreachable!("invalid table {table}"),
    }
    let sql = format!("SELECT rowid, id FROM {table} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![after_rowid, limit as i64], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    collect_rows(rows)
}

fn search_projection_stat(conn: &Connection, key: &str) -> Result<Option<i64>> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn set_search_projection_stat(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![key, value, utc_now().timestamp_millis()],
    )?;
    Ok(())
}

fn restart_search_projection_repair(conn: &Connection) -> Result<()> {
    set_search_projection_stat(
        conn,
        SEARCH_PROJECTION_INIT_STAGE_KEY,
        SEARCH_PROJECTION_INIT_CLEAN_STAGE,
    )?;
    set_search_projection_stat(conn, SEARCH_PROJECTION_INIT_CURSOR_KEY, 0)?;
    set_search_projection_stat(
        conn,
        SEARCH_PROJECTION_INIT_REVISION_KEY,
        semantic_content_revision_i64(conn)?,
    )?;
    set_search_projection_stat(conn, SEARCH_PROJECTION_INIT_SEMANTIC_ITEMS_KEY, 0)?;
    conn.execute(
        "DELETE FROM search_projection_stats WHERE key IN (?1, ?2)",
        params![
            SEARCH_PROJECTION_INIT_COMPLETE_KEY,
            SEARCH_PROJECTION_INTEGRITY_VERSION_KEY,
        ],
    )?;
    Ok(())
}

fn increment_search_projection_stat(conn: &Connection, key: &str, delta: i64) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, MAX(?2, 0), ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = MAX(search_projection_stats.value + ?2, 0),
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![key, delta, utc_now().timestamp_millis()],
    )?;
    Ok(())
}

fn semantic_content_revision_i64(conn: &Connection) -> Result<i64> {
    Ok(search_projection_stat(conn, SEMANTIC_CONTENT_REVISION_STAT_KEY)?.unwrap_or(0))
}

fn ensure_semantic_content_revision_tracking(conn: &Connection) -> Result<()> {
    set_search_projection_stat(
        conn,
        SEMANTIC_CONTENT_REVISION_STAT_KEY,
        semantic_content_revision_i64(conn)?.max(1),
    )?;
    if search_projection_stat(conn, SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION_KEY)?
        == Some(SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION)
    {
        return Ok(());
    }
    for (table, suffix, update_when) in [
        (
            "events",
            "events",
            "OLD.seq IS NOT NEW.seq OR OLD.history_record_id IS NOT NEW.history_record_id OR OLD.session_id IS NOT NEW.session_id OR OLD.run_id IS NOT NEW.run_id OR OLD.event_type IS NOT NEW.event_type OR OLD.role IS NOT NEW.role OR OLD.occurred_at_ms IS NOT NEW.occurred_at_ms OR OLD.capture_source_id IS NOT NEW.capture_source_id OR OLD.payload_json IS NOT NEW.payload_json OR OLD.visibility IS NOT NEW.visibility OR OLD.sync_state IS NOT NEW.sync_state OR OLD.deleted_at_ms IS NOT NEW.deleted_at_ms",
        ),
        (
            "history_records",
            "records",
            "OLD.title IS NOT NEW.title OR OLD.kind IS NOT NEW.kind OR OLD.workspace IS NOT NEW.workspace",
        ),
        (
            "sessions",
            "sessions",
            "OLD.history_record_id IS NOT NEW.history_record_id OR OLD.parent_session_id IS NOT NEW.parent_session_id OR OLD.root_session_id IS NOT NEW.root_session_id OR OLD.capture_source_id IS NOT NEW.capture_source_id OR OLD.provider IS NOT NEW.provider OR OLD.external_session_id IS NOT NEW.external_session_id OR OLD.agent_type IS NOT NEW.agent_type OR OLD.is_primary IS NOT NEW.is_primary",
        ),
        (
            "capture_sources",
            "sources",
            "OLD.provider IS NOT NEW.provider OR OLD.cwd IS NOT NEW.cwd OR OLD.raw_source_path IS NOT NEW.raw_source_path OR OLD.metadata_json IS NOT NEW.metadata_json",
        ),
        (
            "runs",
            "runs",
            "OLD.history_record_id IS NOT NEW.history_record_id OR OLD.session_id IS NOT NEW.session_id OR OLD.source_id IS NOT NEW.source_id",
        ),
    ] {
        if !table_exists(conn, table)? {
            continue;
        }
        for (operation, timing, when_clause) in [
            ("insert", "AFTER INSERT", ""),
            ("update", "AFTER UPDATE", update_when),
            ("delete", "AFTER DELETE", ""),
        ] {
            let trigger = format!("semantic_content_revision_{suffix}_{operation}");
            conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))?;
            let when_clause = if when_clause.is_empty() {
                String::new()
            } else {
                format!("WHEN {when_clause}")
            };
            conn.execute_batch(&format!(
                r#"
                CREATE TRIGGER {trigger}
                {timing} ON {table}
                {when_clause}
                BEGIN
                    UPDATE search_projection_stats
                    SET value = value + 1,
                        updated_at_ms = CAST(strftime('%s', 'now') AS INTEGER) * 1000
                    WHERE key = '{SEMANTIC_CONTENT_REVISION_STAT_KEY}';
                END;
                "#
            ))?;
        }
    }
    set_search_projection_stat(
        conn,
        SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION_KEY,
        SEMANTIC_CONTENT_REVISION_TRIGGERS_VERSION,
    )
}

fn cleanup_published_search_projection(
    conn: &Connection,
    _slice: &crate::IndexingSlice,
    remaining: &mut usize,
) -> Result<()> {
    let Some(cursor) = search_projection_stat(conn, SEARCH_PROJECTION_CLEANUP_CURSOR_KEY)? else {
        return Ok(());
    };
    let table_index = usize::try_from(cursor)
        .unwrap_or_else(|_| unreachable!("invalid search projection cleanup cursor"));
    let Some(table) = SEARCH_PROJECTION_CLEAN_TABLES.get(table_index) else {
        recreate_empty_search_projection_rebuild_tables(conn)?;
        conn.execute(
            "DELETE FROM search_projection_stats WHERE key = ?1",
            params![SEARCH_PROJECTION_CLEANUP_CURSOR_KEY],
        )?;
        return Ok(());
    };
    let deleted = clean_search_projection_rebuild_rows(conn, table, *remaining)?;
    if deleted < *remaining {
        set_search_projection_stat(conn, SEARCH_PROJECTION_CLEANUP_CURSOR_KEY, cursor + 1)?;
    }
    *remaining = (*remaining).saturating_sub(deleted);
    Ok(())
}

fn ensure_search_projection_rebuild_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS ctx_history_search_rebuild USING fts5(
            record_id UNINDEXED,
            title,
            summary,
            primary_user_text,
            decision_text,
            context_text,
            tag_text
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS event_search_rebuild USING fts5(
            event_id UNINDEXED,
            history_record_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            preview_text,
            rank_bucket UNINDEXED
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS artifact_search_rebuild USING fts5(
            artifact_id UNINDEXED,
            history_record_id UNINDEXED,
            preview_text
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS ctx_history_search_scriptgram_rebuild USING fts5(
            record_id UNINDEXED,
            token_text
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS event_search_scriptgram_rebuild USING fts5(
            event_id UNINDEXED,
            history_record_id UNINDEXED,
            session_id UNINDEXED,
            role UNINDEXED,
            token_text,
            rank_bucket UNINDEXED
        );
        CREATE TABLE IF NOT EXISTS event_search_lookup_rebuild (
            event_id TEXT PRIMARY KEY NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            history_record_id TEXT REFERENCES history_records(id),
            session_id TEXT REFERENCES sessions(id),
            role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
            preview_text TEXT NOT NULL,
            rank_bucket TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn search_projection_rebuild_integrity_valid(conn: &Connection) -> Result<bool> {
    for table in [
        "ctx_history_search_rebuild",
        "ctx_history_search_scriptgram_rebuild",
        "event_search_rebuild",
        "event_search_scriptgram_rebuild",
        "artifact_search_rebuild",
    ] {
        let sql = format!("INSERT INTO {table}({table}, rank) VALUES ('integrity-check', 1)");
        match conn.execute(&sql, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::AuthorizationForStatementDenied
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn search_projection_error_requires_repair(error: &StoreError) -> bool {
    let StoreError::Sql(sqlite_error) = error else {
        return false;
    };
    if matches!(
        sqlite_error,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(error.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    ) {
        return true;
    }
    let message = sqlite_error.to_string().to_ascii_lowercase();
    message.contains("database disk image is malformed")
        || message.contains("malformed fts")
        || message.contains("fts5") && message.contains("corrupt")
}

fn publish_search_projection(conn: &Connection) -> Result<()> {
    for (active, rebuild, publishing) in SEARCH_PROJECTION_TABLE_PAIRS {
        if table_exists(conn, publishing)? {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        if table_exists(conn, active)? {
            conn.execute(&format!("ALTER TABLE {active} RENAME TO {publishing}"), [])?;
            conn.execute(&format!("ALTER TABLE {rebuild} RENAME TO {active}"), [])?;
            conn.execute(&format!("ALTER TABLE {publishing} RENAME TO {rebuild}"), [])?;
        } else {
            conn.execute(&format!("ALTER TABLE {rebuild} RENAME TO {active}"), [])?;
        }
    }
    ensure_search_projection_rebuild_tables(conn)?;
    Ok(())
}

fn recreate_empty_search_projection_rebuild_tables(conn: &Connection) -> Result<()> {
    for table in SEARCH_PROJECTION_CLEAN_TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
    }
    ensure_search_projection_rebuild_tables(conn)
}

fn event_search_lookup_table_ready(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")?
        && table_has_column(conn, "event_search_lookup", "history_record_id")?
        && table_has_column(conn, "event_search_lookup", "preview_text")?)
}

fn active_search_projection_tables_ready(conn: &Connection) -> Result<bool> {
    for (table, columns) in [
        (
            "ctx_history_search",
            &[
                "record_id",
                "title",
                "summary",
                "primary_user_text",
                "decision_text",
                "context_text",
                "tag_text",
            ][..],
        ),
        (
            "ctx_history_search_scriptgram",
            &["record_id", "token_text"][..],
        ),
        (
            "event_search",
            &[
                "event_id",
                "history_record_id",
                "session_id",
                "role",
                "preview_text",
                "rank_bucket",
            ][..],
        ),
        (
            "event_search_scriptgram",
            &[
                "event_id",
                "history_record_id",
                "session_id",
                "role",
                "token_text",
                "rank_bucket",
            ][..],
        ),
        (
            "artifact_search",
            &["artifact_id", "history_record_id", "preview_text"][..],
        ),
    ] {
        if !fts5_table_ready(conn, table, columns)? {
            return Ok(false);
        }
    }
    event_search_lookup_table_ready(conn)
}

fn fts5_table_ready(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    let sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(false);
    };
    if !sql.to_ascii_lowercase().contains("using fts5") {
        return Ok(false);
    }
    for column in columns {
        if !table_has_column(conn, table, column)? {
            return Ok(false);
        }
    }
    Ok(true)
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

fn cached_semantic_searchable_item_count(conn: &Connection) -> Result<Option<usize>> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(None);
    }
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
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
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
        conn.execute(
            "DELETE FROM search_projection_stats WHERE key = ?1",
            params![SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY],
        )?;
        return Ok(count);
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
) -> Result<()> {
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
        conn.execute(
            "DELETE FROM search_projection_stats WHERE key = ?1",
            params![SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY],
        )?;
        return Ok(());
    }
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

#[derive(Clone, Copy)]
struct EventProjectionTables {
    search: &'static str,
    scriptgram: &'static str,
    lookup: &'static str,
}

impl EventProjectionTables {
    const ACTIVE: Self = Self {
        search: "event_search",
        scriptgram: "event_search_scriptgram",
        lookup: "event_search_lookup",
    };
    const REBUILD: Self = Self {
        search: "event_search_rebuild",
        scriptgram: "event_search_scriptgram_rebuild",
        lookup: "event_search_lookup_rebuild",
    };
}

pub(crate) fn insert_event_search_projection_for_event(
    conn: &Connection,
    event: &Event,
) -> Result<()> {
    insert_event_search_projection_for_event_id(conn, event.id, event)?;
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
        insert_event_search_projection_for_event_id_in_tables(
            conn,
            event.id,
            event,
            EventProjectionTables::REBUILD,
        )?;
    }
    certify_active_search_projection_revision(conn)
}

pub(crate) fn upsert_event_search_projection_for_event(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    upsert_event_search_projection_for_event_in_tables(
        conn,
        event_id,
        event,
        EventProjectionTables::ACTIVE,
    )?;
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some() {
        upsert_event_search_projection_for_event_in_tables(
            conn,
            event_id,
            event,
            EventProjectionTables::REBUILD,
        )?;
    }
    certify_active_search_projection_revision(conn)
}

pub(crate) fn certify_active_search_projection_revision(conn: &Connection) -> Result<()> {
    if search_projection_stat(conn, SEARCH_PROJECTION_INIT_STAGE_KEY)?.is_some()
        || search_projection_stat(conn, SEARCH_PROJECTION_INIT_COMPLETE_KEY)?.is_none()
        || search_projection_stat(conn, SEARCH_PROJECTION_INTEGRITY_VERSION_KEY)?
            != Some(SEARCH_PROJECTION_INTEGRITY_VERSION)
    {
        return Ok(());
    }
    let revision = semantic_content_revision_i64(conn)?;
    if search_projection_stat(conn, SEARCH_PROJECTION_CERTIFIED_REVISION_KEY)? == Some(revision) {
        return Ok(());
    }
    set_search_projection_stat(conn, SEARCH_PROJECTION_CERTIFIED_REVISION_KEY, revision)
}

fn upsert_event_search_projection_for_event_in_tables(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
    tables: EventProjectionTables,
) -> Result<()> {
    let has_event_search = table_exists(conn, tables.search)?;
    let has_event_lookup = table_exists(conn, tables.lookup)?;
    let has_event_scriptgram = fts_table_has_columns(
        conn,
        tables.scriptgram,
        &[
            "event_id",
            "history_record_id",
            "session_id",
            "role",
            "token_text",
            "rank_bucket",
        ],
    )?;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }
    let event_id_text = event_id.to_string();
    if has_event_search {
        conn.execute(
            &format!("DELETE FROM {} WHERE event_id = ?1", tables.search),
            params![&event_id_text],
        )?;
    }
    if has_event_scriptgram {
        conn.execute(
            &format!("DELETE FROM {} WHERE event_id = ?1", tables.scriptgram),
            params![&event_id_text],
        )?;
    }
    if has_event_lookup {
        conn.execute(
            &format!("DELETE FROM {} WHERE event_id = ?1", tables.lookup),
            params![&event_id_text],
        )?;
    }
    insert_event_search_projection_for_event_id_in_tables(conn, event_id, event, tables)
}

pub(crate) fn insert_event_search_projection_for_event_id(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
) -> Result<()> {
    insert_event_search_projection_for_event_id_in_tables(
        conn,
        event_id,
        event,
        EventProjectionTables::ACTIVE,
    )
}

fn insert_event_search_projection_for_event_id_in_tables(
    conn: &Connection,
    event_id: Uuid,
    event: &Event,
    tables: EventProjectionTables,
) -> Result<()> {
    let has_event_search = table_exists(conn, tables.search)?;
    let has_event_lookup = table_exists(conn, tables.lookup)?;
    let has_event_scriptgram = fts_table_has_columns(
        conn,
        tables.scriptgram,
        &[
            "event_id",
            "history_record_id",
            "session_id",
            "role",
            "token_text",
            "rank_bucket",
        ],
    )?;
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
        conn.execute(
            &format!(
                r#"
            INSERT INTO {}
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
                tables.search
            ),
            params![
                &event_id,
                &history_record_id,
                &session_id,
                role,
                &preview,
                rank_bucket,
            ],
        )?;
    }
    if has_event_scriptgram {
        let token_text = scriptgram_index_text(&preview);
        if !token_text.is_empty() {
            conn.execute(
                &format!(
                    r#"
                INSERT INTO {}
                (event_id, history_record_id, session_id, role, token_text, rank_bucket)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                    tables.scriptgram
                ),
                params![
                    &event_id,
                    &history_record_id,
                    &session_id,
                    role,
                    token_text,
                    rank_bucket,
                ],
            )?;
        }
    }
    if has_event_lookup && semantic_lookup_event_parts(event.event_type, role) {
        conn.execute(
            &format!(
                r#"
            INSERT INTO {}
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
                tables.lookup
            ),
            params![
                &event_id,
                &history_record_id,
                &session_id,
                role,
                &preview,
                rank_bucket,
            ],
        )?;
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
    let terms = fts_match_clauses(query);
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

pub(crate) fn fts_match_clauses(query: &str) -> Vec<String> {
    lexical_query_terms(query)
        .into_iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect()
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

#[cfg(test)]
mod maintenance_tests {
    use std::{cell::Cell, rc::Rc};

    use chrono::{DateTime, Utc};
    use ctx_history_core::{
        new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
    };
    use rusqlite::hooks::{AuthAction, Authorization};

    use super::*;
    use crate::work_control::install_test_disk_space_probe;

    fn semantic_event(seq: u64) -> Event {
        Event {
            id: new_id(),
            seq,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: DateTime::parse_from_rfc3339("2026-07-14T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            capture_source_id: None,
            payload: serde_json::json!({ "text": format!("semantic bootstrap item {seq}") }),
            payload_blob_id: None,
            dedupe_key: None,
            sync: SyncMetadata {
                visibility: Visibility::LocalOnly,
                fidelity: Fidelity::Imported,
                sync_state: SyncState::LocalOnly,
                sync_version: 0,
                deleted_at: None,
                metadata: serde_json::json!({}),
            },
        }
    }

    #[test]
    fn partial_bootstrap_hides_semantic_count_until_completion() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        for seq in 0..(SEARCH_PROJECTION_INIT_BATCH_ROWS + 6) {
            store.upsert_event(&semantic_event(seq as u64)).unwrap();
        }
        assert_eq!(
            store.cached_event_embedding_document_count().unwrap(),
            Some(SEARCH_PROJECTION_INIT_BATCH_ROWS + 6)
        );

        store
            .conn
            .execute_batch(
                "DELETE FROM event_search;\
                 DELETE FROM event_search_scriptgram;\
                 DELETE FROM event_search_lookup;\
                 DELETE FROM search_projection_stats \
                 WHERE key LIKE 'search_projection_init_v2:%';",
            )
            .unwrap();
        store.ensure_search_projection_initialized().unwrap();
        assert!(store.search_projection_maintenance_pending().unwrap());
        assert_eq!(
            search_projection_stat(&store.conn, SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY).unwrap(),
            None
        );

        assert!(store.run_search_projection_maintenance_slice().unwrap());
        assert!(matches!(
            store.search_event_hits("semantic bootstrap item", 100),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        drop(store);

        let store = Store::open(&db_path).unwrap();
        assert!(store.search_projection_maintenance_pending().unwrap());
        assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 0);
        assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
        assert!(matches!(
            store.count_event_embedding_documents(),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        assert!(matches!(
            store.search_event_hits("semantic bootstrap item", 100),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        let concurrent_reader = Store::open_read_only(&db_path).unwrap();
        assert!(!concurrent_reader.search_projection_ready().unwrap());
        assert!(matches!(
            concurrent_reader.search_event_hits("semantic bootstrap item", 100),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        drop(concurrent_reader);

        store
            .refresh_event_embedding_document_count_cache()
            .unwrap();
        store.upsert_event(&semantic_event(10_000)).unwrap();
        assert_eq!(
            search_projection_stat(&store.conn, SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY).unwrap(),
            None
        );

        while store.run_search_projection_maintenance_slice().unwrap() {}
        let complete_count = SEARCH_PROJECTION_INIT_BATCH_ROWS + 7;
        assert!(!store.search_projection_maintenance_pending().unwrap());
        assert_eq!(
            store.count_event_embedding_documents_exact().unwrap(),
            complete_count
        );
        assert_eq!(
            store.cached_event_embedding_document_count().unwrap(),
            Some(complete_count)
        );
        assert_eq!(
            store.count_event_embedding_documents().unwrap(),
            complete_count
        );
        assert!(!store
            .search_event_hits("semantic bootstrap item", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn lexical_readiness_and_match_share_one_snapshot_during_refresh_race() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let event = semantic_event(1);
        let event_id = event.id;
        store.upsert_event(&event).unwrap();
        assert!(store.search_projection_ready().unwrap());
        let concurrent = Store::open(&db_path).unwrap();

        let hits = store
            .search_event_hits_page_with_ranking_after_ready("bootstrap", 10, 0, false, || {
                concurrent.schedule_search_projection_refresh().unwrap()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].event_id, event_id);
        assert!(!concurrent.search_projection_ready().unwrap());
    }

    #[test]
    fn projection_batch_revalidates_disk_at_final_precommit_point() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&semantic_event(1)).unwrap();
        store.schedule_search_projection_refresh().unwrap();
        let checks = Rc::new(Cell::new(0_usize));
        let hook_checks = Rc::clone(&checks);
        let probe = install_test_disk_space_probe(move |_path, operation| {
            if operation == "search projection rebuild" {
                let current = hook_checks.get();
                hook_checks.set(current + 1);
                return Ok(if current < 2 { u64::MAX } else { 0 });
            }
            Ok(u64::MAX)
        });

        let error = store.run_search_projection_maintenance_slice().unwrap_err();
        assert!(matches!(error, StoreError::InsufficientDiskSpace { .. }));
        assert_eq!(checks.get(), 3);
        assert!(store.search_projection_maintenance_pending().unwrap());
        drop(probe);
        while store.run_search_projection_maintenance_slice().unwrap() {}
        assert!(store.search_projection_ready().unwrap());
    }

    #[test]
    fn absent_active_fts_generation_is_not_ready_and_schedules_bounded_repair() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&semantic_event(1)).unwrap();
        store.conn.execute_batch("DROP TABLE event_search").unwrap();
        assert!(!store.search_projection_ready().unwrap());
        drop(store);

        let repaired = Store::open(&db_path).unwrap();
        assert!(repaired.search_projection_maintenance_pending().unwrap());
        while repaired.run_search_projection_maintenance_slice().unwrap() {}
        assert!(repaired.search_projection_ready().unwrap());
        assert_eq!(
            repaired.search_event_hits("bootstrap", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn active_fts_corruption_returns_pending_and_schedules_bounded_repair() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&semantic_event(1)).unwrap();
        assert!(store.search_projection_ready().unwrap());

        let error = StoreError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("database disk image is malformed".to_owned()),
        ));
        assert!(matches!(
            store.handle_search_projection_query_error(error),
            StoreError::SearchProjectionMaintenancePending
        ));
        assert!(store.search_projection_maintenance_pending().unwrap());
        assert!(matches!(
            store.search_event_hits("bootstrap", 10),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));

        while store.run_search_projection_maintenance_slice().unwrap() {}
        assert!(store.search_projection_ready().unwrap());
        assert_eq!(store.search_event_hits("bootstrap", 10).unwrap().len(), 1);
    }

    #[test]
    fn missing_integrity_generation_never_certifies_existing_fts() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&semantic_event(1)).unwrap();
        store
            .conn
            .execute(
                "DELETE FROM search_projection_stats WHERE key = ?1",
                [SEARCH_PROJECTION_INTEGRITY_VERSION_KEY],
            )
            .unwrap();
        assert!(!store.search_projection_ready().unwrap());
        drop(store);

        let repaired = Store::open(&db_path).unwrap();
        assert!(repaired.search_projection_maintenance_pending().unwrap());
        while repaired.run_search_projection_maintenance_slice().unwrap() {}
        assert!(repaired.search_projection_ready().unwrap());
    }

    #[test]
    fn corrupt_rebuild_fts_integrity_restarts_bounded_repair_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        for seq in 0..SEARCH_PROJECTION_INIT_BATCH_ROWS {
            store.upsert_event(&semantic_event(seq as u64)).unwrap();
        }
        store.schedule_search_projection_refresh().unwrap();
        assert!(store.run_search_projection_maintenance_slice().unwrap());
        assert_eq!(
            search_projection_stat(&store.conn, SEARCH_PROJECTION_INIT_STAGE_KEY).unwrap(),
            Some(SEARCH_PROJECTION_INIT_EVENTS_STAGE)
        );
        set_search_projection_stat(
            &store.conn,
            SEARCH_PROJECTION_INIT_STAGE_KEY,
            SEARCH_PROJECTION_INIT_PUBLISH_STAGE,
        )
        .unwrap();
        store.conn.authorizer(Some(
            |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                AuthAction::Insert {
                    table_name: "event_search_rebuild",
                } => Authorization::Deny,
                _ => Authorization::Allow,
            },
        ));

        assert!(store.run_search_projection_maintenance_slice().unwrap());
        store
            .conn
            .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
        assert_eq!(
            search_projection_stat(&store.conn, SEARCH_PROJECTION_INIT_STAGE_KEY).unwrap(),
            Some(SEARCH_PROJECTION_INIT_CLEAN_STAGE)
        );
        assert!(!store.search_projection_ready().unwrap());
        while store.run_search_projection_maintenance_slice().unwrap() {}
        assert!(store.search_projection_ready().unwrap());
        assert_eq!(
            store.search_event_hits("bootstrap", 100).unwrap().len(),
            SEARCH_PROJECTION_INIT_BATCH_ROWS
        );
    }

    #[test]
    fn markerless_projection_is_rebuilt_bounded_instead_of_certified_on_open() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let event = semantic_event(1);
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&event).unwrap();
        let physical_rows = table_row_count(&store.conn, "event_search").unwrap();
        store
            .conn
            .execute(
                "DELETE FROM search_projection_stats WHERE key LIKE 'search_projection_init_v2:%'",
                [],
            )
            .unwrap();
        let read_only = Store::open_read_only(&db_path).unwrap();
        assert!(!read_only.search_projection_ready().unwrap());
        assert!(matches!(
            read_only.search_event_hits("semantic bootstrap item", 10),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        drop(read_only);
        drop(store);

        let reopened = Store::open(&db_path).unwrap();
        assert!(!reopened.search_projection_ready().unwrap());
        assert!(reopened.search_projection_maintenance_pending().unwrap());
        assert_eq!(
            table_row_count(&reopened.conn, "event_search").unwrap(),
            physical_rows
        );
        while reopened.run_search_projection_maintenance_slice().unwrap() {}
        assert!(reopened.search_projection_ready().unwrap());
        assert_eq!(
            reopened
                .search_event_hits("semantic bootstrap item", 10)
                .unwrap()
                .into_iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            vec![event.id]
        );
    }

    #[test]
    fn markerless_projection_with_wrong_content_or_membership_is_not_certified() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let event = semantic_event(1);
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&event).unwrap();
        store
            .conn
            .execute_batch(&format!(
                "DELETE FROM event_search WHERE event_id = '{id}';
                 INSERT INTO event_search
                    (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                 VALUES ('{id}', NULL, NULL, 'user', 'wrong projection text', 'message');
                 INSERT INTO event_search_scriptgram
                    (event_id, history_record_id, session_id, role, token_text, rank_bucket)
                 VALUES ('phantom', NULL, NULL, 'user', 'phantom', 'message');
                 DELETE FROM search_projection_stats
                 WHERE key LIKE 'search_projection_init_v2:%';",
                id = event.id
            ))
            .unwrap();

        let read_only = Store::open_read_only(&db_path).unwrap();
        assert!(!read_only.search_projection_ready().unwrap());
        assert!(matches!(
            read_only.search_event_hits("wrong", 10),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        drop(read_only);
        drop(store);

        let reopened = Store::open(&db_path).unwrap();
        assert!(reopened.search_projection_maintenance_pending().unwrap());
        while reopened.run_search_projection_maintenance_slice().unwrap() {}
        assert_eq!(
            reopened
                .search_event_hits("semantic bootstrap item", 10)
                .unwrap()
                .into_iter()
                .map(|hit| hit.event_id)
                .collect::<Vec<_>>(),
            vec![event.id]
        );
        assert!(reopened.search_event_hits("wrong", 10).unwrap().is_empty());
    }

    #[test]
    fn shadow_projection_survives_reopen_and_publishes_atomically_for_readers() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        for seq in 0..(SEARCH_PROJECTION_INIT_BATCH_ROWS + 6) {
            store.upsert_event(&semantic_event(seq as u64)).unwrap();
        }
        store
            .conn
            .execute_batch(
                "DELETE FROM event_search;
                 DELETE FROM event_search_scriptgram;
                 DELETE FROM event_search_lookup;
                 DELETE FROM search_projection_stats
                 WHERE key LIKE 'search_projection_init_v2:%';",
            )
            .unwrap();
        store.ensure_search_projection_initialized().unwrap();
        assert!(store.run_search_projection_maintenance_slice().unwrap());
        drop(store);

        let reopened = Store::open(&db_path).unwrap();
        assert!(reopened.search_projection_maintenance_pending().unwrap());
        let reader = Store::open_read_only(&db_path).unwrap();
        reader.conn.execute_batch("BEGIN DEFERRED").unwrap();
        assert_eq!(
            reader
                .conn
                .query_row("SELECT COUNT(*) FROM event_search", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );

        while reopened.run_search_projection_maintenance_slice().unwrap() {}
        assert_eq!(
            reader
                .conn
                .query_row("SELECT COUNT(*) FROM event_search", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "a reader snapshot must never observe a partially published generation"
        );
        reader.conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            reader
                .conn
                .query_row("SELECT COUNT(*) FROM event_search", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            (SEARCH_PROJECTION_INIT_BATCH_ROWS + 6) as i64
        );
    }

    #[test]
    fn published_projection_cleanup_resumes_after_reopen_without_blocking_search() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        for seq in 0..(SEARCH_PROJECTION_INIT_BATCH_ROWS + 6) {
            store.upsert_event(&semantic_event(seq as u64)).unwrap();
        }
        store.schedule_search_projection_refresh().unwrap();

        while search_projection_stat(&store.conn, SEARCH_PROJECTION_INIT_STAGE_KEY)
            .unwrap()
            .is_some()
        {
            store.run_search_projection_maintenance_slice().unwrap();
        }
        assert!(store.search_projection_ready().unwrap());
        assert!(store.search_projection_maintenance_pending().unwrap());
        assert!(SEARCH_PROJECTION_CLEAN_TABLES
            .iter()
            .any(|table| { table_row_count(&store.conn, table).unwrap() > 0 }));
        drop(store);

        let reopened = Store::open(&db_path).unwrap();
        assert!(reopened.search_projection_ready().unwrap());
        assert!(!reopened
            .search_event_hits("semantic bootstrap item", 10)
            .unwrap()
            .is_empty());
        while reopened.run_search_projection_maintenance_slice().unwrap() {}

        assert!(!reopened.search_projection_maintenance_pending().unwrap());
        assert!(SEARCH_PROJECTION_CLEAN_TABLES
            .iter()
            .all(|table| { table_row_count(&reopened.conn, table).unwrap() == 0 }));
    }

    #[test]
    fn semantic_hydration_returns_typed_pending_when_projection_is_rebuilding() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let event = semantic_event(1);
        store.upsert_event(&event).unwrap();
        store.schedule_search_projection_refresh().unwrap();
        let ranges = HashMap::from([(event.id, (0, usize::MAX))]);

        assert!(matches!(
            store.event_embedding_documents_by_ids(&[event.id]),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
        assert!(matches!(
            store.semantic_event_snapshot(&ranges),
            Err(StoreError::SearchProjectionMaintenancePending)
        ));
    }

    #[test]
    fn semantic_revision_ignores_noop_upserts_and_tracks_all_document_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let event = semantic_event(1);
        store.upsert_event(&event).unwrap();
        let after_insert = store.semantic_content_revision().unwrap();

        store.upsert_event(&event).unwrap();
        assert_eq!(store.semantic_content_revision().unwrap(), after_insert);

        let mut changed = event.clone();
        changed.payload = serde_json::json!({ "text": "changed semantic content" });
        store.upsert_event(&changed).unwrap();
        assert!(store.semantic_content_revision().unwrap() > after_insert);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'semantic_content_revision_runs_update'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn semantic_validation_and_hydration_share_one_reader_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let event = semantic_event(1);
        let store = Store::open(&db_path).unwrap();
        store.upsert_event(&event).unwrap();
        let updater = Store::open(&db_path).unwrap();
        let ranges = HashMap::from([(event.id, (0, usize::MAX))]);
        let mut updated = event.clone();
        updated.payload = serde_json::json!({ "text": "replacement semantic text" });

        let (documents, hits) = store
            .semantic_event_snapshot_after_documents(&ranges, || {
                updater.upsert_event(&updated).unwrap();
            })
            .unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(hits.len(), 1);
        assert!(documents[0].text.contains("semantic bootstrap item 1"));
        assert!(hits[0].preview.contains("semantic bootstrap item 1"));

        let (documents, hits) = store.semantic_event_snapshot(&ranges).unwrap();
        assert!(documents[0].text.contains("replacement semantic text"));
        assert!(hits[0].preview.contains("replacement semantic text"));
    }
}
