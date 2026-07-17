use std::time::{Duration, Instant};

use ctx_history_core::{EntityTimestamps, HistoryRecord, HistoryRecordLink};
use ctx_protocol::{
    search_analyzed_token_count, SearchClause, SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
};
use rusqlite::{params, params_from_iter, types::Value, ErrorCode, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_timestamp_ms, optional_uuid_string, parse_optional_uuid,
    parse_text_enum, parse_time, parse_uuid, timestamp_ms,
};
use crate::schema::ddl::table_exists;
use crate::search::analyzer::{
    branch_needs_scriptgram, candidate_branch_match_query, lexical_query_terms,
    scriptgram_match_clauses, scriptgram_match_query,
};
use crate::search::projections::{
    event_scriptgram_table_ready, fts_match_clauses, fts_match_query,
    record_scriptgram_table_ready, upsert_record_search_projection,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

pub const MAX_RECORD_CANDIDATES_PER_CLAUSE: usize = 1_024;
const RECORD_SEARCH_TITLE_MAX_CHARS: usize = 512;
const RECORD_SEARCH_BODY_MAX_CHARS: usize = 2_048;
const RECORD_SEARCH_TAG_TEXT_MAX_CHARS: usize = 1_024;
const RECORD_SEARCH_KIND_MAX_CHARS: usize = 128;
const RECORD_SEARCH_WORKSPACE_MAX_CHARS: usize = 4_096;
const RECORD_SEARCH_DOCUMENT_MAX_BYTES: usize = (RECORD_SEARCH_TITLE_MAX_CHARS
    + RECORD_SEARCH_BODY_MAX_CHARS
    + RECORD_SEARCH_TAG_TEXT_MAX_CHARS
    + RECORD_SEARCH_KIND_MAX_CHARS
    + RECORD_SEARCH_WORKSPACE_MAX_CHARS)
    * 4;

#[derive(Debug, Clone, PartialEq)]
pub struct RecordSearchCandidate {
    pub record_id: Uuid,
    pub rank: f64,
    pub updated_at_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordSearchCandidateBatch {
    pub candidates: Vec<RecordSearchCandidate>,
    pub examined: usize,
    pub truncated: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSearchDocument {
    pub record_id: Uuid,
    pub title: String,
    pub body: String,
    pub tag_text: String,
    pub kind: String,
    pub workspace: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub is_complete: bool,
}

impl RecordSearchDocument {
    pub fn into_history_record(self) -> Result<HistoryRecord> {
        let tags = serde_json::from_str::<Vec<String>>(&self.tag_text)?;
        Ok(HistoryRecord {
            id: self.record_id,
            title: self.title,
            body: self.body,
            tags,
            kind: self.kind,
            workspace: self.workspace,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

impl Store {
    pub fn get_record_search_document(
        &self,
        record_id: Uuid,
    ) -> Result<Option<RecordSearchDocument>> {
        self.get_record_search_document_bounded(
            record_id,
            RECORD_SEARCH_DOCUMENT_MAX_BYTES,
            Duration::from_secs(10),
        )
    }

    pub fn get_record_search_document_bounded(
        &self,
        record_id: Uuid,
        maximum_bytes: usize,
        timeout: Duration,
    ) -> Result<Option<RecordSearchDocument>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        run_bounded_record_lookup(&self.conn, timeout, || {
            let visible =
                crate::provider_files::history_record_material_visible_predicate("records");
            let complete = record_search_document_complete_sql();
            let maximum_bytes = maximum_bytes.min(i64::MAX as usize) as i64;
            let summary = self
                .conn
                .query_row(
                    &format!(
                        r#"
                        SELECT records.id,
                               records.created_at_ms,
                               records.updated_at_ms,
                               {complete}
                        FROM history_records AS records
                        WHERE records.id = ?1 AND {visible}
                        LIMIT 1
                        "#,
                    ),
                    params![
                        record_id.to_string(),
                        RECORD_SEARCH_TITLE_MAX_CHARS as i64,
                        RECORD_SEARCH_BODY_MAX_CHARS as i64,
                        RECORD_SEARCH_TAG_TEXT_MAX_CHARS as i64,
                        RECORD_SEARCH_KIND_MAX_CHARS as i64,
                        RECORD_SEARCH_WORKSPACE_MAX_CHARS as i64,
                        maximum_bytes,
                    ],
                    |row| {
                        Ok((
                            parse_uuid(row.get::<_, String>(0)?)?,
                            ms_to_time(row.get(1)?)?,
                            ms_to_time(row.get(2)?)?,
                            row.get::<_, i64>(3)? != 0,
                        ))
                    },
                )
                .optional()?;
            let Some((record_id, created_at, updated_at, is_complete)) = summary else {
                return Ok(None);
            };
            if !is_complete {
                return Ok(Some(RecordSearchDocument {
                    record_id,
                    title: String::new(),
                    body: String::new(),
                    tag_text: String::new(),
                    kind: String::new(),
                    workspace: None,
                    created_at,
                    updated_at,
                    is_complete: false,
                }));
            }

            let document = self
                .conn
                .query_row(
                    &format!(
                        r#"
                        SELECT records.id,
                               COALESCE(records.title, ''),
                               COALESCE(records.body, ''),
                               COALESCE(records.tags_json, '[]'),
                               COALESCE(records.kind, ''),
                               records.workspace,
                               records.created_at_ms,
                               records.updated_at_ms
                        FROM history_records AS records
                        WHERE records.id = ?1 AND {visible} AND {complete}
                        LIMIT 1
                        "#,
                    ),
                    params![
                        record_id.to_string(),
                        RECORD_SEARCH_TITLE_MAX_CHARS as i64,
                        RECORD_SEARCH_BODY_MAX_CHARS as i64,
                        RECORD_SEARCH_TAG_TEXT_MAX_CHARS as i64,
                        RECORD_SEARCH_KIND_MAX_CHARS as i64,
                        RECORD_SEARCH_WORKSPACE_MAX_CHARS as i64,
                        maximum_bytes,
                    ],
                    |row| {
                        Ok(RecordSearchDocument {
                            record_id: parse_uuid(row.get::<_, String>(0)?)?,
                            title: row.get(1)?,
                            body: row.get(2)?,
                            tag_text: row.get(3)?,
                            kind: row.get(4)?,
                            workspace: row.get(5)?,
                            created_at: ms_to_time(row.get(6)?)?,
                            updated_at: ms_to_time(row.get(7)?)?,
                            is_complete: true,
                        })
                    },
                )
                .optional()?;
            Ok(document.or_else(|| {
                Some(RecordSearchDocument {
                    record_id,
                    title: String::new(),
                    body: String::new(),
                    tag_text: String::new(),
                    kind: String::new(),
                    workspace: None,
                    created_at,
                    updated_at,
                    is_complete: false,
                })
            }))
        })
    }

    pub fn search_record_candidates_for_clause(
        &self,
        clause: &SearchClause,
        limit: usize,
        timeout: Duration,
    ) -> Result<RecordSearchCandidateBatch> {
        self.search_record_candidates_for_branch(clause, &[], &[], limit, timeout)
    }

    pub fn search_record_candidates_for_branch(
        &self,
        seed: &SearchClause,
        required: &[SearchClause],
        excluded: &[SearchClause],
        limit: usize,
        timeout: Duration,
    ) -> Result<RecordSearchCandidateBatch> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if !table_exists(&self.conn, "ctx_history_search")? {
            return Ok(RecordSearchCandidateBatch {
                candidates: Vec::new(),
                examined: 0,
                truncated: false,
                timed_out: false,
            });
        }
        if std::iter::once(seed)
            .chain(required)
            .chain(excluded)
            .any(|clause| {
                search_analyzed_token_count(clause.value()) > SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE
            })
        {
            return Ok(RecordSearchCandidateBatch {
                candidates: Vec::new(),
                examined: 0,
                truncated: true,
                timed_out: false,
            });
        }
        let use_scriptgram =
            record_scriptgram_table_ready(&self.conn)? && branch_needs_scriptgram(seed, required);
        let table = if use_scriptgram {
            "ctx_history_search_scriptgram"
        } else {
            "ctx_history_search"
        };
        let Some(match_query) =
            candidate_branch_match_query(seed, required, excluded, use_scriptgram)
        else {
            return Ok(RecordSearchCandidateBatch {
                candidates: Vec::new(),
                examined: 0,
                truncated: false,
                timed_out: false,
            });
        };
        let candidate_limit = limit.clamp(1, MAX_RECORD_CANDIDATES_PER_CLAUSE);
        let visible = crate::provider_files::history_record_material_visible_predicate("record");
        let sql = format!(
            r#"
            SELECT candidate.record_id, candidate.rank
            FROM {table} AS candidate
            WHERE {table} MATCH ?1
              AND EXISTS (
                  SELECT 1 FROM history_records AS record
                  WHERE record.id = candidate.record_id AND {visible}
              )
            ORDER BY candidate.rank
            LIMIT ?2
            "#,
        );
        let started = Instant::now();
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));
        let mut candidates = Vec::with_capacity(candidate_limit);
        let mut examined = 0usize;
        let query_result = (|| -> Result<()> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params![
                match_query,
                candidate_limit.saturating_add(1) as i64
            ])?;
            while let Some(row) = rows.next()? {
                examined = examined.saturating_add(1);
                if candidates.len() < candidate_limit {
                    candidates.push(RecordSearchCandidate {
                        record_id: parse_uuid(row.get::<_, String>(0)?)?,
                        rank: row.get(1)?,
                        updated_at_ms: 0,
                        created_at_ms: 0,
                    });
                }
            }
            let mut timestamps = self.conn.prepare(
                "SELECT updated_at_ms, created_at_ms FROM history_records WHERE id = ?1",
            )?;
            for candidate in &mut candidates {
                if let Some((updated_at_ms, created_at_ms)) = timestamps
                    .query_row(params![candidate.record_id.to_string()], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?
                {
                    candidate.updated_at_ms = updated_at_ms;
                    candidate.created_at_ms = created_at_ms;
                }
            }
            Ok(())
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        let mut timed_out = false;
        match query_result {
            Ok(()) => {}
            Err(StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == ErrorCode::OperationInterrupted
                    && started.elapsed() >= timeout =>
            {
                timed_out = true;
            }
            Err(error) => return Err(error),
        }
        Ok(RecordSearchCandidateBatch {
            truncated: examined > candidate_limit || timed_out,
            candidates,
            examined,
            timed_out,
        })
    }

    pub fn upsert_history_record_link(&self, link: &HistoryRecordLink) -> Result<Uuid> {
        self.with_provider_file_publication_write(|| self.upsert_history_record_link_inner(link))
    }

    fn upsert_history_record_link_inner(&self, link: &HistoryRecordLink) -> Result<Uuid> {
        let conflict_id = self
            .conn
            .query_row(
                "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                params![
                    link.history_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str(),
                ],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .optional()?;
        self.ensure_provider_file_history_record_link_write_allowed(
            conflict_id.unwrap_or(link.id),
            link,
        )?;
        self.conn.execute(
                r#"
                INSERT INTO history_record_links
                (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
                    confidence = excluded.confidence,
                    source_id = excluded.source_id,
                    updated_at_ms = excluded.updated_at_ms,
                    visibility = excluded.visibility,
                    fidelity = excluded.fidelity,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    deleted_at_ms = excluded.deleted_at_ms,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    link.id.to_string(),
                    link.history_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str(),
                    link.confidence.as_str(),
                    optional_uuid_string(link.source_id),
                    timestamp_ms(link.timestamps.created_at),
                    timestamp_ms(link.timestamps.updated_at),
                    link.sync.visibility.as_str(),
                    link.sync.fidelity.as_str(),
                    link.sync.sync_state.as_str(),
                    link.sync.sync_version as i64,
                    optional_timestamp_ms(link.sync.deleted_at),
                    serde_json::to_string(&link.sync.metadata)?,
                ],
            )?;
        let id = self.conn
                .query_row(
                    "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                    params![
                        link.history_record_id.to_string(),
                        link.target_type.as_str(),
                        link.target_id.to_string(),
                        link.link_type.as_str()
                    ],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .map_err(StoreError::from)?;
        self.track_provider_file_publication_direct_entity(
            "history_record_link",
            "history_record_links",
            id,
        )?;
        Ok(id)
    }

    pub(crate) fn list_history_record_links(&self) -> Result<Vec<HistoryRecordLink>> {
        let visible = crate::provider_files::history_record_link_material_visible_predicate(
            "history_record_links",
        );
        let mut stmt = self.conn.prepare(
            history_record_link_select_sql(&format!("WHERE {visible} ORDER BY updated_at_ms, id"))
                .as_str(),
        )?;
        let rows = stmt.query_map([], history_record_link_from_row)?;
        collect_rows(rows)
    }

    pub fn insert_record(&self, record: &HistoryRecord) -> Result<()> {
        self.with_provider_file_publication_write(|| self.insert_record_inner(record))
    }

    fn insert_record_inner(&self, record: &HistoryRecord) -> Result<()> {
        self.ensure_provider_file_history_record_write_allowed(record.id)?;
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
                INSERT INTO history_records
                (
                    id, title, summary, status, started_at_ms, last_activity_at_ms,
                    created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        self.track_provider_file_publication_history_record(record.id)?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &HistoryRecord) -> Result<()> {
        self.with_provider_file_publication_write(|| self.upsert_record_inner(record))
    }

    fn upsert_record_inner(&self, record: &HistoryRecord) -> Result<()> {
        self.ensure_provider_file_history_record_write_allowed(record.id)?;
        self.upsert_record_row(record)?;
        self.track_provider_file_publication_history_record(record.id)?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn delete_orphan_record(&self, record_id: Uuid) -> Result<bool> {
        self.with_provider_file_publication_write(|| self.delete_orphan_record_inner(record_id))
    }

    fn delete_orphan_record_inner(&self, record_id: Uuid) -> Result<bool> {
        self.ensure_provider_file_history_record_write_allowed(record_id)?;
        self.delete_orphan_record_row(record_id)
    }

    pub(crate) fn delete_orphan_record_row(&self, record_id: Uuid) -> Result<bool> {
        let record_id = record_id.to_string();
        let deleted = self.conn.execute(
            r#"
            DELETE FROM history_records
            WHERE id = ?1
              AND NOT EXISTS (SELECT 1 FROM sessions WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM runs WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM events WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM history_record_links WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM summaries WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM files_touched WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM history_record_tags WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM record_edges WHERE from_record_id = ?1 OR to_record_id = ?1)
            "#,
            params![&record_id],
        )?;
        if deleted > 0 && table_exists(&self.conn, "ctx_history_search")? {
            self.conn.execute(
                "DELETE FROM ctx_history_search WHERE record_id = ?1",
                params![&record_id],
            )?;
        }
        Ok(deleted > 0)
    }

    pub fn upsert_records(&self, records: &[HistoryRecord]) -> Result<()> {
        self.with_provider_file_publication_write(|| self.upsert_records_inner(records))
    }

    fn upsert_records_inner(&self, records: &[HistoryRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        for record in records {
            self.ensure_provider_file_history_record_write_allowed(record.id)?;
        }
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.begin_immediate_batch()?;
        }
        for record in records {
            if let Err(err) = self
                .upsert_record_row(record)
                .and_then(|()| self.track_provider_file_publication_history_record(record.id))
            {
                if owns_transaction {
                    let _ = self.rollback_batch();
                }
                return Err(err);
            }
        }
        for record in records {
            upsert_record_search_projection(&self.conn, record)?;
        }
        if owns_transaction {
            if let Err(err) = self.commit_batch() {
                let _ = self.rollback_batch();
                return Err(err);
            }
        }
        Ok(())
    }

    fn upsert_record_row(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
                INSERT INTO history_records
                (
                    id, title, summary, status, started_at_ms, last_activity_at_ms,
                    created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    summary = excluded.summary,
                    status = excluded.status,
                    started_at_ms = excluded.started_at_ms,
                    last_activity_at_ms = excluded.last_activity_at_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    body = excluded.body,
                    tags_json = excluded.tags_json,
                    kind = excluded.kind,
                    workspace = excluded.workspace,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at
                "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_record(&self, id: Uuid) -> Result<HistoryRecord> {
        let visible = self.importer_history_record_material_visible_predicate("history_records");
        self.conn
            .query_row(
                record_select_sql(&format!("WHERE id = ?1 AND {visible}")).as_str(),
                params![id.to_string()],
                record_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_records(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.list_records_page(limit, 0)
    }

    pub fn list_records_page(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        let visible = self.importer_history_record_material_visible_predicate("history_records");
        let mut stmt = self.conn.prepare(
            record_select_sql(&format!(
                "WHERE {visible} ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2"
            ))
            .as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.search_records_page(query, limit, 0)
    }

    pub fn search_records_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if fts_match_query(query).is_none() && scriptgram_match_query(query).is_none() {
            return Ok(Vec::new());
        }
        if let Some(records) = self.search_records_fts(query, limit, offset)? {
            return Ok(records);
        }
        let terms = lexical_query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut values = terms
            .iter()
            .map(|term| Value::Text(format!("%{term}%")))
            .collect::<Vec<_>>();
        let predicates = (1..=terms.len())
            .map(|index| {
                format!("title LIKE ?{index} OR body LIKE ?{index} OR tags_json LIKE ?{index}")
            })
            .collect::<Vec<_>>();
        let coverage = predicates
            .iter()
            .map(|predicate| format!("CASE WHEN {predicate} THEN 1 ELSE 0 END"))
            .collect::<Vec<_>>()
            .join(" + ");
        values.push(Value::Integer(limit as i64));
        let limit_parameter = values.len();
        values.push(Value::Integer(offset as i64));
        let offset_parameter = values.len();
        let visible = self.importer_history_record_material_visible_predicate("history_records");
        let tail = format!(
            "WHERE ({}) AND {visible} ORDER BY ({coverage}) DESC, created_at DESC, id LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}",
            predicates.join(") OR (")
        );
        let mut stmt = self.conn.prepare(&record_select_sql(&tail))?;
        let rows = stmt.query_map(params_from_iter(values), record_from_row)?;
        collect_rows(rows)
    }

    fn search_records_fts(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<Vec<HistoryRecord>>> {
        if !table_exists(&self.conn, "ctx_history_search")? {
            return Ok(None);
        }
        let match_clauses = fts_match_clauses(query);
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_artifact_search = table_exists(&self.conn, "artifact_search")?;
        let has_record_scriptgram = record_scriptgram_table_ready(&self.conn)?;
        let has_event_scriptgram = event_scriptgram_table_ready(&self.conn)?;
        let record_visible = self.importer_history_record_material_visible_predicate("record");
        let event_visible = crate::provider_files::event_material_visible_predicate("event");
        let artifact_visible = crate::provider_files::direct_source_material_visible_predicate(
            "artifact",
            "source_id",
        );
        let scriptgram_clauses = if has_record_scriptgram || has_event_scriptgram {
            scriptgram_match_clauses(query)
        } else {
            Vec::new()
        };
        if match_clauses.is_empty() && scriptgram_clauses.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let mut selects = Vec::new();
        let mut values = Vec::<Value>::new();
        for (term_index, clause) in match_clauses.into_iter().enumerate() {
            values.push(Value::Text(clause));
            let parameter = values.len();
            selects.push(format!(
                "SELECT search.record_id, {term_index}, bm25(ctx_history_search) FROM ctx_history_search AS search JOIN history_records AS record ON record.id = search.record_id WHERE ctx_history_search MATCH ?{parameter} AND {record_visible}"
            ));
            if has_event_search && has_artifact_search {
                selects.push(format!(
                    "SELECT search.history_record_id, {term_index}, bm25(event_search) FROM event_search AS search JOIN events AS event ON event.id = search.event_id WHERE event_search MATCH ?{parameter} AND search.history_record_id IS NOT NULL AND {event_visible}"
                ));
                selects.push(format!(
                    "SELECT search.history_record_id, {term_index}, bm25(artifact_search) FROM artifact_search AS search JOIN artifacts AS artifact ON artifact.id = search.artifact_id WHERE artifact_search MATCH ?{parameter} AND search.history_record_id IS NOT NULL AND {artifact_visible}"
                ));
            }
        }
        for (term_index, clause) in scriptgram_clauses {
            values.push(Value::Text(clause));
            let parameter = values.len();
            if has_record_scriptgram {
                selects.push(format!(
                    "SELECT search.record_id, {term_index}, bm25(ctx_history_search_scriptgram) + 0.35 FROM ctx_history_search_scriptgram AS search JOIN history_records AS record ON record.id = search.record_id WHERE ctx_history_search_scriptgram MATCH ?{parameter} AND {record_visible}"
                ));
            }
            if has_event_scriptgram {
                selects.push(format!(
                    "SELECT search.history_record_id, {term_index}, bm25(event_search_scriptgram) + 0.35 FROM event_search_scriptgram AS search JOIN events AS event ON event.id = search.event_id WHERE event_search_scriptgram MATCH ?{parameter} AND search.history_record_id IS NOT NULL AND {event_visible}"
                ));
            }
        }
        values.push(Value::Integer(limit as i64));
        let limit_parameter = values.len();
        values.push(Value::Integer(offset as i64));
        let offset_parameter = values.len();
        let sql = format!(
            r#"
            WITH matches(record_id, term_index, score) AS MATERIALIZED (
                {}
            ),
            term_matches(record_id, term_index, score) AS (
                SELECT record_id, term_index, MIN(score)
                FROM matches
                WHERE record_id IS NOT NULL
                GROUP BY record_id, term_index
            )
            SELECT record_id
            FROM term_matches
            GROUP BY record_id
            ORDER BY COUNT(*) DESC, SUM(score), record_id
            LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}
            "#,
            selects.join(" UNION ALL ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(self.get_record(parse_uuid(row?)?)?);
        }
        Ok(Some(records))
    }
}

fn record_search_document_complete_sql() -> String {
    "length(COALESCE(records.title, '')) <= ?2 \
         AND length(COALESCE(records.body, '')) <= ?3 \
         AND length(COALESCE(records.tags_json, '')) <= ?4 \
         AND length(COALESCE(records.kind, '')) <= ?5 \
         AND (records.workspace IS NULL OR length(records.workspace) <= ?6) \
         AND length(CAST(COALESCE(records.title, '') AS BLOB)) \
             + length(CAST(COALESCE(records.body, '') AS BLOB)) \
             + length(CAST(COALESCE(records.tags_json, '') AS BLOB)) \
             + length(CAST(COALESCE(records.kind, '') AS BLOB)) \
             + length(CAST(COALESCE(records.workspace, '') AS BLOB)) <= ?7"
        .to_owned()
}

fn run_bounded_record_lookup<T>(
    conn: &rusqlite::Connection,
    timeout: Duration,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let timeout = timeout.max(Duration::from_millis(1));
    let started = Instant::now();
    let progress_started = started;
    conn.progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));
    let result = operation();
    conn.progress_handler(0, None::<fn() -> bool>);
    match result {
        Err(StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
            if error.code == ErrorCode::OperationInterrupted && started.elapsed() >= timeout =>
        {
            Err(StoreError::BoundedSearchTimedOut {
                timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            })
        }
        result => result,
    }
}

pub(crate) fn history_record_link_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM history_record_links {tail}"
    )
}

pub(crate) fn history_record_link_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<HistoryRecordLink> {
    Ok(HistoryRecordLink {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_uuid(row.get::<_, String>(1)?)?,
        target_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkTargetType>(
            row.get::<_, String>(2)?,
        )?,
        target_id: parse_uuid(row.get::<_, String>(3)?)?,
        link_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkType>(
            row.get::<_, String>(4)?,
        )?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(5)?)?,
        source_id: parse_optional_uuid(row.get(6)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        sync: sync_metadata_from_row(row, 9, 10, 11, 12, 13, 14)?,
    })
}

pub(crate) fn record_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, title, body, tags_json, kind, workspace, created_at, updated_at FROM history_records {tail}"
    )
}

pub(crate) fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecord> {
    let tags_json: String = row.get(3)?;
    Ok(HistoryRecord {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        title: row.get(1)?,
        body: row.get(2)?,
        tags: serde_json::from_str(&tags_json)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        kind: row.get(4)?,
        workspace: row.get(5)?,
        created_at: parse_time(row.get::<_, String>(6)?)?,
        updated_at: parse_time(row.get::<_, String>(7)?)?,
    })
}
