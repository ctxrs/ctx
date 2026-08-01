use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CoreRecord, EventIdentityInput, EventRole,
    EventType, NativeItemKey, NativeSessionKey, ProjectionContractError, SessionIdentityInput,
    SourceKey, StableEntityId, SubrecordSelector, TypedKey, MAX_CORE_CONTENT_BYTES,
};
use serde_json::Value;

use super::super::{
    records::{assistant_text, lingma_timestamp},
    LingmaRow,
};
use super::{
    LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0, ASSISTANT_ERROR_COORDINATE,
    ASSISTANT_SUMMARY_COORDINATE, LOGICAL_EVENT_KIND, LOGICAL_SESSION_KIND, NATIVE_POSITION_KIND,
    NATIVE_REQUEST_NAMESPACE, NATIVE_SESSION_NAMESPACE, NATIVE_SUBRECORD_NAMESPACE,
    PARSER_REVISION, USER_PROMPT_COORDINATE,
};

struct ProjectedEvent {
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
}

pub(super) struct ParsedRow {
    pub(super) ordinal: u64,
    pub(super) row: LingmaRow,
    pub(super) record_digest: [u8; 32],
    pub(super) request_identity_unique: bool,
}

pub(super) fn project_row(
    source: &SourceKey,
    parsed: ParsedRow,
    records: &mut Vec<CoreRecord>,
) -> LingmaSourceBackedResultV0<()> {
    let session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(parsed.row.session_id.clone())?,
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let native_identity = native_item_identity(&parsed)?;
    let user_sequence = parsed.ordinal.saturating_mul(2);
    let user_event = ProjectedEvent {
        role: EventRole::User,
        event_type: EventType::Message,
        occurred_at: lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH),
    };
    records.push(project_event(
        source,
        session_id,
        &parsed.row,
        &native_identity,
        user_sequence,
        USER_PROMPT_COORDINATE,
        &parsed.row.chat_prompt,
        &parsed.row.chat_prompt,
        user_event,
    )?);

    if let Some((text, body_kind, event_type)) = assistant_text(&parsed.row) {
        let logical_text = text;
        let coordinate = if body_kind == "summary" {
            ASSISTANT_SUMMARY_COORDINATE
        } else {
            ASSISTANT_ERROR_COORDINATE
        };
        let occurred_at = lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH)
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or_else(|| {
                lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH)
            });
        let assistant_event = ProjectedEvent {
            role: EventRole::Assistant,
            event_type,
            occurred_at,
        };
        records.push(project_event(
            source,
            session_id,
            &parsed.row,
            &native_identity,
            user_sequence.saturating_add(1),
            coordinate,
            &logical_text,
            if body_kind == "summary" {
                parsed.row.summary.as_deref().unwrap_or_default()
            } else {
                parsed.row.error_result.as_deref().unwrap_or_default()
            },
            assistant_event,
        )?);
    }
    Ok(())
}

struct LingmaNativeItemIdentity {
    item_key: NativeItemKey,
    coordinate: TypedKey,
}

fn native_item_identity(
    parsed: &ParsedRow,
) -> Result<LingmaNativeItemIdentity, ProjectionContractError> {
    if let Some(request_id) = parsed
        .row
        .request_id
        .as_ref()
        .filter(|request_id| !request_id.trim().is_empty())
        .filter(|_| parsed.request_identity_unique)
    {
        let session = TypedKey::utf8(parsed.row.session_id.clone())?;
        let request = TypedKey::utf8(request_id.clone())?;
        return Ok(LingmaNativeItemIdentity {
            item_key: NativeItemKey::composite(
                NATIVE_REQUEST_NAMESPACE,
                vec![session.clone(), request.clone()],
            )?,
            coordinate: TypedKey::composite(vec![TypedKey::utf8("request")?, session, request])?,
        });
    }
    let revision_scope = TypedKey::bytes(parsed.record_digest.to_vec())?;
    Ok(LingmaNativeItemIdentity {
        item_key: NativeItemKey::revision_scoped_position(
            NATIVE_POSITION_KIND,
            TypedKey::U64(parsed.ordinal),
            revision_scope.clone(),
        )?,
        coordinate: TypedKey::composite(vec![
            TypedKey::utf8("position")?,
            TypedKey::U64(parsed.ordinal),
            revision_scope.clone(),
        ])?,
    })
}

#[allow(clippy::too_many_arguments)]
fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    row: &LingmaRow,
    native_identity: &LingmaNativeItemIdentity,
    event_sequence: u64,
    coordinate_kind: &'static str,
    logical_text: &str,
    provider_content: &str,
    event: ProjectedEvent,
) -> LingmaSourceBackedResultV0<CoreRecord> {
    let subrecord =
        SubrecordSelector::native_id(NATIVE_SUBRECORD_NAMESPACE, TypedKey::utf8(coordinate_kind)?)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_identity.item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    if logical_text.is_empty() {
        return Err(LingmaSourceBackedErrorV0::EmptySelectedBody);
    }
    let native_event_id = TypedKey::composite(vec![
        native_identity.coordinate.clone(),
        TypedKey::utf8(coordinate_kind)?,
    ])?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        PARSER_REVISION,
        logical_text,
    )?;
    record.provider_session_id = Some(row.session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.content.structured_content = structured_content(logical_text, provider_content);
    record.validate_contract()?;
    Ok(record)
}

fn structured_content(body: &str, provider_content: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(provider_content).ok()?;
    if !matches!(value, Value::Array(_) | Value::Object(_)) {
        return None;
    }
    let encoded = serde_json::to_vec(&value).ok()?;
    body.len()
        .checked_add(encoded.len())
        .filter(|bytes| *bytes <= MAX_CORE_CONTENT_BYTES)
        .map(|_| value)
}
