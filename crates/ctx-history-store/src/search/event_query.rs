use rusqlite::types::Value;

use crate::provider_files::event_material_visible_predicate;

pub(super) fn lexical_event_search_query(
    match_clauses: Vec<String>,
    limit: usize,
    offset: usize,
    prefer_conversation: bool,
) -> (String, Vec<Value>) {
    let mut values = Vec::<Value>::new();
    let selects = match_clauses
        .into_iter()
        .enumerate()
        .map(|(term_index, clause)| {
            values.push(Value::Text(clause));
            format!(
                r#"SELECT event_search.event_id,
                          event_search.history_record_id,
                          event_search.session_id,
                          event_search.role,
                          event_search.preview_text,
                          {term_index},
                          bm25(event_search)
                   FROM event_search
                   WHERE event_search MATCH ?{}"#,
                values.len()
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    values.push(Value::Integer(limit.max(1) as i64));
    let limit_parameter = values.len();
    values.push(Value::Integer(offset as i64));
    let offset_parameter = values.len();
    let sql = format!(
        r#"
        WITH matches(event_id, history_record_id, session_id, role, preview_text, term_index, score) AS MATERIALIZED (
            {selects}
        ),
        ranked(event_id, history_record_id, session_id, role, preview_text, matched_terms, score) AS (
            -- Projection maintenance keeps one row per event. Grouping only by the
            -- canonical event key also collapses legacy duplicate result rows.
            SELECT event_id,
                   MIN(history_record_id),
                   MIN(session_id),
                   MIN(role),
                   MIN(preview_text),
                   COUNT(*),
                   SUM(score)
            FROM matches
            GROUP BY event_id
        )
        {}
        LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}
        "#,
        event_search_hit_sql(
            "ranked AS event_search",
            &event_search_score("event_search.score", prefer_conversation),
            "ORDER BY event_search.matched_terms DESC, search_score, e.occurred_at_ms DESC, e.seq DESC, event_search.event_id",
        )
    );
    (sql, values)
}

pub(super) fn event_search_score(score_sql: &str, prefer_conversation: bool) -> String {
    if prefer_conversation {
        format!(
            "CASE WHEN e.event_type IN ('message', 'summary') THEN ({score_sql}) - (ABS({score_sql}) * 0.15) ELSE ({score_sql}) END"
        )
    } else {
        score_sql.to_owned()
    }
}

pub(super) fn event_search_hit_sql(from_sql: &str, score_sql: &str, tail_sql: &str) -> String {
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
        WHERE {}
        {tail_sql}
        "#,
        event_material_visible_predicate("e")
    )
}
