use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_protocol::{
    search_analyzed_token_count, SearchClause, SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
};
use rusqlite::{params_from_iter, types::Value, ErrorCode};
use uuid::Uuid;

use crate::connection::parse_uuid;
use crate::provider_files::{
    event_material_visible_predicate, file_touched_material_visible_predicate,
};
use crate::schema::ddl::table_exists;
use crate::search::analyzer::{branch_needs_scriptgram, candidate_branch_match_query};
use crate::search::projections::event_scriptgram_table_ready;
use crate::{Result, Store};

pub const MAX_EVENT_CANDIDATES_PER_CLAUSE: usize = 1_024;

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchCandidate {
    pub event_id: Uuid,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventSearchCandidateBatch {
    pub candidates: Vec<EventSearchCandidate>,
    /// Raw ranked FTS rows consumed before publication and filter checks.
    pub examined: usize,
    /// The raw window reached its one-row exhaustion sentinel or timed out.
    pub truncated: bool,
    pub timed_out: bool,
}

/// Predicates that can be evaluated while SQLite is walking a bounded FTS
/// candidate window. Keeping this DTO in the store layer makes it impossible
/// for callers to accidentally collect an unbounded set of matching ids before
/// applying structured search filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventCandidateScope {
    pub session_id: Option<Uuid>,
    pub provider: Option<CaptureProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub workspace_contains: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub event_type: Option<EventType>,
    pub agent_scope: EventCandidateAgentScope,
    pub excluded_session: Option<EventCandidateExcludedSession>,
    pub file: Option<EventCandidateFileScope>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EventCandidateAgentScope {
    /// Include primary agents, subagents, and events without a session.
    Any,
    /// Match the default agent-history policy: explicit primary sessions plus
    /// events whose source predates agent classification.
    #[default]
    PrimaryOrUnclassified,
    /// Include only sessions explicitly marked primary.
    PrimaryOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCandidateExcludedSession {
    pub provider: CaptureProvider,
    pub provider_session_id: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCandidateFileScope {
    pub exact: String,
    pub suffix_like: String,
}

impl EventCandidateFileScope {
    pub fn new(file: &str) -> Option<Self> {
        let exact = file.trim();
        if exact.is_empty() {
            return None;
        }
        let suffix = exact.trim_start_matches(['/', '\\']);
        Some(Self {
            exact: exact.to_owned(),
            suffix_like: format!("%/{}", escape_like_pattern(suffix)),
        })
    }
}

impl EventCandidateScope {
    /// Normalize human-provided string selectors and make the explicit-session
    /// behavior match the public search API (a selected session is allowed even
    /// when it belongs to a subagent).
    pub fn normalized(mut self) -> Self {
        self.history_source = normalized_scope_text(self.history_source);
        self.provider_key = normalized_scope_text(self.provider_key);
        self.source_id = normalized_scope_text(self.source_id);
        self.source_format = normalized_scope_text(self.source_format);
        self.workspace_contains = normalized_scope_text(self.workspace_contains);
        if self.session_id.is_some() {
            self.agent_scope = EventCandidateAgentScope::Any;
        }
        if let Some(excluded) = self.excluded_session.as_mut() {
            excluded.provider_session_id = excluded.provider_session_id.trim().to_owned();
        }
        self
    }
}

fn normalized_scope_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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

fn bind_candidate_value(values: &mut Vec<Value>, value: Value) -> usize {
    values.push(value);
    values.len()
}

/// Build the scoped FTS statement and its positional values. This is kept as a
/// small pure function so the query plan can be regression-tested without
/// executing or hydrating any candidate payloads.
pub(crate) fn scoped_event_candidate_query(
    table: &str,
    match_query: String,
    sqlite_limit: i64,
    scope: &EventCandidateScope,
) -> (String, Vec<Value>) {
    debug_assert!(matches!(table, "event_search" | "event_search_scriptgram"));
    let mut values = vec![Value::Text(match_query)];
    let mut predicates = vec![event_material_visible_predicate("e")];

    let effective_session = "COALESCE(e.session_id, r.session_id)";
    let effective_record = "COALESCE(e.history_record_id, candidate.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id)";
    let effective_provider = "COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider)";
    let effective_external_session = "COALESCE(s.external_session_id, rs.external_session_id)";
    let effective_agent_type = "COALESCE(s.agent_type, rs.agent_type)";
    let effective_is_primary = "COALESCE(s.is_primary, rs.is_primary)";
    let effective_source_metadata = "COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json)";
    let plugin_name = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_plugin.plugin_name'), json_extract({effective_source_metadata}, '$.ctx_history_plugin.plugin_name'))"
    );
    let plugin_source_id = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_plugin.plugin_source_id'), json_extract({effective_source_metadata}, '$.ctx_history_plugin.plugin_source_id'))"
    );
    let history_source = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_plugin.history_source'), json_extract({effective_source_metadata}, '$.ctx_history_plugin.history_source'), CASE WHEN {plugin_name} IS NOT NULL AND {plugin_source_id} IS NOT NULL THEN {plugin_name} || '/' || {plugin_source_id} END)"
    );
    let provider_key = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_jsonl_v1.provider_key'), json_extract({effective_source_metadata}, '$.ctx_history_jsonl_v1.provider_key'))"
    );
    let source_id = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_jsonl_v1.source_id'), json_extract({effective_source_metadata}, '$.ctx_history_jsonl_v1.source_id'))"
    );
    let source_format = format!(
        "COALESCE(json_extract({effective_source_metadata}, '$.source_metadata.ctx_history_jsonl_v1.source_format'), json_extract({effective_source_metadata}, '$.ctx_history_jsonl_v1.source_format'), json_extract({effective_source_metadata}, '$.source_metadata.source_format'), json_extract({effective_source_metadata}, '$.source_format'))"
    );

    if let Some(session_id) = scope.session_id {
        let parameter = bind_candidate_value(&mut values, Value::Text(session_id.to_string()));
        predicates.push(format!("{effective_session} = ?{parameter}"));
    }
    if let Some(provider) = scope.provider {
        let parameter =
            bind_candidate_value(&mut values, Value::Text(provider.as_str().to_owned()));
        predicates.push(format!("{effective_provider} = ?{parameter}"));
    }
    if let Some(selector) = scope.history_source.as_ref() {
        let parameter = bind_candidate_value(&mut values, Value::Text(selector.clone()));
        predicates.push(format!(
            "({history_source} = ?{parameter} OR ({provider_key} || '/' || {source_id}) = ?{parameter})"
        ));
    }
    if let Some(value) = scope.provider_key.as_ref() {
        let parameter = bind_candidate_value(&mut values, Value::Text(value.clone()));
        predicates.push(format!("{provider_key} = ?{parameter}"));
    }
    if let Some(value) = scope.source_id.as_ref() {
        let parameter = bind_candidate_value(&mut values, Value::Text(value.clone()));
        predicates.push(format!("{source_id} = ?{parameter}"));
    }
    if let Some(value) = scope.source_format.as_ref() {
        let parameter = bind_candidate_value(&mut values, Value::Text(value.clone()));
        predicates.push(format!("{source_format} = ?{parameter}"));
    }
    if let Some(value) = scope.workspace_contains.as_ref() {
        let parameter = bind_candidate_value(&mut values, Value::Text(value.clone()));
        predicates.push(format!(
            "(instr(lower(COALESCE(event_source.cwd, session_source.cwd, run_source.cwd, '')), lower(?{parameter})) > 0 OR instr(lower(COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path, '')), lower(?{parameter})) > 0 OR instr(lower(COALESCE(wr.workspace, '')), lower(?{parameter})) > 0)"
        ));
    }
    if let Some(since) = scope.since {
        let parameter = bind_candidate_value(&mut values, Value::Integer(since.timestamp_millis()));
        predicates.push(format!("e.occurred_at_ms >= ?{parameter}"));
    }
    if let Some(event_type) = scope.event_type {
        let parameter =
            bind_candidate_value(&mut values, Value::Text(event_type.as_str().to_owned()));
        predicates.push(format!("e.event_type = ?{parameter}"));
    }
    match scope.agent_scope {
        EventCandidateAgentScope::Any => {}
        EventCandidateAgentScope::PrimaryOrUnclassified => predicates.push(format!(
            "({effective_is_primary} = 1 OR {effective_agent_type} = 'primary' OR ({effective_is_primary} IS NULL AND {effective_agent_type} IS NULL))"
        )),
        EventCandidateAgentScope::PrimaryOnly => predicates.push(format!(
            "({effective_is_primary} = 1 OR {effective_agent_type} = 'primary')"
        )),
    }
    if let Some(excluded) = scope.excluded_session.as_ref() {
        let provider_parameter = bind_candidate_value(
            &mut values,
            Value::Text(excluded.provider.as_str().to_owned()),
        );
        let external_parameter = bind_candidate_value(
            &mut values,
            Value::Text(excluded.provider_session_id.clone()),
        );
        let mut excluded_predicates = vec![format!(
            "({effective_provider} = ?{provider_parameter} AND {effective_external_session} = ?{external_parameter})"
        )];
        if let Some(session_id) = excluded.session_id {
            let session_parameter =
                bind_candidate_value(&mut values, Value::Text(session_id.to_string()));
            excluded_predicates.push(format!(
                "(s.id = ?{session_parameter} OR rs.id = ?{session_parameter} OR s.parent_session_id = ?{session_parameter} OR rs.parent_session_id = ?{session_parameter} OR s.root_session_id = ?{session_parameter} OR rs.root_session_id = ?{session_parameter})"
            ));
        }
        predicates.push(format!("NOT ({})", excluded_predicates.join(" OR ")));
    }
    if let Some(file) = scope.file.as_ref() {
        let exact_parameter = bind_candidate_value(&mut values, Value::Text(file.exact.clone()));
        let suffix_parameter =
            bind_candidate_value(&mut values, Value::Text(file.suffix_like.clone()));
        let path_matches = |alias: &str| {
            format!(
                "({alias}.path = ?{exact_parameter} OR {alias}.old_path = ?{exact_parameter} OR {alias}.path LIKE ?{suffix_parameter} ESCAPE '\\' OR {alias}.old_path LIKE ?{suffix_parameter} ESCAPE '\\')"
            )
        };
        let direct_path = path_matches("direct_ft");
        let event_path = path_matches("event_ft");
        let run_path = path_matches("run_ft");
        let source_path = path_matches("source_ft");
        let direct_visible = file_touched_material_visible_predicate("direct_ft");
        let event_visible = file_touched_material_visible_predicate("event_ft");
        let run_visible = file_touched_material_visible_predicate("run_ft");
        let source_visible = file_touched_material_visible_predicate("source_ft");
        predicates.push(format!(
            r#"(
                EXISTS (
                    SELECT 1 FROM files_touched AS direct_ft
                    WHERE {direct_path}
                      AND {direct_visible}
                      AND (direct_ft.event_id = e.id
                           OR direct_ft.run_id = e.run_id
                           OR direct_ft.history_record_id = {effective_record})
                )
                OR EXISTS (
                    SELECT 1 FROM files_touched AS event_ft
                    WHERE {event_path}
                      AND {event_visible}
                      AND event_ft.event_id IN (
                          SELECT file_event.id
                          FROM events AS file_event
                          LEFT JOIN sessions AS file_event_session ON file_event_session.id = file_event.session_id
                          WHERE file_event.session_id = {effective_session}
                             OR COALESCE(file_event.history_record_id, file_event_session.history_record_id) = {effective_record}
                      )
                )
                OR EXISTS (
                    SELECT 1 FROM files_touched AS run_ft
                    WHERE {run_path}
                      AND {run_visible}
                      AND run_ft.run_id IN (
                          SELECT file_run.id
                          FROM runs AS file_run
                          LEFT JOIN sessions AS file_run_session ON file_run_session.id = file_run.session_id
                          WHERE file_run.session_id = {effective_session}
                             OR COALESCE(file_run.history_record_id, file_run_session.history_record_id) = {effective_record}
                      )
                )
                OR EXISTS (
                    SELECT 1
                    FROM files_touched AS source_ft
                    JOIN sessions AS file_source_session ON file_source_session.capture_source_id = source_ft.source_id
                    WHERE {source_path}
                      AND {source_visible}
                      AND (file_source_session.id = {effective_session}
                           OR file_source_session.history_record_id = {effective_record})
                )
            )"#
        ));
    }

    let limit_parameter = bind_candidate_value(&mut values, Value::Integer(sqlite_limit));
    let record_visible = crate::provider_files::history_record_material_visible_predicate("wr");
    let sql = format!(
        r#"
        WITH raw_candidates AS MATERIALIZED (
            SELECT candidate.event_id, candidate.history_record_id, candidate.rank
            FROM {table} AS candidate
            WHERE {table} MATCH ?1
            ORDER BY candidate.rank
            LIMIT ?{limit_parameter}
        )
        SELECT candidate.event_id, candidate.rank, raw_window.examined
        FROM (SELECT COUNT(*) AS examined FROM raw_candidates) AS raw_window
        LEFT JOIN raw_candidates AS candidate
          ON EXISTS (
              SELECT 1
              FROM events AS e
              LEFT JOIN runs AS r ON r.id = e.run_id
              LEFT JOIN sessions AS s ON s.id = e.session_id
              LEFT JOIN sessions AS rs ON rs.id = r.session_id
              LEFT JOIN capture_sources AS event_source ON event_source.id = e.capture_source_id
              LEFT JOIN capture_sources AS session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
              LEFT JOIN capture_sources AS run_source ON run_source.id = r.source_id
              LEFT JOIN history_records AS wr ON wr.id = {effective_record} AND {record_visible}
              WHERE e.id = candidate.event_id
                AND {predicates}
          )
        ORDER BY candidate.rank, candidate.event_id
        "#,
        predicates = predicates.join("\n                AND ")
    );
    (sql, values)
}

impl Store {
    /// Retrieve a bounded, id-only candidate list for one structured clause.
    /// The FTS virtual table owns rank ordering, so SQLite can stop at the
    /// requested window without materializing all matching previews.
    pub fn search_event_candidates_for_clause(
        &self,
        clause: &SearchClause,
        limit: usize,
        timeout: Duration,
    ) -> Result<EventSearchCandidateBatch> {
        self.search_event_candidates_for_clause_scoped(
            clause,
            &EventCandidateScope {
                agent_scope: EventCandidateAgentScope::Any,
                ..EventCandidateScope::default()
            },
            limit,
            timeout,
        )
    }

    /// Retrieve a bounded, id-only candidate list after applying all
    /// representable structured predicates. The raw ranked FTS window is
    /// materialized before publication and filter verification, so selective
    /// predicates cannot cause corpus-sized candidate work.
    pub fn search_event_candidates_for_clause_scoped(
        &self,
        clause: &SearchClause,
        scope: &EventCandidateScope,
        limit: usize,
        timeout: Duration,
    ) -> Result<EventSearchCandidateBatch> {
        self.search_event_candidates_for_branch_scoped(clause, &[], &[], scope, limit, timeout)
    }

    /// Retrieve a bounded candidate branch after intersecting required lexical
    /// clauses and safely representable exclusions in FTS. This prevents a
    /// high-frequency seed from filling the candidate window with rows that
    /// residual `must`/`must_not` verification would immediately discard.
    pub fn search_event_candidates_for_branch_scoped(
        &self,
        seed: &SearchClause,
        required: &[SearchClause],
        excluded: &[SearchClause],
        scope: &EventCandidateScope,
        limit: usize,
        timeout: Duration,
    ) -> Result<EventSearchCandidateBatch> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if !table_exists(&self.conn, "event_search")? {
            return Ok(EventSearchCandidateBatch {
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
            return Ok(EventSearchCandidateBatch {
                candidates: Vec::new(),
                examined: 0,
                truncated: true,
                timed_out: false,
            });
        }

        let use_scriptgram =
            event_scriptgram_table_ready(&self.conn)? && branch_needs_scriptgram(seed, required);
        let table = if use_scriptgram {
            "event_search_scriptgram"
        } else {
            "event_search"
        };
        let Some(match_query) =
            candidate_branch_match_query(seed, required, excluded, use_scriptgram)
        else {
            return Ok(EventSearchCandidateBatch {
                candidates: Vec::new(),
                examined: 0,
                truncated: false,
                timed_out: false,
            });
        };
        let candidate_limit = limit.clamp(1, MAX_EVENT_CANDIDATES_PER_CLAUSE);
        let sqlite_limit = candidate_limit.saturating_add(1) as i64;
        let scope = scope.clone().normalized();
        let (sql, values) = scoped_event_candidate_query(table, match_query, sqlite_limit, &scope);
        let started = Instant::now();
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));

        let mut candidates = Vec::with_capacity(candidate_limit);
        let mut examined = 0usize;
        let query_result = (|| -> Result<()> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(values))?;
            while let Some(row) = rows.next()? {
                examined = usize::try_from(row.get::<_, i64>(2)?).unwrap_or(usize::MAX);
                let Some(event_id) = row.get::<_, Option<String>>(0)? else {
                    continue;
                };
                if candidates.len() >= candidate_limit {
                    continue;
                }
                candidates.push(EventSearchCandidate {
                    event_id: parse_uuid(event_id)?,
                    rank: row.get(1)?,
                });
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
        Ok(EventSearchCandidateBatch {
            truncated: examined > candidate_limit || timed_out,
            candidates,
            examined,
            timed_out,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use ctx_history_core::{
        Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
    };

    use super::*;

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn event(id: Uuid, seq: u64, event_type: EventType, text: &str) -> Event {
        Event {
            id,
            seq,
            history_record_id: None,
            session_id: None,
            run_id: None,
            event_type,
            role: Some(EventRole::User),
            occurred_at: fixed_time(),
            capture_source_id: None,
            payload: serde_json::json!({ "text": text }),
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

    fn store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("bounded-candidates.sqlite")).unwrap();
        (temp, store)
    }

    #[test]
    fn scoped_query_caps_materialized_ranked_rows_before_verification() {
        let scope = EventCandidateScope {
            event_type: Some(EventType::Message),
            ..EventCandidateScope::default()
        };
        let (sql, values) =
            scoped_event_candidate_query("event_search", "common".to_owned(), 5, &scope);

        let materialized = sql.find("raw_candidates AS MATERIALIZED").unwrap();
        let raw_limit = sql.find("LIMIT ?3").unwrap();
        let verification = sql.find("LEFT JOIN raw_candidates AS candidate").unwrap();
        assert!(materialized < raw_limit);
        assert!(raw_limit < verification);
        assert!(sql.contains("SELECT COUNT(*) AS examined FROM raw_candidates"));
        assert!(sql.contains("ORDER BY candidate.rank, candidate.event_id"));
        assert_eq!(values[2], Value::Integer(5));
    }

    #[test]
    fn common_token_selective_filter_accounts_for_the_raw_candidate_window() {
        let (_temp, store) = store();
        for seq in 1..=6 {
            let decoy = event(
                Uuid::new_v4(),
                seq,
                EventType::ToolOutput,
                "budgetcommon budgetcommon budgetcommon",
            );
            store.upsert_event(&decoy).unwrap();
        }
        let selected = event(
            Uuid::new_v4(),
            7,
            EventType::Message,
            &format!("budgetcommon {}", "unrelated ".repeat(100)),
        );
        store.upsert_event(&selected).unwrap();

        let batch = store
            .search_event_candidates_for_clause_scoped(
                &SearchClause::all("budgetcommon"),
                &EventCandidateScope {
                    event_type: Some(EventType::Message),
                    ..EventCandidateScope::default()
                },
                2,
                Duration::from_secs(5),
            )
            .unwrap();

        assert!(batch.candidates.is_empty());
        assert_eq!(batch.examined, 3);
        assert!(batch.truncated);
        assert!(!batch.timed_out);
    }

    #[test]
    fn common_token_ties_are_returned_in_event_id_order() {
        let (_temp, store) = store();
        let high_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000002").unwrap();
        let low_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000001").unwrap();
        store
            .upsert_event(&event(high_id, 1, EventType::Message, "stablecommon"))
            .unwrap();
        store
            .upsert_event(&event(low_id, 2, EventType::Message, "stablecommon"))
            .unwrap();

        let batch = store
            .search_event_candidates_for_clause(
                &SearchClause::all("stablecommon"),
                2,
                Duration::from_secs(5),
            )
            .unwrap();

        assert_eq!(batch.examined, 2);
        assert!(!batch.truncated);
        assert_eq!(
            batch
                .candidates
                .iter()
                .map(|candidate| candidate.event_id)
                .collect::<Vec<_>>(),
            vec![low_id, high_id]
        );
    }
}
