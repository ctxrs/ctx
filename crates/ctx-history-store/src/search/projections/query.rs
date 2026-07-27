use std::collections::HashMap;

use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use rusqlite::{params_from_iter, types::Value};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, parse_optional_text_enum,
    parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::schema::ddl::table_exists;
use crate::search::analyzer::{lexical_query_terms, scriptgram_match_clauses};
use crate::search::event_query::{
    event_search_hit_sql, event_search_score, lexical_event_search_query,
};
use crate::{Result, Store};

use super::eligibility::semantic_lite_turn_anchor_eligible_predicate;
use super::identity::{event_search_source_identity, EventSearchHit};
use super::semantic_document::{
    semantic_lite_turn_document_select_sql, semantic_lite_turn_source_chunk,
};
use super::storage::event_scriptgram_table_ready;

impl Store {
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
