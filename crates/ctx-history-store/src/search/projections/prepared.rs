use ctx_history_core::{Event, EventRole, EventType, RedactionState, SyncState, Visibility};
use rusqlite::Row;
use uuid::Uuid;

use crate::connection::{optional_uuid_string, parse_optional_text_enum, parse_text_enum};
use crate::Result;

use super::eligibility::event_search_policy_allows;
use super::encoding::event_search_preview_from_payload;

pub(super) struct PreparedEventProjection {
    pub(super) event_id: String,
    pub(super) history_record_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) role: Option<EventRole>,
    pub(super) event_type: EventType,
    pub(super) preview: String,
}

impl PreparedEventProjection {
    pub(super) fn from_event(event_id: Uuid, event: &Event) -> Option<Self> {
        Self {
            event_id: event_id.to_string(),
            history_record_id: optional_uuid_string(event.history_record_id),
            session_id: optional_uuid_string(event.session_id),
            role: event.role,
            event_type: event.event_type,
            preview: String::new(),
        }
        .apply_policy(
            &event.payload,
            RedactionState::SafePreview,
            event.sync.visibility,
            event.sync.sync_state,
            event.sync.deleted_at.is_some(),
        )
    }

    pub(super) fn from_stored_row(row: &Row<'_>) -> Result<Option<Self>> {
        let role = parse_optional_text_enum::<EventRole>(row.get::<_, Option<String>>(3)?)?;
        let event_type = parse_text_enum::<EventType>(row.get::<_, String>(4)?)?;
        let payload_json = row.get::<_, String>(5)?;
        let payload = serde_json::from_str(&payload_json)?;
        let redaction_state = parse_text_enum::<RedactionState>(row.get::<_, String>(6)?)?;
        let visibility = parse_text_enum::<Visibility>(row.get::<_, String>(7)?)?;
        let sync_state = parse_text_enum::<SyncState>(row.get::<_, String>(8)?)?;
        let deleted = row.get::<_, Option<i64>>(9)?.is_some();

        Ok(Self {
            event_id: row.get(0)?,
            history_record_id: row.get(1)?,
            session_id: row.get(2)?,
            role,
            event_type,
            preview: String::new(),
        }
        .apply_policy(&payload, redaction_state, visibility, sync_state, deleted))
    }

    fn apply_policy(
        mut self,
        payload: &serde_json::Value,
        redaction_state: RedactionState,
        visibility: Visibility,
        sync_state: SyncState,
        deleted: bool,
    ) -> Option<Self> {
        if !event_search_policy_allows(redaction_state, visibility, sync_state, deleted) {
            return None;
        }
        self.preview =
            event_search_preview_from_payload(self.event_type, self.role, payload, redaction_state);
        if self.preview.trim().is_empty() {
            None
        } else {
            Some(self)
        }
    }
}
