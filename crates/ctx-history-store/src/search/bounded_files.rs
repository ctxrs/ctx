use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use ctx_history_core::CaptureProvider;
use rusqlite::{params, ErrorCode};
use uuid::Uuid;

use crate::connection::{parse_optional_uuid, parse_uuid};
use crate::search::bounded_candidates::normalized_scope_text;
use crate::{Result, Store};

pub const MAX_FILE_SEARCH_CANDIDATES: usize = 1_024;
const MAX_FILE_SEARCH_ROWS: usize = 4_096;

/// Source predicates that SQLite can apply before selecting the representative
/// touch for a file-only result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSearchCandidateScope {
    pub provider: Option<CaptureProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
}

impl FileSearchCandidateScope {
    fn normalized(mut self) -> Self {
        self.history_source = normalized_scope_text(self.history_source);
        self.provider_key = normalized_scope_text(self.provider_key);
        self.source_id = normalized_scope_text(self.source_id);
        self.source_format = normalized_scope_text(self.source_format);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSearchCandidateBatch {
    pub record_ids: Vec<Uuid>,
    pub representative_touch_ids: BTreeMap<Uuid, Uuid>,
    pub representative_source_ids: BTreeMap<Uuid, Uuid>,
    pub examined: usize,
    pub truncated: bool,
    pub timed_out: bool,
}

impl Store {
    /// Return a bounded record-ID list for file-only search without building a
    /// global in-memory scope of every matching touch.
    pub fn search_file_record_candidates(
        &self,
        file: &str,
        limit: usize,
        timeout: Duration,
    ) -> Result<FileSearchCandidateBatch> {
        self.search_file_record_candidates_scoped(
            file,
            &FileSearchCandidateScope::default(),
            limit,
            timeout,
        )
    }

    /// Return a bounded record-ID list after applying source predicates to
    /// matching touches. Source ancestry is resolved through the touch, its
    /// event/run, and their sessions before the representative touch and row
    /// limits are selected.
    pub fn search_file_record_candidates_scoped(
        &self,
        file: &str,
        scope: &FileSearchCandidateScope,
        limit: usize,
        timeout: Duration,
    ) -> Result<FileSearchCandidateBatch> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(FileSearchCandidateBatch::default());
        };
        let scope = scope.clone().normalized();
        let candidate_limit = limit.clamp(1, MAX_FILE_SEARCH_CANDIDATES);
        let row_limit = candidate_limit
            .saturating_mul(4)
            .min(MAX_FILE_SEARCH_ROWS)
            .saturating_add(1);
        let started = Instant::now();
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));
        let mut record_ids = Vec::with_capacity(candidate_limit);
        let mut representative_touch_ids = BTreeMap::new();
        let mut representative_source_ids = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut examined = 0usize;
        let query_result = (|| -> Result<()> {
            let touch_visible =
                crate::provider_files::file_touched_material_visible_predicate("ft");
            let record_visible =
                crate::provider_files::history_record_material_visible_predicate("record");
            let mut stmt = self.conn.prepare(&format!(
                r#"
                WITH touch_candidates AS (
                    SELECT COALESCE(
                               ft.history_record_id,
                               e.history_record_id,
                               direct_run.history_record_id,
                               event_run.history_record_id,
                               event_session.history_record_id,
                               direct_run_session.history_record_id,
                               event_run_session.history_record_id,
                               source_session.history_record_id
                           ) AS record_id,
                           ft.id AS touch_id,
                           COALESCE(
                               ft.source_id,
                               e.capture_source_id,
                               direct_run.source_id,
                               event_run.source_id,
                               event_session.capture_source_id,
                               direct_run_session.capture_source_id,
                               event_run_session.capture_source_id
                           ) AS effective_source_id,
                           ft.updated_at_ms
                    FROM files_touched AS ft
                    LEFT JOIN events AS e ON e.id = ft.event_id
                    LEFT JOIN runs AS direct_run ON direct_run.id = ft.run_id
                    LEFT JOIN runs AS event_run ON event_run.id = e.run_id
                    LEFT JOIN sessions AS event_session ON event_session.id = e.session_id
                    LEFT JOIN sessions AS direct_run_session
                        ON direct_run_session.id = direct_run.session_id
                    LEFT JOIN sessions AS event_run_session
                        ON event_run_session.id = event_run.session_id
                    LEFT JOIN sessions AS source_session
                        ON source_session.capture_source_id = ft.source_id
                    WHERE (ft.path = ?1
                           OR ft.old_path = ?1
                           OR ft.path LIKE ?2 ESCAPE '\'
                           OR ft.old_path LIKE ?2 ESCAPE '\')
                      AND {touch_visible}
                ), scoped_touches AS (
                    SELECT candidate.record_id,
                           candidate.touch_id,
                           candidate.effective_source_id,
                           candidate.updated_at_ms
                    FROM touch_candidates AS candidate
                    LEFT JOIN capture_sources AS source
                        ON source.id = candidate.effective_source_id
                    WHERE candidate.record_id IS NOT NULL
                      AND EXISTS (
                          SELECT 1 FROM history_records AS record
                          WHERE record.id = candidate.record_id AND {record_visible}
                      )
                      AND (?3 IS NULL OR source.provider = ?3)
                      AND (
                          ?4 IS NULL
                          OR COALESCE(
                              json_extract(source.metadata_json, '$.source_metadata.ctx_history_plugin.history_source'),
                              json_extract(source.metadata_json, '$.ctx_history_plugin.history_source'),
                              CASE
                                  WHEN COALESCE(
                                      json_extract(source.metadata_json, '$.source_metadata.ctx_history_plugin.plugin_name'),
                                      json_extract(source.metadata_json, '$.ctx_history_plugin.plugin_name')
                                  ) IS NOT NULL
                                  AND COALESCE(
                                      json_extract(source.metadata_json, '$.source_metadata.ctx_history_plugin.plugin_source_id'),
                                      json_extract(source.metadata_json, '$.ctx_history_plugin.plugin_source_id')
                                  ) IS NOT NULL
                                  THEN COALESCE(
                                      json_extract(source.metadata_json, '$.source_metadata.ctx_history_plugin.plugin_name'),
                                      json_extract(source.metadata_json, '$.ctx_history_plugin.plugin_name')
                                  ) || '/' || COALESCE(
                                      json_extract(source.metadata_json, '$.source_metadata.ctx_history_plugin.plugin_source_id'),
                                      json_extract(source.metadata_json, '$.ctx_history_plugin.plugin_source_id')
                                  )
                              END
                          ) = ?4
                          OR COALESCE(
                              json_extract(source.metadata_json, '$.source_metadata.ctx_history_jsonl_v1.provider_key'),
                              json_extract(source.metadata_json, '$.ctx_history_jsonl_v1.provider_key')
                          ) || '/' || COALESCE(
                              json_extract(source.metadata_json, '$.source_metadata.ctx_history_jsonl_v1.source_id'),
                              json_extract(source.metadata_json, '$.ctx_history_jsonl_v1.source_id')
                          ) = ?4
                      )
                      AND (?5 IS NULL OR COALESCE(
                          json_extract(source.metadata_json, '$.source_metadata.ctx_history_jsonl_v1.provider_key'),
                          json_extract(source.metadata_json, '$.ctx_history_jsonl_v1.provider_key')
                      ) = ?5)
                      AND (?6 IS NULL OR COALESCE(
                          json_extract(source.metadata_json, '$.source_metadata.ctx_history_jsonl_v1.source_id'),
                          json_extract(source.metadata_json, '$.ctx_history_jsonl_v1.source_id')
                      ) = ?6)
                      AND (?7 IS NULL OR COALESCE(
                          json_extract(source.metadata_json, '$.source_metadata.ctx_history_jsonl_v1.source_format'),
                          json_extract(source.metadata_json, '$.ctx_history_jsonl_v1.source_format'),
                          json_extract(source.metadata_json, '$.source_metadata.source_format'),
                          json_extract(source.metadata_json, '$.source_format')
                      ) = ?7)
                    ORDER BY candidate.updated_at_ms DESC, candidate.touch_id DESC
                    LIMIT ?8
                )
                SELECT record_id, touch_id, effective_source_id
                FROM scoped_touches
                ORDER BY updated_at_ms DESC, touch_id DESC
                "#,
            ))?;
            let mut rows = stmt.query(params![
                exact,
                suffix,
                scope.provider.map(|provider| provider.as_str().to_owned()),
                scope.history_source,
                scope.provider_key,
                scope.source_id,
                scope.source_format,
                row_limit as i64,
            ])?;
            while let Some(row) = rows.next()? {
                examined = examined.saturating_add(1);
                let record_id = parse_uuid(row.get::<_, String>(0)?)?;
                if seen.insert(record_id) && record_ids.len() < candidate_limit {
                    record_ids.push(record_id);
                    representative_touch_ids
                        .insert(record_id, parse_uuid(row.get::<_, String>(1)?)?);
                    if let Some(source_id) = parse_optional_uuid(row.get::<_, Option<String>>(2)?)?
                    {
                        representative_source_ids.insert(record_id, source_id);
                    }
                }
            }
            Ok(())
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        let mut timed_out = false;
        match query_result {
            Ok(()) => {}
            Err(crate::StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == ErrorCode::OperationInterrupted
                    && started.elapsed() >= timeout =>
            {
                timed_out = true;
            }
            Err(error) => return Err(error),
        }
        Ok(FileSearchCandidateBatch {
            truncated: timed_out || examined >= row_limit || seen.len() > candidate_limit,
            record_ids,
            representative_touch_ids,
            representative_source_ids,
            examined,
            timed_out,
        })
    }
}

fn file_touch_match_values(file: &str) -> Option<(String, String)> {
    let exact = file.trim();
    if exact.is_empty() {
        return None;
    }
    let suffix = exact.trim_start_matches(['/', '\\']);
    Some((
        exact.to_owned(),
        format!("%/{}", escape_like_pattern(suffix)),
    ))
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}
