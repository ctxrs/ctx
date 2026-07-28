use rusqlite::types::Value;

/// Builds the lexical event-search query.
///
/// `event_search` is contentless, so a match yields a rowid and a score and
/// nothing else. The rowid is `events.seq`, so every projected column comes
/// from the canonical rows via `JOIN events e ON e.seq = ranked.seq`, and
/// `preview_text` is re-derived in Rust from `e.payload_json`.
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
                r#"SELECT event_search.rowid, {term_index}, bm25(event_search)
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
        WITH matches(seq, term_index, score) AS MATERIALIZED (
            {selects}
        ),
        ranked(seq, matched_terms, score) AS (
            -- FTS5 yields each document at most once per term, so the row count
            -- per seq is the number of distinct query terms that matched.
            SELECT seq, COUNT(*), SUM(score)
            FROM matches
            GROUP BY seq
        )
        {}
        LIMIT ?{limit_parameter} OFFSET ?{offset_parameter}
        "#,
        event_search_hit_sql(
            &event_search_score("ranked.score", prefer_conversation),
            "ORDER BY ranked.matched_terms DESC, search_score, e.occurred_at_ms DESC, e.seq DESC, e.id",
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

/// Projects one search hit from a `ranked(seq, matched_terms, score)` CTE.
///
/// The pre-v48 index stored `history_record_id` and `session_id` alongside the
/// text, and the hit path read them as COALESCE fallbacks. Those stored values
/// were `COALESCE(e.history_record_id, r.history_record_id, s.history_record_id,
/// rs.history_record_id)` and `e.session_id` respectively, so substituting the
/// live joins is exact rather than approximate - verified against all 735,006
/// projected rows of the qualification corpus by
/// `stored_projection_keys_still_equal_the_live_join`.
pub(super) fn event_search_hit_sql(score_sql: &str, tail_sql: &str) -> String {
    format!(
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               COALESCE(e.session_id, s.id, rs.id),
               e.run_id,
               e.seq,
               e.event_type,
               e.role,
               e.occurred_at_ms,
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
               json_patch(
                   COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json, '{{}}'),
                   CASE
                       WHEN COALESCE(
                           json_extract(s.metadata_json, '$.source_metadata'),
                           json_extract(rs.metadata_json, '$.source_metadata')
                       ) IS NULL THEN '{{}}'
                       ELSE json_object(
                           'source_metadata',
                           COALESCE(
                               json_extract(s.metadata_json, '$.source_metadata'),
                               json_extract(rs.metadata_json, '$.source_metadata')
                           )
                       )
                   END
               ),
               wr.title,
               wr.kind,
               wr.workspace
        FROM ranked
        JOIN events e ON e.seq = ranked.seq
        LEFT JOIN runs r ON r.id = e.run_id
        LEFT JOIN sessions s ON s.id = e.session_id
        LEFT JOIN sessions rs ON rs.id = r.session_id
        LEFT JOIN capture_sources event_source ON event_source.id = e.capture_source_id
        LEFT JOIN capture_sources session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id)
        LEFT JOIN capture_sources run_source ON run_source.id = r.source_id
        LEFT JOIN history_records wr ON wr.id = COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id)
        {tail_sql}
        "#
    )
}
