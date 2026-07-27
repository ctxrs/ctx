use std::collections::HashSet;

use ctx_history_core::{Event, EventRole, EventType, RedactionState, SyncState, Visibility};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use uuid::Uuid;

use crate::connection::{parse_optional_text_enum, parse_text_enum, parse_uuid};
use crate::schema::ddl::table_exists;
use crate::{Result, Store};

use super::encoding::event_search_preview_from_payload;

impl Store {
    pub fn semantic_eligible_event_ids(&self, event_ids: &[Uuid]) -> Result<HashSet<Uuid>> {
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
}

pub(super) fn semantic_lite_turn_anchor_eligible_predicate() -> String {
    semantic_lite_turn_user_eligible_predicate("anchor", "anchor_search")
}

pub(super) fn semantic_lite_turn_user_eligible_predicate(
    event_alias: &str,
    search_alias: &str,
) -> String {
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

pub(super) fn semantic_lite_turn_preview_is_control(preview: &str) -> bool {
    let trimmed = preview.trim();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<turn_aborted>")
        || trimmed.starts_with("<subagent_notification>")
        || trimmed.starts_with("Warning: The maximum number of unified exec processes")
}

pub(super) fn semantic_lookup_event_parts(event_type: EventType, role: Option<&str>) -> bool {
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
    if !event_search_policy_allows(redaction_state, visibility, sync_state, deleted) {
        return false;
    }
    let preview = event_search_preview_from_payload(event_type, role, payload, redaction_state);
    !preview.trim().is_empty() && !semantic_lite_turn_preview_is_control(&preview)
}

pub(super) fn event_search_policy_allows(
    redaction_state: RedactionState,
    visibility: Visibility,
    sync_state: SyncState,
    deleted: bool,
) -> bool {
    !(deleted
        || visibility == Visibility::Withheld
        || sync_state == SyncState::Withheld
        || matches!(
            redaction_state,
            RedactionState::Raw | RedactionState::Withheld
        ))
}
