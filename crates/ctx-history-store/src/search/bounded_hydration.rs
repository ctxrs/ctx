use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use rusqlite::{params_from_iter, types::Value, ErrorCode};
use uuid::Uuid;

use crate::connection::{
    ms_to_time, nonnegative_i64_to_u64, parse_optional_text_enum, parse_optional_uuid,
    parse_text_enum, parse_uuid,
};
use crate::provider_files::event_material_visible_predicate;
use crate::search::projections::EventSearchHit;
use crate::{Result, Store, StoreError};

const BOUNDED_ID_CHUNK_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSearchPreview {
    pub event_id: Uuid,
    pub preview: String,
    pub is_complete: bool,
}

impl Store {
    pub fn event_search_previews_by_ids_bounded_visible(
        &self,
        event_ids: &[Uuid],
        row_limit: usize,
        total_byte_limit: usize,
        per_lookup_byte_limit: usize,
        timeout: Duration,
    ) -> Result<Vec<EventSearchPreview>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if event_ids.is_empty()
            || row_limit == 0
            || total_byte_limit == 0
            || per_lookup_byte_limit == 0
        {
            return Ok(Vec::new());
        }
        let requested = bounded_unique_ids(event_ids, row_limit);
        let mut previews = run_bounded_lookup(&self.conn, timeout, || {
            self.lookup_event_previews(&requested, total_byte_limit, per_lookup_byte_limit)
        })?;
        let mut total_bytes = 0usize;
        let mut result = Vec::with_capacity(previews.len());
        for event_id in requested {
            let Some(preview) = previews.remove(&event_id) else {
                continue;
            };
            let bytes = preview.preview.len();
            if bytes > per_lookup_byte_limit || total_bytes.saturating_add(bytes) > total_byte_limit
            {
                continue;
            }
            total_bytes = total_bytes.saturating_add(bytes);
            result.push(preview);
        }
        Ok(result)
    }

    pub fn event_search_hits_by_scores_compact_bounded_visible(
        &self,
        candidate_scores: &[(Uuid, f64)],
        row_limit: usize,
        total_byte_limit: usize,
        per_event_byte_limit: usize,
        timeout: Duration,
    ) -> Result<Vec<EventSearchHit>> {
        self.event_search_hits_by_scores_bounded_visible_inner(
            candidate_scores,
            row_limit,
            total_byte_limit,
            per_event_byte_limit,
            false,
            timeout,
        )
    }

    pub fn event_search_hits_by_scores_bounded_visible(
        &self,
        candidate_scores: &[(Uuid, f64)],
        row_limit: usize,
        total_byte_limit: usize,
        per_event_byte_limit: usize,
        timeout: Duration,
    ) -> Result<Vec<EventSearchHit>> {
        self.event_search_hits_by_scores_bounded_visible_inner(
            candidate_scores,
            row_limit,
            total_byte_limit,
            per_event_byte_limit,
            true,
            timeout,
        )
    }

    fn event_search_hits_by_scores_bounded_visible_inner(
        &self,
        candidate_scores: &[(Uuid, f64)],
        row_limit: usize,
        total_byte_limit: usize,
        per_event_byte_limit: usize,
        include_cursor: bool,
        timeout: Duration,
    ) -> Result<Vec<EventSearchHit>> {
        let _projection_snapshot = self.begin_readable_search_projection()?;
        if candidate_scores.is_empty()
            || row_limit == 0
            || total_byte_limit == 0
            || per_event_byte_limit == 0
        {
            return Ok(Vec::new());
        }
        let requested = bounded_unique_scored_ids(candidate_scores, row_limit);
        let ids = requested.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let mut hits = run_bounded_lookup(&self.conn, timeout, || {
            self.lookup_event_hits(&ids, total_byte_limit, per_event_byte_limit, include_cursor)
        })?;
        let scores = requested.iter().copied().collect::<HashMap<_, _>>();
        let mut total_bytes = 0usize;
        let mut result = Vec::with_capacity(hits.len());
        for (event_id, score) in requested {
            let Some(mut hit) = hits.remove(&event_id) else {
                continue;
            };
            hit.score = scores.get(&event_id).copied().unwrap_or(score);
            let bytes = hit.input_bytes();
            if bytes > per_event_byte_limit || total_bytes.saturating_add(bytes) > total_byte_limit
            {
                continue;
            }
            total_bytes = total_bytes.saturating_add(bytes);
            result.push(hit);
        }
        Ok(result)
    }

    fn lookup_event_previews(
        &self,
        event_ids: &[Uuid],
        total_byte_limit: usize,
        per_lookup_byte_limit: usize,
    ) -> Result<HashMap<Uuid, EventSearchPreview>> {
        let mut result = HashMap::with_capacity(event_ids.len());
        let mut retained_bytes = 0usize;
        for chunk in event_ids.chunks(BOUNDED_ID_CHUNK_SIZE) {
            let lookup_sql = bounded_preview_sql(chunk.len(), true, per_lookup_byte_limit);
            collect_preview_rows(
                &self.conn,
                &lookup_sql,
                chunk,
                total_byte_limit,
                per_lookup_byte_limit,
                &mut retained_bytes,
                &mut result,
            )?;
            let missing = chunk
                .iter()
                .copied()
                .filter(|event_id| !result.contains_key(event_id))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                let fallback_sql = bounded_preview_sql(missing.len(), false, per_lookup_byte_limit);
                collect_preview_rows(
                    &self.conn,
                    &fallback_sql,
                    &missing,
                    total_byte_limit,
                    per_lookup_byte_limit,
                    &mut retained_bytes,
                    &mut result,
                )?;
            }
        }
        Ok(result)
    }

    fn lookup_event_hits(
        &self,
        event_ids: &[Uuid],
        total_byte_limit: usize,
        per_event_byte_limit: usize,
        include_cursor: bool,
    ) -> Result<HashMap<Uuid, EventSearchHit>> {
        let mut result = HashMap::with_capacity(event_ids.len());
        let mut retained_bytes = 0usize;
        for chunk in event_ids.chunks(BOUNDED_ID_CHUNK_SIZE) {
            let lookup_sql =
                bounded_hit_sql(chunk.len(), true, per_event_byte_limit, include_cursor);
            collect_hit_rows(
                &self.conn,
                &lookup_sql,
                chunk,
                total_byte_limit,
                per_event_byte_limit,
                &mut retained_bytes,
                &mut result,
            )?;
            let missing = chunk
                .iter()
                .copied()
                .filter(|event_id| !result.contains_key(event_id))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                let fallback_sql =
                    bounded_hit_sql(missing.len(), false, per_event_byte_limit, include_cursor);
                collect_hit_rows(
                    &self.conn,
                    &fallback_sql,
                    &missing,
                    total_byte_limit,
                    per_event_byte_limit,
                    &mut retained_bytes,
                    &mut result,
                )?;
            }
        }
        Ok(result)
    }
}

fn run_bounded_lookup<T>(
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

fn bounded_unique_ids(ids: &[Uuid], limit: usize) -> Vec<Uuid> {
    let mut seen = HashSet::with_capacity(ids.len().min(limit));
    ids.iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .take(limit)
        .collect()
}

fn bounded_unique_scored_ids(ids: &[(Uuid, f64)], limit: usize) -> Vec<(Uuid, f64)> {
    let mut seen = HashSet::with_capacity(ids.len().min(limit));
    ids.iter()
        .copied()
        .filter(|(id, _)| seen.insert(*id))
        .take(limit)
        .collect()
}

fn requested_values_sql(count: usize) -> String {
    (0..count)
        .map(|index| format!("(?{}, {index})", index.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn bounded_text_sql(expression: &str, maximum_bytes: usize) -> String {
    let conservative_chars = maximum_bytes.saturating_div(4);
    format!(
        "CASE WHEN {expression} IS NULL THEN NULL \
         WHEN length(CAST({expression} AS BLOB)) <= {maximum_bytes} THEN {expression} \
         ELSE substr({expression}, 1, {conservative_chars}) END"
    )
}

fn bounded_text_complete_sql(expression: &str, maximum_bytes: usize) -> String {
    format!(
        "CASE WHEN {expression} IS NULL THEN 1 \
         ELSE length(CAST({expression} AS BLOB)) <= {maximum_bytes} END"
    )
}

fn canonical_preview_sql() -> String {
    let body_text = "CASE WHEN json_type(e.payload_json, '$.body') = 'text' \
                     THEN json_extract(e.payload_json, '$.body') END";
    let text = |path: &str| format!("json_extract(e.payload_json, '{path}')");
    let conversation = format!(
        "COALESCE({}, {}, {}, {}, {}, {}, {}, {}, {})",
        text("$.body.text"),
        text("$.body.preview"),
        text("$.body.summary"),
        text("$.body.message"),
        body_text,
        text("$.text"),
        text("$.preview"),
        text("$.summary"),
        text("$.message")
    );
    let tool = format!(
        "trim(COALESCE({}, '') || ' ' || COALESCE({}, '') || ' ' || \
         COALESCE({}, '') || ' ' || COALESCE({}, '') || ' ' || \
         COALESCE({}, '') || ' ' || COALESCE({}, '') || ' ' || \
         COALESCE({}, '') || ' ' || COALESCE({}, '') || ' ' || \
         COALESCE({}, '') || ' ' || COALESCE({}, '') || ' ' || \
         COALESCE({}, '') || ' ' || COALESCE({}, ''))",
        text("$.body.command"),
        text("$.command"),
        text("$.body.text"),
        text("$.text"),
        text("$.body.tool"),
        text("$.tool"),
        text("$.body.name"),
        text("$.name"),
        text("$.body.arguments_preview"),
        text("$.arguments_preview"),
        text("$.body.status"),
        text("$.status")
    );
    let output = format!(
        "COALESCE({}, {}, {conversation})",
        text("$.body.output_preview"),
        text("$.output_preview")
    );
    format!(
        "CASE \
         WHEN e.event_type IN ('message', 'summary') THEN {conversation} \
         WHEN e.event_type IN ('tool_call', 'command_started', 'command_finished') THEN {tool} \
         WHEN e.event_type IN ('tool_output', 'command_output') THEN {output} \
         ELSE '' END"
    )
}

fn source_metadata_sql() -> &'static str {
    "COALESCE(event_source.metadata_json, session_source.metadata_json, run_source.metadata_json)"
}

fn bounded_preview_sql(count: usize, use_lookup: bool, maximum_bytes: usize) -> String {
    let requested = requested_values_sql(count);
    let preview = if use_lookup {
        "search.preview_text".to_owned()
    } else {
        canonical_preview_sql()
    };
    let lookup_join = if use_lookup {
        "JOIN event_search_lookup AS search ON search.event_id = requested.id"
    } else {
        "AND NOT EXISTS (SELECT 1 FROM event_search_lookup AS existing_lookup \
         WHERE existing_lookup.event_id = e.id)"
    };
    let visible = event_material_visible_predicate("e");
    let preview_complete = bounded_text_complete_sql(&preview, maximum_bytes);
    let preview = bounded_text_sql(&preview, maximum_bytes);
    format!(
        "WITH requested(id, ordinal) AS (VALUES {requested}) \
         SELECT e.id, {preview}, {preview_complete} \
         FROM requested \
         JOIN events AS e ON e.id = requested.id \
         {lookup_join} \
         WHERE {visible} \
         ORDER BY requested.ordinal"
    )
}

fn bounded_hit_sql(
    count: usize,
    use_lookup: bool,
    maximum_bytes: usize,
    include_cursor: bool,
) -> String {
    let requested = requested_values_sql(count);
    let preview = if use_lookup {
        "search.preview_text".to_owned()
    } else {
        canonical_preview_sql()
    };
    let lookup_join = if use_lookup {
        "JOIN event_search_lookup AS search ON search.event_id = requested.id"
    } else {
        "LEFT JOIN event_search_lookup AS search ON 0"
    };
    let metadata = source_metadata_sql();
    let plugin_name = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_plugin.plugin_name'), \
         json_extract({metadata}, '$.ctx_history_plugin.plugin_name'))"
    );
    let plugin_source_id = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_plugin.plugin_source_id'), \
         json_extract({metadata}, '$.ctx_history_plugin.plugin_source_id'))"
    );
    let history_source = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_plugin.history_source'), \
         json_extract({metadata}, '$.ctx_history_plugin.history_source'), \
         CASE WHEN {plugin_name} IS NOT NULL AND {plugin_source_id} IS NOT NULL \
         THEN {plugin_name} || '/' || {plugin_source_id} END)"
    );
    let provider_key = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_jsonl_v1.provider_key'), \
         json_extract({metadata}, '$.ctx_history_jsonl_v1.provider_key'))"
    );
    let source_id = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_jsonl_v1.source_id'), \
         json_extract({metadata}, '$.ctx_history_jsonl_v1.source_id'))"
    );
    let source_format = format!(
        "COALESCE(json_extract({metadata}, '$.source_metadata.ctx_history_jsonl_v1.source_format'), \
         json_extract({metadata}, '$.ctx_history_jsonl_v1.source_format'), \
         json_extract({metadata}, '$.source_metadata.source_format'), \
         json_extract({metadata}, '$.source_format'))"
    );
    let cursor = if include_cursor {
        format!(
            "COALESCE(json_extract(e.payload_json, '$.cursor'), \
             json_extract(e.payload_json, '$.body.cursor'), \
             json_extract({metadata}, '$.cursor.after.cursor'))"
        )
    } else {
        "NULL".to_owned()
    };
    let text = |expression: &str| bounded_text_sql(expression, maximum_bytes);
    let visible = event_material_visible_predicate("e");
    let record_visible = crate::provider_files::history_record_material_visible_predicate("wr");
    let missing_lookup = if use_lookup {
        "1 = 1"
    } else {
        "NOT EXISTS (SELECT 1 FROM event_search_lookup AS existing_lookup \
         WHERE existing_lookup.event_id = e.id)"
    };
    format!(
        "WITH requested(id, ordinal) AS (VALUES {requested}) \
         SELECT e.id, \
                COALESCE(e.history_record_id, search.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id), \
                COALESCE(e.session_id, search.session_id, s.id, rs.id), \
                e.run_id, e.seq, e.event_type, e.role, e.occurred_at_ms, \
                {preview}, 0.0, \
                COALESCE(s.provider, rs.provider, event_source.provider, session_source.provider, run_source.provider), \
                {external_session}, COALESCE(s.parent_session_id, rs.parent_session_id), \
                COALESCE(s.root_session_id, rs.root_session_id), \
                COALESCE(s.agent_type, rs.agent_type), COALESCE(s.is_primary, rs.is_primary), \
                {cwd}, {raw_source_path}, {cursor}, {record_title}, {record_kind}, \
                {record_workspace}, {history_source}, {plugin_name}, {provider_key}, \
                {source_id}, {source_format} \
         FROM requested \
         JOIN events AS e ON e.id = requested.id \
         {lookup_join} \
         LEFT JOIN runs AS r ON r.id = e.run_id \
         LEFT JOIN sessions AS s ON s.id = COALESCE(e.session_id, search.session_id) \
         LEFT JOIN sessions AS rs ON rs.id = r.session_id \
         LEFT JOIN capture_sources AS event_source ON event_source.id = e.capture_source_id \
         LEFT JOIN capture_sources AS session_source ON session_source.id = COALESCE(s.capture_source_id, rs.capture_source_id) \
         LEFT JOIN capture_sources AS run_source ON run_source.id = r.source_id \
         LEFT JOIN history_records AS wr ON wr.id = COALESCE(e.history_record_id, search.history_record_id, s.history_record_id, rs.history_record_id, r.history_record_id) \
             AND {record_visible} \
         WHERE {missing_lookup} AND {visible} \
         ORDER BY requested.ordinal",
        preview = text(&preview),
        external_session = text("COALESCE(s.external_session_id, rs.external_session_id)"),
        cwd = text("COALESCE(event_source.cwd, session_source.cwd, run_source.cwd)"),
        raw_source_path =
            text("COALESCE(event_source.raw_source_path, session_source.raw_source_path, run_source.raw_source_path)"),
        cursor = text(&cursor),
        record_title = text("wr.title"),
        record_kind = text("wr.kind"),
        record_workspace = text("wr.workspace"),
        history_source = text(&history_source),
        plugin_name = text(&plugin_name),
        provider_key = text(&provider_key),
        source_id = text(&source_id),
        source_format = text(&source_format),
    )
}

fn collect_preview_rows(
    conn: &rusqlite::Connection,
    sql: &str,
    event_ids: &[Uuid],
    total_byte_limit: usize,
    per_lookup_byte_limit: usize,
    retained_bytes: &mut usize,
    output: &mut HashMap<Uuid, EventSearchPreview>,
) -> Result<()> {
    let values = event_ids
        .iter()
        .map(|event_id| Value::Text(event_id.to_string()))
        .collect::<Vec<_>>();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let event_id = parse_uuid(row.get::<_, String>(0)?)?;
        let preview = row.get::<_, Option<String>>(1)?.unwrap_or_default();
        let is_complete = row.get::<_, i64>(2)? != 0;
        let bytes = preview.len();
        if (preview.trim().is_empty() && is_complete)
            || bytes > per_lookup_byte_limit
            || retained_bytes.saturating_add(bytes) > total_byte_limit
        {
            continue;
        }
        *retained_bytes = retained_bytes.saturating_add(bytes);
        output.insert(
            event_id,
            EventSearchPreview {
                event_id,
                preview,
                is_complete,
            },
        );
    }
    Ok(())
}

fn collect_hit_rows(
    conn: &rusqlite::Connection,
    sql: &str,
    event_ids: &[Uuid],
    total_byte_limit: usize,
    per_event_byte_limit: usize,
    retained_bytes: &mut usize,
    output: &mut HashMap<Uuid, EventSearchHit>,
) -> Result<()> {
    let values = event_ids
        .iter()
        .map(|event_id| Value::Text(event_id.to_string()))
        .collect::<Vec<_>>();
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let event_id = parse_uuid(row.get::<_, String>(0)?)?;
        let preview = row.get::<_, Option<String>>(8)?.unwrap_or_default();
        if preview.trim().is_empty() {
            continue;
        }
        let hit = EventSearchHit {
            event_id,
            history_record_id: parse_optional_uuid(row.get(1)?)?,
            session_id: parse_optional_uuid(row.get(2)?)?,
            run_id: parse_optional_uuid(row.get(3)?)?,
            seq: nonnegative_i64_to_u64(row.get(4)?)?,
            event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
            role: parse_optional_text_enum::<EventRole>(row.get(6)?)?,
            occurred_at: ms_to_time(row.get(7)?)?,
            preview,
            score: row.get(9)?,
            provider: parse_optional_text_enum::<CaptureProvider>(row.get(10)?)?,
            session_external_session_id: row.get(11)?,
            session_parent_session_id: parse_optional_uuid(row.get(12)?)?,
            session_root_session_id: parse_optional_uuid(row.get(13)?)?,
            agent_type: parse_optional_text_enum::<AgentType>(row.get(14)?)?,
            session_is_primary: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
            cwd: row.get(16)?,
            raw_source_path: row.get(17)?,
            cursor: row.get(18)?,
            record_title: row.get(19)?,
            record_kind: row.get(20)?,
            record_workspace: row.get(21)?,
            history_source: row.get(22)?,
            history_source_plugin: row.get(23)?,
            provider_key: row.get(24)?,
            source_id: row.get(25)?,
            source_format: row.get(26)?,
        };
        let bytes = hit.input_bytes();
        if bytes > per_event_byte_limit || retained_bytes.saturating_add(bytes) > total_byte_limit {
            continue;
        }
        *retained_bytes = retained_bytes.saturating_add(bytes);
        output.insert(event_id, hit);
    }
    Ok(())
}

impl EventSearchHit {
    /// Bytes read into memory for bounded verification and hydration.
    pub fn input_bytes(&self) -> usize {
        [
            Some(self.preview.as_str()),
            self.session_external_session_id.as_deref(),
            self.history_source.as_deref(),
            self.history_source_plugin.as_deref(),
            self.provider_key.as_deref(),
            self.source_id.as_deref(),
            self.source_format.as_deref(),
            self.cwd.as_deref(),
            self.raw_source_path.as_deref(),
            self.cursor.as_deref(),
            self.record_title.as_deref(),
            self.record_kind.as_deref(),
            self.record_workspace.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::len)
        .fold(0usize, usize::saturating_add)
    }
}
