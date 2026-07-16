use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    utc_now, AgentType, CaptureProvider, Event, EventRole, EventType, HistoryRecord,
    RedactionState, SyncState, Visibility,
};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_uuid_string,
    parse_optional_text_enum, parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::provider_files::event_material_visible_predicate;
use crate::records::record_from_row;
use crate::schema::ddl::{
    create_event_search_lookup_table, ensure_search_projection_stats_table, table_exists,
};
use crate::schema::fts::create_fts_tables_if_supported;
use crate::search::analyzer::{
    lexical_query_terms, scriptgram_index_text, scriptgram_match_clauses,
};
use crate::search::event_query::{
    event_search_hit_sql, event_search_score, lexical_event_search_query,
};
use crate::{EventSearchBulkMaintenanceOutcome, Result, Store, StoreError};

const SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY: &str = "semantic_searchable_lite_turn_items_v3";
pub(crate) const SEARCH_PROJECTION_REBUILD_REQUIRED_STAT_KEY: &str =
    "search_projection_rebuild_required_v1";
const SEARCH_PROJECTION_REBUILD_CURSOR_STAT_KEY: &str = "search_projection_rebuild_cursor_v1";
const SEARCH_PROJECTION_REBUILD_PAGE_UNITS_STAT_KEY: &str =
    "search_projection_rebuild_page_units_v1";
const SEARCH_PROJECTION_READY_VERSION_STAT_KEY: &str = "search_projection_ready_version_v1";
const SEMANTIC_SEARCHABLE_ITEMS_BUILD_CURSOR_STAT_KEY: &str =
    "semantic_searchable_lite_turn_build_cursor_v1";
const SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY: &str =
    "semantic_searchable_lite_turn_build_count_v1";
const SEARCH_PROJECTION_READY_VERSION: i64 = 1;
const SEARCH_PROJECTION_REBUILD_SLICE_UNITS: usize = 64;
const SEARCH_PROJECTION_REBUILD_SLICE_BYTES: usize = 8 * 1024 * 1024;
const SEARCH_PROJECTION_REBUILD_SLICE_MAX_MILLIS: u64 = 25;
const SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES: usize = 256;
const SEARCH_PROJECTION_RECORD_TAGS_MAX_BYTES: usize = 64 * 1024;
const SEARCH_PROJECTION_EVENT_PAYLOAD_MAX_BYTES: usize = 256 * 1024;
const SEMANTIC_TURN_TEXT_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_LITE_TURN_RANK_BUCKET: &str = "lite_turn";

pub(crate) struct SearchProjectionReadGuard<'a> {
    conn: &'a Connection,
}

impl Drop for SearchProjectionReadGuard<'_> {
    fn drop(&mut self) {
        let _ = self
            .conn
            .execute_batch("RELEASE ctx_search_projection_read;");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
enum SearchProjectionRebuildPhase {
    DeleteRecordSearch = 1,
    DeleteRecordScriptgram = 2,
    DeleteEventSearch = 3,
    DeleteEventScriptgram = 4,
    DeleteEventLookup = 5,
    DeleteArtifactSearch = 6,
    PopulateRecords = 7,
    PopulateEvents = 8,
    PopulateSemanticCount = 9,
    FinalizeBulk = 10,
}

impl SearchProjectionRebuildPhase {
    fn from_value(value: i64) -> Result<Self> {
        match value {
            1 => Ok(Self::DeleteRecordSearch),
            2 => Ok(Self::DeleteRecordScriptgram),
            3 => Ok(Self::DeleteEventSearch),
            4 => Ok(Self::DeleteEventScriptgram),
            5 => Ok(Self::DeleteEventLookup),
            6 => Ok(Self::DeleteArtifactSearch),
            7 => Ok(Self::PopulateRecords),
            8 => Ok(Self::PopulateEvents),
            9 => Ok(Self::PopulateSemanticCount),
            10 => Ok(Self::FinalizeBulk),
            value => Err(StoreError::InvalidSearchProjectionRebuildPhase(value)),
        }
    }

    fn next(self) -> Self {
        match self {
            Self::DeleteRecordSearch => Self::DeleteRecordScriptgram,
            Self::DeleteRecordScriptgram => Self::DeleteEventSearch,
            Self::DeleteEventSearch => Self::DeleteEventScriptgram,
            Self::DeleteEventScriptgram => Self::DeleteEventLookup,
            Self::DeleteEventLookup => Self::DeleteArtifactSearch,
            Self::DeleteArtifactSearch => Self::PopulateRecords,
            Self::PopulateRecords => Self::PopulateEvents,
            Self::PopulateEvents => Self::PopulateSemanticCount,
            Self::PopulateSemanticCount | Self::FinalizeBulk => Self::FinalizeBulk,
        }
    }
}

#[derive(Debug)]
struct SearchProjectionRebuildBudget {
    units: usize,
    bytes: usize,
    max_units: usize,
    started: Instant,
}

impl Default for SearchProjectionRebuildBudget {
    fn default() -> Self {
        Self {
            units: 0,
            bytes: 0,
            max_units: SEARCH_PROJECTION_REBUILD_SLICE_UNITS,
            started: Instant::now(),
        }
    }
}

impl SearchProjectionRebuildBudget {
    fn with_max_units(max_units: usize) -> Self {
        Self {
            max_units: max_units.clamp(1, SEARCH_PROJECTION_REBUILD_SLICE_UNITS),
            ..Self::default()
        }
    }

    fn remaining_units(&self) -> usize {
        self.max_units.saturating_sub(self.units)
    }

    fn can_admit(&self, unit_bytes: usize) -> bool {
        !self.exhausted()
            && self.units < self.max_units
            && (self.units == 0
                || self.bytes.saturating_add(unit_bytes) <= SEARCH_PROJECTION_REBUILD_SLICE_BYTES)
    }

    fn record(&mut self, unit_bytes: usize) {
        self.units = self.units.saturating_add(1);
        self.bytes = self.bytes.saturating_add(unit_bytes);
    }

    fn exhausted(&self) -> bool {
        self.units >= self.max_units
            || self.bytes >= SEARCH_PROJECTION_REBUILD_SLICE_BYTES
            || self.started.elapsed()
                >= Duration::from_millis(SEARCH_PROJECTION_REBUILD_SLICE_MAX_MILLIS)
    }
}

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
    pub fn refresh_search_index(&self) -> Result<EventSearchBulkMaintenanceOutcome> {
        crate::connection::with_immediate_transaction(&self.conn, || {
            if !search_projection_rebuild_pending(&self.conn)? {
                mark_search_projection_rebuild_required(&self.conn)?;
            }
            Ok(())
        })?;
        self.advance_search_projection_rebuild()
    }

    pub(crate) fn rebuild_search_projection(&self) -> Result<EventSearchBulkMaintenanceOutcome> {
        self.refresh_search_index()
    }

    pub fn optimize_search_index(&self) -> Result<()> {
        self.merge_all_fts_tables_bounded()
    }

    pub fn event_search_projection_needs_backfill(&self) -> Result<bool> {
        Ok(search_projection_rebuild_pending(&self.conn)? || !search_projection_ready(&self.conn)?)
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if !table_exists(&self.conn, "event_search")? {
            return Ok(Vec::new());
        }
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
        semantic_searchable_item_count_exact(&self.conn)
    }

    pub fn cached_event_embedding_document_count(&self) -> Result<Option<usize>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        crate::connection::with_read_transaction(&self.conn, || {
            if self.has_pending_provider_file_publications()? {
                return Ok(None);
            }
            cached_semantic_searchable_item_count(&self.conn)
        })
    }

    pub fn event_embedding_document_count_cached_or_exact(&self) -> Result<usize> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        crate::connection::with_read_transaction(&self.conn, || {
            if self.has_pending_provider_file_publications()? {
                return Err(StoreError::SemanticSearchableItemCountPending);
            }
            cached_semantic_searchable_item_count(&self.conn)?
                .ok_or(StoreError::SemanticSearchableItemCountPending)
        })
    }

    pub fn refresh_event_embedding_document_count_cache(&self) -> Result<()> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        refresh_semantic_searchable_item_stats(&self.conn).map(|_| ())
    }

    pub fn recent_event_embedding_documents(
        &self,
        before: Option<(i64, u64)>,
        limit: usize,
    ) -> Result<Vec<EventEmbeddingDocument>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let next_user_predicate =
            semantic_lite_turn_user_eligible_predicate("next_user", "next_user_search");
        let assistant_visible = event_material_visible_predicate("candidate");
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
                          AND {assistant_visible}
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
        let _projection_snapshot = self.begin_readable_search_projection()?;
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

    pub(crate) fn ensure_search_projection_initialized(
        &self,
    ) -> Result<EventSearchBulkMaintenanceOutcome> {
        if !search_projection_rebuild_pending(&self.conn)? && !search_projection_ready(&self.conn)?
        {
            crate::connection::with_immediate_transaction(&self.conn, || {
                if !search_projection_rebuild_pending(&self.conn)?
                    && !search_projection_ready(&self.conn)?
                {
                    mark_search_projection_rebuild_required(&self.conn)?;
                }
                Ok(())
            })?;
        }
        if !search_projection_rebuild_pending(&self.conn)? {
            self.search_projection_recovery_deferred.set(false);
            return Ok(EventSearchBulkMaintenanceOutcome::Complete);
        }
        let outcome = self.advance_search_projection_rebuild()?;
        if outcome.is_complete() {
            self.search_projection_recovery_deferred.set(false);
        }
        Ok(outcome)
    }

    pub(crate) fn establish_empty_search_projection_ready(&self) -> Result<()> {
        crate::connection::with_immediate_transaction(&self.conn, || {
            clear_search_projection_rebuild_state(&self.conn)?;
            publish_search_projection_ready(&self.conn)
        })
    }

    pub(crate) fn begin_readable_search_projection(&self) -> Result<SearchProjectionReadGuard<'_>> {
        self.conn
            .execute_batch("SAVEPOINT ctx_search_projection_read;")?;
        if let Err(error) = self.ensure_search_projection_readable() {
            let _ = self.conn.execute_batch(
                "ROLLBACK TO ctx_search_projection_read; RELEASE ctx_search_projection_read;",
            );
            return Err(error);
        }
        Ok(SearchProjectionReadGuard { conn: &self.conn })
    }

    fn ensure_search_projection_readable(&self) -> Result<()> {
        if self.search_projection_recovery_deferred.get()
            || search_projection_rebuild_pending(&self.conn)?
            || !search_projection_ready(&self.conn)?
            || !search_projection_shape_compatible(&self.conn)?
        {
            return Err(StoreError::SearchProjectionRebuildPending);
        }
        Ok(())
    }

    fn advance_search_projection_rebuild(&self) -> Result<EventSearchBulkMaintenanceOutcome> {
        match self.advance_search_projection_rebuild_inner() {
            Err(error) if error.is_retryable_search_projection_recovery() => {
                Ok(EventSearchBulkMaintenanceOutcome::Pending)
            }
            result => result,
        }
    }

    fn advance_search_projection_rebuild_inner(&self) -> Result<EventSearchBulkMaintenanceOutcome> {
        let Some((phase, _)) = search_projection_rebuild_state(&self.conn)? else {
            return Ok(EventSearchBulkMaintenanceOutcome::Complete);
        };
        let guard = self.begin_event_search_bulk_mode()?;

        if phase == SearchProjectionRebuildPhase::FinalizeBulk {
            return self.finalize_search_projection_rebuild(&guard);
        }
        if self.event_search_bulk_admission_outcome()? == EventSearchBulkMaintenanceOutcome::Pending
        {
            let _ = self.finish_event_search_bulk_mode(&guard)?;
            return Ok(EventSearchBulkMaintenanceOutcome::Pending);
        }

        run_search_projection_rebuild_slice(self)?;
        let phase = search_projection_rebuild_state(&self.conn)?
            .map(|(phase, _)| phase)
            .unwrap_or(SearchProjectionRebuildPhase::FinalizeBulk);
        if phase == SearchProjectionRebuildPhase::FinalizeBulk {
            return self.finalize_search_projection_rebuild(&guard);
        }

        let _ = self.maintain_event_search_bulk_mode()?;
        Ok(EventSearchBulkMaintenanceOutcome::Pending)
    }

    fn finalize_search_projection_rebuild(
        &self,
        guard: &crate::EventSearchBulkGuard,
    ) -> Result<EventSearchBulkMaintenanceOutcome> {
        if self.finish_event_search_bulk_mode(guard)? == EventSearchBulkMaintenanceOutcome::Pending
        {
            return Ok(EventSearchBulkMaintenanceOutcome::Pending);
        }
        if self.event_search_bulk_maintenance_outcome()?
            == EventSearchBulkMaintenanceOutcome::Pending
        {
            return Ok(EventSearchBulkMaintenanceOutcome::Pending);
        }
        if cached_semantic_searchable_item_count(&self.conn)?.is_none() {
            crate::connection::with_immediate_transaction(&self.conn, || {
                set_search_projection_rebuild_state(
                    &self.conn,
                    SearchProjectionRebuildPhase::PopulateSemanticCount,
                    0,
                )
            })?;
            return Ok(EventSearchBulkMaintenanceOutcome::Pending);
        }
        crate::connection::with_immediate_transaction(&self.conn, || {
            if matches!(
                search_projection_rebuild_state(&self.conn)?,
                Some((SearchProjectionRebuildPhase::FinalizeBulk, _))
            ) {
                publish_search_projection_ready(&self.conn)?;
                clear_search_projection_rebuild_state(&self.conn)?;
            }
            Ok(())
        })?;
        Ok(if search_projection_rebuild_pending(&self.conn)? {
            EventSearchBulkMaintenanceOutcome::Pending
        } else {
            EventSearchBulkMaintenanceOutcome::Complete
        })
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

pub(crate) fn mark_search_projection_rebuild_required(conn: &Connection) -> Result<()> {
    ensure_search_projection_stats_table(conn)?;
    invalidate_search_projection_ready(conn)?;
    set_search_projection_rebuild_state(conn, SearchProjectionRebuildPhase::DeleteRecordSearch, 0)?;
    invalidate_semantic_searchable_item_stats(conn)
}

fn search_projection_ready(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(false);
    }
    let ready_version = conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            [SEARCH_PROJECTION_READY_VERSION_STAT_KEY],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(ready_version == Some(SEARCH_PROJECTION_READY_VERSION))
}

fn publish_search_projection_ready(conn: &Connection) -> Result<()> {
    if !search_projection_shape_compatible(conn)? {
        return Err(StoreError::SearchProjectionSchemaIncompatible(
            "required FTS or lookup tables are missing or malformed",
        ));
    }
    ensure_search_projection_stats_table(conn)?;
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            SEARCH_PROJECTION_READY_VERSION_STAT_KEY,
            SEARCH_PROJECTION_READY_VERSION,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(())
}

pub(crate) fn trust_existing_search_projection_if_not_rebuild_pending(
    conn: &Connection,
) -> Result<()> {
    ensure_search_projection_stats_table(conn)?;
    if !search_projection_rebuild_pending(conn)? {
        if search_projection_shape_compatible(conn)? {
            publish_search_projection_ready(conn)?;
        } else {
            mark_search_projection_rebuild_required(conn)?;
        }
    }
    Ok(())
}

fn invalidate_search_projection_ready(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1",
        [SEARCH_PROJECTION_READY_VERSION_STAT_KEY],
    )?;
    Ok(())
}

fn set_search_projection_rebuild_state(
    conn: &Connection,
    phase: SearchProjectionRebuildPhase,
    cursor: i64,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            SEARCH_PROJECTION_REBUILD_REQUIRED_STAT_KEY,
            phase as i64,
            now,
        ],
    )?;
    conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![SEARCH_PROJECTION_REBUILD_CURSOR_STAT_KEY, cursor, now],
    )?;
    Ok(())
}

fn clear_search_projection_rebuild_state(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key = ?2 OR key = ?3",
        params![
            SEARCH_PROJECTION_REBUILD_REQUIRED_STAT_KEY,
            SEARCH_PROJECTION_REBUILD_CURSOR_STAT_KEY,
            SEARCH_PROJECTION_REBUILD_PAGE_UNITS_STAT_KEY,
        ],
    )?;
    Ok(())
}

fn search_projection_rebuild_state(
    conn: &Connection,
) -> Result<Option<(SearchProjectionRebuildPhase, i64)>> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(None);
    }
    let phase = conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            [SEARCH_PROJECTION_REBUILD_REQUIRED_STAT_KEY],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(phase) = phase else {
        return Ok(None);
    };
    let cursor = conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            [SEARCH_PROJECTION_REBUILD_CURSOR_STAT_KEY],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(Some((
        SearchProjectionRebuildPhase::from_value(phase)?,
        cursor.max(0),
    )))
}

fn run_search_projection_rebuild_slice(store: &Store) -> Result<()> {
    let page_units = search_projection_rebuild_page_units(&store.conn)?;
    store.begin_immediate_batch()?;
    let progress_started = Instant::now();
    store.conn.progress_handler(
        1_000,
        Some(move || {
            progress_started.elapsed()
                >= Duration::from_millis(SEARCH_PROJECTION_REBUILD_SLICE_MAX_MILLIS)
        }),
    );
    let result = run_search_projection_rebuild_slice_inner(store, page_units);
    store.conn.progress_handler(0, None::<fn() -> bool>);
    match result {
        Ok(()) => {
            if page_units < SEARCH_PROJECTION_REBUILD_SLICE_UNITS {
                if let Err(error) = set_search_projection_stat_value(
                    &store.conn,
                    SEARCH_PROJECTION_REBUILD_PAGE_UNITS_STAT_KEY,
                    page_units
                        .saturating_mul(2)
                        .min(SEARCH_PROJECTION_REBUILD_SLICE_UNITS) as i64,
                ) {
                    let _ = store.rollback_batch();
                    return Err(error);
                }
            }
            match store.commit_batch() {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = store.rollback_batch();
                    Err(error)
                }
            }
        }
        Err(error) if search_projection_rebuild_interrupted(&error) => {
            let _ = store.rollback_batch();
            if page_units == 1 {
                return Err(StoreError::SearchProjectionRebuildTimedOut {
                    timeout_ms: SEARCH_PROJECTION_REBUILD_SLICE_MAX_MILLIS,
                });
            }
            crate::connection::with_immediate_transaction(&store.conn, || {
                set_search_projection_stat_value(
                    &store.conn,
                    SEARCH_PROJECTION_REBUILD_PAGE_UNITS_STAT_KEY,
                    page_units.saturating_div(2).max(1) as i64,
                )
            })
        }
        Err(error) => {
            let _ = store.rollback_batch();
            Err(error)
        }
    }
}

fn run_search_projection_rebuild_slice_inner(store: &Store, page_units: usize) -> Result<()> {
    let Some((mut phase, mut cursor)) = search_projection_rebuild_state(&store.conn)? else {
        return Ok(());
    };
    create_fts_tables_if_supported(&store.conn)?;
    if !table_exists(&store.conn, "event_search_lookup")? {
        create_event_search_lookup_table(&store.conn)?;
    }
    if event_search_lookup_table_malformed(&store.conn)? {
        return Err(StoreError::SearchProjectionSchemaIncompatible(
            "event_search_lookup has an incompatible shape",
        ));
    }
    if !search_projection_shape_compatible(&store.conn)? {
        return Err(StoreError::SearchProjectionSchemaIncompatible(
            "required FTS tables have an incompatible shape",
        ));
    }

    let mut budget = SearchProjectionRebuildBudget::with_max_units(page_units);
    while !budget.exhausted() && phase != SearchProjectionRebuildPhase::FinalizeBulk {
        let step = match phase {
            SearchProjectionRebuildPhase::DeleteRecordSearch => {
                delete_projection_rows(&store.conn, "ctx_history_search", cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::DeleteRecordScriptgram => delete_projection_rows(
                &store.conn,
                "ctx_history_search_scriptgram",
                cursor,
                &mut budget,
            )?,
            SearchProjectionRebuildPhase::DeleteEventSearch => {
                delete_projection_rows(&store.conn, "event_search", cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::DeleteEventScriptgram => {
                delete_projection_rows(&store.conn, "event_search_scriptgram", cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::DeleteEventLookup => {
                delete_projection_rows(&store.conn, "event_search_lookup", cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::DeleteArtifactSearch => {
                delete_projection_rows(&store.conn, "artifact_search", cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::PopulateRecords => {
                populate_record_projection_rows(store, cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::PopulateEvents => {
                populate_event_projection_rows(store, cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::PopulateSemanticCount => {
                populate_semantic_searchable_count(&store.conn, cursor, &mut budget)?
            }
            SearchProjectionRebuildPhase::FinalizeBulk => unreachable!(),
        };
        match step {
            SearchProjectionRebuildStep::Complete => {
                phase = phase.next();
                cursor = 0;
            }
            SearchProjectionRebuildStep::Pending(next_cursor) => {
                cursor = next_cursor;
                set_search_projection_rebuild_state(&store.conn, phase, cursor)?;
                return Ok(());
            }
        }
        set_search_projection_rebuild_state(&store.conn, phase, cursor)?;
    }
    Ok(())
}

fn search_projection_rebuild_page_units(conn: &Connection) -> Result<usize> {
    Ok(
        search_projection_stat_value(conn, SEARCH_PROJECTION_REBUILD_PAGE_UNITS_STAT_KEY)?
            .unwrap_or(SEARCH_PROJECTION_REBUILD_SLICE_UNITS as i64)
            .clamp(1, SEARCH_PROJECTION_REBUILD_SLICE_UNITS as i64) as usize,
    )
}

fn search_projection_rebuild_interrupted(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Sql(rusqlite::Error::SqliteFailure(sqlite_error, _))
            if sqlite_error.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchProjectionRebuildStep {
    Complete,
    Pending(i64),
}

fn delete_projection_rows(
    conn: &Connection,
    table: &'static str,
    cursor: i64,
    budget: &mut SearchProjectionRebuildBudget,
) -> Result<SearchProjectionRebuildStep> {
    if !table_exists(conn, table)? {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let limit = budget.remaining_units();
    if limit == 0 {
        return Ok(SearchProjectionRebuildStep::Pending(cursor));
    }
    let bytes = projection_row_bytes_expression(table);
    let sql =
        format!("SELECT rowid, {bytes} FROM {table} WHERE rowid > ?1 ORDER BY rowid LIMIT ?2");
    let rows = {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![cursor, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?.max(0) as usize))
        })?;
        collect_rows(rows)?
    };
    if rows.is_empty() {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let selected_rows = rows.len();
    let delete_sql = format!("DELETE FROM {table} WHERE rowid = ?1");
    let mut next_cursor = cursor;
    for (rowid, row_bytes) in rows {
        let unit_bytes = row_bytes.saturating_add(SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES);
        ensure_rebuild_unit_fits(unit_bytes)?;
        if !budget.can_admit(unit_bytes) {
            return Ok(SearchProjectionRebuildStep::Pending(next_cursor));
        }
        conn.execute(&delete_sql, params![rowid])?;
        budget.record(unit_bytes);
        next_cursor = rowid;
    }
    Ok(if selected_rows < limit && !budget.exhausted() {
        SearchProjectionRebuildStep::Complete
    } else {
        SearchProjectionRebuildStep::Pending(next_cursor)
    })
}

fn projection_row_bytes_expression(table: &'static str) -> &'static str {
    match table {
        "ctx_history_search" => "length(CAST(COALESCE(record_id, '') AS BLOB)) + length(CAST(COALESCE(title, '') AS BLOB)) + length(CAST(COALESCE(summary, '') AS BLOB)) + length(CAST(COALESCE(primary_user_text, '') AS BLOB)) + length(CAST(COALESCE(decision_text, '') AS BLOB)) + length(CAST(COALESCE(context_text, '') AS BLOB)) + length(CAST(COALESCE(tag_text, '') AS BLOB))",
        "ctx_history_search_scriptgram" => "length(CAST(COALESCE(record_id, '') AS BLOB)) + length(CAST(COALESCE(token_text, '') AS BLOB))",
        "event_search" => "length(CAST(COALESCE(event_id, '') AS BLOB)) + length(CAST(COALESCE(history_record_id, '') AS BLOB)) + length(CAST(COALESCE(session_id, '') AS BLOB)) + length(CAST(COALESCE(role, '') AS BLOB)) + length(CAST(COALESCE(preview_text, '') AS BLOB)) + length(CAST(COALESCE(rank_bucket, '') AS BLOB))",
        "event_search_scriptgram" => "length(CAST(COALESCE(event_id, '') AS BLOB)) + length(CAST(COALESCE(history_record_id, '') AS BLOB)) + length(CAST(COALESCE(session_id, '') AS BLOB)) + length(CAST(COALESCE(role, '') AS BLOB)) + length(CAST(COALESCE(token_text, '') AS BLOB)) + length(CAST(COALESCE(rank_bucket, '') AS BLOB))",
        "event_search_lookup" => "length(CAST(COALESCE(event_id, '') AS BLOB)) + length(CAST(COALESCE(history_record_id, '') AS BLOB)) + length(CAST(COALESCE(session_id, '') AS BLOB)) + length(CAST(COALESCE(role, '') AS BLOB)) + length(CAST(COALESCE(preview_text, '') AS BLOB)) + length(CAST(COALESCE(rank_bucket, '') AS BLOB))",
        "artifact_search" => "length(CAST(COALESCE(artifact_id, '') AS BLOB)) + length(CAST(COALESCE(history_record_id, '') AS BLOB)) + length(CAST(COALESCE(preview_text, '') AS BLOB))",
        _ => unreachable!("invalid projection table {table}"),
    }
}

fn ensure_rebuild_unit_fits(unit_bytes: usize) -> Result<()> {
    if unit_bytes > SEARCH_PROJECTION_REBUILD_SLICE_BYTES {
        return Err(StoreError::SearchProjectionRebuildUnitTooLarge {
            bytes: unit_bytes,
            max_bytes: SEARCH_PROJECTION_REBUILD_SLICE_BYTES,
        });
    }
    Ok(())
}

fn populate_record_projection_rows(
    store: &Store,
    cursor: i64,
    budget: &mut SearchProjectionRebuildBudget,
) -> Result<SearchProjectionRebuildStep> {
    if !table_exists(&store.conn, "ctx_history_search")? {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let limit = budget.remaining_units();
    if limit == 0 {
        return Ok(SearchProjectionRebuildStep::Pending(cursor));
    }
    let records = {
        let mut stmt = store.conn.prepare(
            r#"
            SELECT id,
                   substr(title, 1, 512),
                   substr(body, 1, 2048),
                   CASE
                       WHEN length(CAST(tags_json AS BLOB)) <= ?3 THEN tags_json
                       ELSE '[]'
                   END,
                   '',
                   NULL,
                   created_at,
                   updated_at,
                   rowid,
                   length(CAST(tags_json AS BLOB))
            FROM history_records
            WHERE rowid > ?1
            ORDER BY rowid
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                cursor,
                limit as i64,
                SEARCH_PROJECTION_RECORD_TAGS_MAX_BYTES as i64
            ],
            |row| {
                Ok((
                    record_from_row(row)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?.max(0) as usize,
                ))
            },
        )?;
        collect_rows(rows)?
    };
    if records.is_empty() {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let selected_rows = records.len();
    let has_record_scriptgram = record_scriptgram_table_ready(&store.conn)?;
    let mut next_cursor = cursor;
    for (record, rowid, tags_bytes) in records {
        if tags_bytes > SEARCH_PROJECTION_RECORD_TAGS_MAX_BYTES {
            return Err(StoreError::SearchProjectionRebuildUnitTooLarge {
                bytes: tags_bytes,
                max_bytes: SEARCH_PROJECTION_RECORD_TAGS_MAX_BYTES,
            });
        }
        let source_bytes = record
            .title
            .len()
            .saturating_add(record.body.len())
            .saturating_add(tags_bytes);
        let unit_bytes = record_search_projection_bytes(&record, has_record_scriptgram)
            .saturating_add(source_bytes)
            .saturating_add(SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES);
        ensure_rebuild_unit_fits(unit_bytes)?;
        if !budget.can_admit(unit_bytes) {
            return Ok(SearchProjectionRebuildStep::Pending(next_cursor));
        }
        replace_record_search_projection(&store.conn, &record, has_record_scriptgram)?;
        budget.record(unit_bytes);
        next_cursor = rowid;
    }
    Ok(if selected_rows < limit && !budget.exhausted() {
        SearchProjectionRebuildStep::Complete
    } else {
        SearchProjectionRebuildStep::Pending(next_cursor)
    })
}

fn record_search_projection_bytes(record: &HistoryRecord, has_scriptgram: bool) -> usize {
    let title = local_preview(&record.title, 512);
    let body = local_preview(&record.body, 2048);
    let tags = local_preview(&record.tags.join(" "), 1024);
    let mut bytes = record
        .id
        .to_string()
        .len()
        .saturating_add(title.len())
        .saturating_add(body.len().saturating_mul(2))
        .saturating_add(tags.len());
    if has_scriptgram {
        bytes = bytes
            .saturating_add(scriptgram_index_text(&record_search_scriptgram_source(record)).len());
    }
    bytes
}

#[allow(clippy::type_complexity)]
fn populate_event_projection_rows(
    store: &Store,
    cursor: i64,
    budget: &mut SearchProjectionRebuildBudget,
) -> Result<SearchProjectionRebuildStep> {
    if !table_exists(&store.conn, "events")? {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let max_rows_by_source_bytes = SEARCH_PROJECTION_REBUILD_SLICE_BYTES
        / (SEARCH_PROJECTION_EVENT_PAYLOAD_MAX_BYTES
            + SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES);
    let limit = budget
        .remaining_units()
        .min(max_rows_by_source_bytes.max(1));
    if limit == 0 {
        return Ok(SearchProjectionRebuildStep::Pending(cursor));
    }
    let rows = {
        let mut stmt = store.conn.prepare(
            r#"
            SELECT e.rowid,
                   e.id,
                   COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
                   e.session_id,
                   e.role,
                   e.event_type,
                   CASE
                       WHEN length(CAST(e.payload_json AS BLOB)) <= ?3 THEN e.payload_json
                       ELSE '{}'
                   END,
                   'safe_preview',
                   length(CAST(e.payload_json AS BLOB))
            FROM events e
            LEFT JOIN runs r ON r.id = e.run_id
            LEFT JOIN sessions s ON s.id = e.session_id
            LEFT JOIN sessions rs ON rs.id = r.session_id
            WHERE e.rowid > ?1
            ORDER BY e.rowid
            LIMIT ?2
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                cursor,
                limit as i64,
                SEARCH_PROJECTION_EVENT_PAYLOAD_MAX_BYTES as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?.max(0) as usize,
                ))
            },
        )?;
        collect_rows(rows)?
    };
    if rows.is_empty() {
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    let selected_rows = rows.len();
    let has_event_search = table_exists(&store.conn, "event_search")?;
    let has_event_lookup = event_search_lookup_table_ready(&store.conn)?;
    let has_event_scriptgram = event_scriptgram_table_ready(&store.conn)?;
    let mut next_cursor = cursor;
    for (
        rowid,
        event_id,
        history_record_id,
        session_id,
        role,
        event_type,
        payload_json,
        redaction_state,
        payload_bytes,
    ) in rows
    {
        if payload_bytes > SEARCH_PROJECTION_EVENT_PAYLOAD_MAX_BYTES {
            return Err(StoreError::SearchProjectionRebuildUnitTooLarge {
                bytes: payload_bytes,
                max_bytes: SEARCH_PROJECTION_EVENT_PAYLOAD_MAX_BYTES,
            });
        }
        let event_type = parse_text_enum::<EventType>(event_type)?;
        let role = parse_optional_text_enum::<EventRole>(role)?;
        let redaction_state = parse_text_enum::<RedactionState>(redaction_state)?;
        let preview = event_search_preview(event_type, role, &payload_json, redaction_state)?;
        let token_text = if has_event_scriptgram {
            scriptgram_index_text(&preview)
        } else {
            String::new()
        };
        let unit_bytes = event_search_projection_bytes(
            &event_id,
            history_record_id.as_deref(),
            session_id.as_deref(),
            role,
            event_type,
            &preview,
            &token_text,
            has_event_search,
            has_event_lookup,
            has_event_scriptgram,
        )
        .saturating_add(payload_bytes)
        .saturating_add(SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES);
        ensure_rebuild_unit_fits(unit_bytes)?;
        if !budget.can_admit(unit_bytes) {
            return Ok(SearchProjectionRebuildStep::Pending(next_cursor));
        }
        replace_stored_event_search_projection(
            &store.conn,
            &event_id,
            history_record_id.as_deref(),
            session_id.as_deref(),
            role,
            event_type,
            &preview,
            &token_text,
            has_event_search,
            has_event_lookup,
            has_event_scriptgram,
        )?;
        budget.record(unit_bytes);
        next_cursor = rowid;
    }
    Ok(if selected_rows < limit && !budget.exhausted() {
        SearchProjectionRebuildStep::Complete
    } else {
        SearchProjectionRebuildStep::Pending(next_cursor)
    })
}

#[allow(clippy::too_many_arguments)]
fn event_search_projection_bytes(
    event_id: &str,
    history_record_id: Option<&str>,
    session_id: Option<&str>,
    role: Option<EventRole>,
    event_type: EventType,
    preview: &str,
    token_text: &str,
    has_event_search: bool,
    has_event_lookup: bool,
    has_event_scriptgram: bool,
) -> usize {
    if preview.trim().is_empty() {
        return 0;
    }
    let common = event_id
        .len()
        .saturating_add(history_record_id.map(str::len).unwrap_or_default())
        .saturating_add(session_id.map(str::len).unwrap_or_default())
        .saturating_add(role.map(|value| value.as_str().len()).unwrap_or_default())
        .saturating_add(event_type.as_str().len());
    let mut bytes = 0usize;
    if has_event_search {
        bytes = bytes.saturating_add(common.saturating_add(preview.len()));
    }
    if has_event_scriptgram && !token_text.is_empty() {
        bytes = bytes.saturating_add(common.saturating_add(token_text.len()));
    }
    if has_event_lookup && semantic_lookup_event_parts(event_type, role.map(|value| value.as_str()))
    {
        bytes = bytes.saturating_add(common.saturating_add(preview.len()));
    }
    bytes
}

#[allow(clippy::too_many_arguments)]
fn replace_stored_event_search_projection(
    conn: &Connection,
    event_id: &str,
    history_record_id: Option<&str>,
    session_id: Option<&str>,
    role: Option<EventRole>,
    event_type: EventType,
    preview: &str,
    token_text: &str,
    has_event_search: bool,
    has_event_lookup: bool,
    has_event_scriptgram: bool,
) -> Result<()> {
    if has_event_search {
        conn.execute(
            "DELETE FROM event_search WHERE event_id = ?1",
            params![event_id],
        )?;
    }
    if has_event_scriptgram {
        conn.execute(
            "DELETE FROM event_search_scriptgram WHERE event_id = ?1",
            params![event_id],
        )?;
    }
    if has_event_lookup {
        conn.execute(
            "DELETE FROM event_search_lookup WHERE event_id = ?1",
            params![event_id],
        )?;
    }
    if preview.trim().is_empty() {
        return Ok(());
    }

    let role = role.map(|value| value.as_str());
    let rank_bucket = event_type.as_str();
    if has_event_search {
        conn.execute(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                event_id,
                history_record_id,
                session_id,
                role,
                preview,
                rank_bucket,
            ],
        )?;
    }
    if has_event_scriptgram && !token_text.is_empty() {
        conn.execute(
            r#"
            INSERT INTO event_search_scriptgram
            (event_id, history_record_id, session_id, role, token_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                event_id,
                history_record_id,
                session_id,
                role,
                token_text,
                rank_bucket,
            ],
        )?;
    }
    if has_event_lookup && semantic_lookup_event_parts(event_type, role) {
        conn.execute(
            r#"
            INSERT INTO event_search_lookup
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                event_id,
                history_record_id,
                session_id,
                role,
                preview,
                rank_bucket,
            ],
        )?;
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
    replace_record_search_projection(conn, record, record_scriptgram_table_ready(conn)?)
}

fn replace_record_search_projection(
    conn: &Connection,
    record: &HistoryRecord,
    has_record_scriptgram: bool,
) -> Result<()> {
    conn.execute(
        "DELETE FROM ctx_history_search WHERE record_id = ?1",
        params![record.id.to_string()],
    )?;
    if has_record_scriptgram {
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
    if has_record_scriptgram {
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
    fts_table_ready(
        conn,
        "ctx_history_search_scriptgram",
        "record_id UNINDEXED, token_text",
    )
}

pub(crate) fn event_scriptgram_table_ready(conn: &Connection) -> Result<bool> {
    fts_table_ready(
        conn,
        "event_search_scriptgram",
        r#"
        event_id UNINDEXED,
        history_record_id UNINDEXED,
        session_id UNINDEXED,
        role UNINDEXED,
        token_text,
        rank_bucket UNINDEXED
        "#,
    )
}

fn search_projection_shape_compatible(conn: &Connection) -> Result<bool> {
    Ok(fts_table_ready(
        conn,
        "ctx_history_search",
        r#"
        record_id UNINDEXED,
        title,
        summary,
        primary_user_text,
        decision_text,
        context_text,
        tag_text
        "#,
    )? && fts_table_ready(
        conn,
        "event_search",
        r#"
        event_id UNINDEXED,
        history_record_id UNINDEXED,
        session_id UNINDEXED,
        role UNINDEXED,
        preview_text,
        rank_bucket UNINDEXED
        "#,
    )? && fts_table_ready(
        conn,
        "artifact_search",
        "artifact_id UNINDEXED, history_record_id UNINDEXED, preview_text",
    )? && record_scriptgram_table_ready(conn)?
        && event_scriptgram_table_ready(conn)?
        && event_search_lookup_table_ready(conn)?)
}

fn fts_table_ready(conn: &Connection, table: &str, columns: &str) -> Result<bool> {
    let expected = format!("CREATE VIRTUAL TABLE {table} USING fts5({columns})");
    schema_sql_matches(conn, table, &expected)
}

fn schema_sql_matches(conn: &Connection, table: &str, expected: &str) -> Result<bool> {
    let actual = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .unwrap_or_default();
    Ok(normalize_schema_sql(&actual) == normalize_schema_sql(expected))
}

fn normalize_schema_sql(sql: &str) -> String {
    let mut tokens = Vec::new();
    let mut characters = sql.trim_end_matches(';').chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\'' {
            let mut literal = String::from(character);
            while let Some(literal_character) = characters.next() {
                literal.push(literal_character);
                if literal_character == '\'' {
                    if characters.peek() == Some(&'\'') {
                        literal.push(characters.next().unwrap_or('\''));
                    } else {
                        break;
                    }
                }
            }
            tokens.push(literal);
        } else if character.is_ascii_alphanumeric() || character == '_' {
            let mut word = String::from(character.to_ascii_lowercase());
            while characters
                .peek()
                .is_some_and(|next| next.is_ascii_alphanumeric() || *next == '_')
            {
                word.push(characters.next().unwrap_or_default().to_ascii_lowercase());
            }
            tokens.push(word);
        } else if !character.is_whitespace() {
            tokens.push(character.to_string());
        }
    }
    tokens.join("\u{1f}")
}

fn search_projection_rebuild_pending(conn: &Connection) -> Result<bool> {
    Ok(search_projection_rebuild_state(conn)?.is_some())
}

pub(crate) fn event_search_lookup_table_ready(conn: &Connection) -> Result<bool> {
    schema_sql_matches(
        conn,
        "event_search_lookup",
        r#"
        CREATE TABLE event_search_lookup (
            event_id TEXT PRIMARY KEY NOT NULL REFERENCES events(id) ON DELETE CASCADE,
            history_record_id TEXT REFERENCES history_records(id),
            session_id TEXT REFERENCES sessions(id),
            role TEXT CHECK (role IS NULL OR role IN ('user', 'assistant', 'system', 'tool', 'unknown')),
            preview_text TEXT NOT NULL,
            rank_bucket TEXT NOT NULL
        )
        "#,
    )
}

fn event_search_lookup_table_malformed(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "event_search_lookup")? && !event_search_lookup_table_ready(conn)?)
}

#[derive(Debug)]
struct SemanticSearchableCountPage {
    rows: Vec<(i64, bool)>,
    complete: bool,
}

fn semantic_searchable_count_page(
    conn: &Connection,
    cursor: i64,
    limit: usize,
) -> Result<SemanticSearchableCountPage> {
    if limit == 0 {
        return Ok(SemanticSearchableCountPage {
            rows: Vec::new(),
            complete: false,
        });
    }
    let predicate = semantic_lite_turn_anchor_eligible_predicate();
    let sql = format!(
        r#"
        SELECT anchor.rowid,
               CASE WHEN {predicate} THEN 1 ELSE 0 END
        FROM events AS anchor
        LEFT JOIN event_search_lookup AS anchor_search
          ON anchor_search.event_id = anchor.id
        WHERE anchor.rowid > ?1
        ORDER BY anchor.rowid
        LIMIT ?2
        "#
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![cursor, limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0))
    })?;
    let rows = collect_rows(rows)?;
    Ok(SemanticSearchableCountPage {
        complete: rows.len() < limit,
        rows,
    })
}

fn populate_semantic_searchable_count(
    conn: &Connection,
    cursor: i64,
    budget: &mut SearchProjectionRebuildBudget,
) -> Result<SearchProjectionRebuildStep> {
    let limit = budget.remaining_units();
    if limit == 0 {
        return Ok(SearchProjectionRebuildStep::Pending(cursor));
    }
    let page = semantic_searchable_count_page(conn, cursor, limit)?;
    let mut next_cursor = cursor;
    let mut added = 0_i64;
    for (rowid, eligible) in &page.rows {
        if budget.exhausted() {
            break;
        }
        budget.record(SEARCH_PROJECTION_REBUILD_ROW_OVERHEAD_BYTES);
        next_cursor = *rowid;
        added += i64::from(*eligible);
    }
    let accumulated =
        search_projection_stat_value(conn, SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY)?
            .unwrap_or(0)
            .max(0)
            .saturating_add(added);
    set_search_projection_stat_value(
        conn,
        SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY,
        accumulated,
    )?;
    let processed_all = next_cursor == page.rows.last().map(|(rowid, _)| *rowid).unwrap_or(cursor);
    if processed_all && page.complete {
        publish_semantic_searchable_item_count(conn, accumulated)?;
        return Ok(SearchProjectionRebuildStep::Complete);
    }
    Ok(SearchProjectionRebuildStep::Pending(next_cursor))
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
    let count = conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            params![SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(count.map(|value| value.max(0) as usize))
}

fn search_projection_stat_value(conn: &Connection, key: &str) -> Result<Option<i64>> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            [key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?)
}

fn set_search_projection_stat_value(conn: &Connection, key: &str, value: i64) -> Result<()> {
    ensure_search_projection_stats_table(conn)?;
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

fn publish_semantic_searchable_item_count(conn: &Connection, count: i64) -> Result<()> {
    set_search_projection_stat_value(conn, SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY, count.max(0))?;
    conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key = ?2",
        params![
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_CURSOR_STAT_KEY,
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY,
        ],
    )?;
    Ok(())
}

pub(crate) fn refresh_semantic_searchable_item_stats(conn: &Connection) -> Result<usize> {
    crate::connection::with_immediate_transaction(conn, || {
        refresh_semantic_searchable_item_stats_inner(conn, || {})
    })
}

#[cfg(test)]
pub(crate) fn refresh_semantic_searchable_item_stats_with_hook(
    conn: &Connection,
    after_count: impl FnOnce(),
) -> Result<usize> {
    crate::connection::with_immediate_transaction(conn, || {
        refresh_semantic_searchable_item_stats_inner(conn, after_count)
    })
}

fn refresh_semantic_searchable_item_stats_inner(
    conn: &Connection,
    after_count: impl FnOnce(),
) -> Result<usize> {
    ensure_search_projection_stats_table(conn)?;
    if crate::provider_files::has_fenced_provider_file_publications(conn)? {
        invalidate_semantic_searchable_item_stats(conn)?;
        after_count();
        return Ok(0);
    }
    if let Some(count) = cached_semantic_searchable_item_count(conn)? {
        after_count();
        return Ok(count);
    }
    if !event_search_lookup_table_ready(conn)? {
        after_count();
        return Err(StoreError::SemanticSearchableItemCountPending);
    }
    let cursor =
        search_projection_stat_value(conn, SEMANTIC_SEARCHABLE_ITEMS_BUILD_CURSOR_STAT_KEY)?
            .unwrap_or(0)
            .max(0);
    let count = search_projection_stat_value(conn, SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY)?
        .unwrap_or(0)
        .max(0);
    let page = semantic_searchable_count_page(conn, cursor, SEARCH_PROJECTION_REBUILD_SLICE_UNITS)?;
    let next_count =
        count.saturating_add(page.rows.iter().filter(|(_, eligible)| *eligible).count() as i64);
    let next_cursor = page.rows.last().map(|(rowid, _)| *rowid).unwrap_or(cursor);
    after_count();
    if crate::provider_files::has_fenced_provider_file_publications(conn)? {
        invalidate_semantic_searchable_item_stats(conn)?;
        return Ok(next_count.max(0) as usize);
    }
    if page.complete {
        publish_semantic_searchable_item_count(conn, next_count)?;
    } else {
        set_search_projection_stat_value(
            conn,
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_CURSOR_STAT_KEY,
            next_cursor,
        )?;
        set_search_projection_stat_value(
            conn,
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY,
            next_count,
        )?;
    }
    Ok(next_count.max(0) as usize)
}

pub(crate) fn adjust_semantic_searchable_item_stats(
    conn: &Connection,
    previous_count: usize,
    current_count: usize,
) -> Result<()> {
    crate::connection::with_immediate_transaction(conn, || {
        adjust_semantic_searchable_item_stats_inner(conn, previous_count, current_count)
    })
}

fn adjust_semantic_searchable_item_stats_inner(
    conn: &Connection,
    previous_count: usize,
    current_count: usize,
) -> Result<()> {
    if search_projection_rebuild_pending(conn)? {
        return mark_search_projection_rebuild_required(conn);
    }
    if crate::provider_files::has_fenced_provider_file_publications(conn)? {
        return invalidate_semantic_searchable_item_stats(conn);
    }
    if previous_count == current_count {
        return Ok(());
    }
    if !table_exists(conn, "search_projection_stats")?
        || cached_semantic_searchable_item_count(conn)?.is_none()
    {
        return invalidate_semantic_searchable_item_stats(conn);
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

pub(crate) fn decrement_semantic_searchable_item_stats_if_cached(
    conn: &Connection,
    removed_count: usize,
) -> Result<()> {
    crate::connection::with_immediate_transaction(conn, || {
        decrement_semantic_searchable_item_stats_if_cached_inner(conn, removed_count)
    })
}

fn decrement_semantic_searchable_item_stats_if_cached_inner(
    conn: &Connection,
    removed_count: usize,
) -> Result<()> {
    if search_projection_rebuild_pending(conn)? {
        return mark_search_projection_rebuild_required(conn);
    }
    if crate::provider_files::has_fenced_provider_file_publications(conn)? {
        return invalidate_semantic_searchable_item_stats(conn);
    }
    if removed_count == 0 {
        return Ok(());
    }
    if cached_semantic_searchable_item_count(conn)?.is_none() {
        return invalidate_semantic_searchable_item_stats(conn);
    }
    conn.execute(
        r#"
        UPDATE search_projection_stats
        SET value = MAX(value - ?2, 0),
            updated_at_ms = ?3
        WHERE key = ?1
        "#,
        params![
            SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
            removed_count as i64,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(())
}

pub(crate) fn invalidate_semantic_searchable_item_stats(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "search_projection_stats")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key = ?2 OR key = ?3",
        params![
            SEMANTIC_SEARCHABLE_ITEMS_STAT_KEY,
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_CURSOR_STAT_KEY,
            SEMANTIC_SEARCHABLE_ITEMS_BUILD_COUNT_STAT_KEY,
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
    AND {}
    "#,
        event_material_visible_predicate(event_alias)
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
    let candidate_visible = event_material_visible_predicate("candidate");
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
                      AND {candidate_visible}
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
                      AND {candidate_visible}
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
            .get("text_retention")
            .and_then(|retention| retention.get("omission_policy"))
            .and_then(serde_json::Value::as_str)
            == Some("patch_or_diff")
        // Provider capture envelope v1 compatibility. V2 emits
        // `text_retention.omission_policy` instead.
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
