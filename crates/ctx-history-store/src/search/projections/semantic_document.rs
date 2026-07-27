use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType, RedactionState};
use rusqlite::{params, params_from_iter};
use uuid::Uuid;

use crate::connection::{
    collect_rows, parse_optional_text_enum, parse_optional_uuid, parse_text_enum, parse_uuid,
};
use crate::{Result, Store};

use super::eligibility::{
    semantic_lite_turn_anchor_eligible_predicate, semantic_lite_turn_user_eligible_predicate,
};
use super::encoding::local_preview;
use super::identity::{event_search_source_identity, EventEmbeddingDocument};
use super::storage::{
    cached_semantic_searchable_item_count, refresh_semantic_searchable_item_stats,
    semantic_searchable_item_count_exact,
};

const SEMANTIC_TURN_TEXT_MAX_CHARS: usize = 64 * 1024;
const SEMANTIC_LITE_TURN_RANK_BUCKET: &str = "lite_turn";

impl Store {
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
}

pub(super) fn semantic_lite_turn_document_select_sql(
    anchor_tail: &str,
    document_tail: &str,
) -> String {
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
        anchor_occurred_at_ms: row.get(22)?,
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

pub(super) fn semantic_lite_turn_source_chunk(
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
