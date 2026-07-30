use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, EventIdentityInput, EventRole, EventType,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    SessionIdentityInput, SourceKey, SourceRecordLocator, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::native_source::NativeSqliteValue;

use super::super::super::model::item_id;
use super::{
    discovery::AstrBotSourceBackedSourceV0, AstrBotSourceBackedErrorV0,
    AstrBotSourceBackedResultV0, CONVERSATION_MESSAGE_RELATION, CONVERSATION_OUTPUT_RELATION,
    LOGICAL_EVENT_KIND, LOGICAL_SESSION_KIND, PLATFORM_MESSAGE_RELATION, SESSION_NAMESPACE,
};

#[derive(Debug)]
pub(super) struct SessionFact {
    pub(super) provider_session_id: String,
    pub(super) started_at: DateTime<Utc>,
}

#[derive(Debug)]
pub(super) struct EventFact {
    pub(super) source_record_ordinal: u64,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
}

pub(super) fn conversation_native_item_key(
    physical_rowid: i64,
    item_index: usize,
    item: Option<&Value>,
    revision_scope: &TypedKey,
) -> AstrBotSourceBackedResultV0<NativeItemKey> {
    if let Some(native_id) = item.and_then(item_id) {
        Ok(NativeItemKey::composite(
            "astrbot.conversation-item",
            vec![TypedKey::I64(physical_rowid), TypedKey::utf8(native_id)?],
        )?)
    } else {
        Ok(NativeItemKey::revision_scoped_position(
            "astrbot.conversation-position",
            TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::U64(
                    u64::try_from(item_index)
                        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                ),
            ])?,
            revision_scope.clone(),
        )?)
    }
}

pub(super) fn logical_values_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-backed-logical-row-v0\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn conversation_document(
    source: &AstrBotSourceBackedSourceV0,
    physical_rowid: i64,
    item_index: usize,
    row_digest: [u8; 32],
    item: Option<&Value>,
    session: &SessionFact,
    event: &EventFact,
    complete_text: &str,
) -> AstrBotSourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source_key, &session.provider_session_id)?;
    let revision_scope = TypedKey::bytes(row_digest.to_vec())?;
    let native_item_key =
        conversation_native_item_key(physical_rowid, item_index, item, &revision_scope)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source_key,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let logical_relation = if event.event_type == EventType::Message {
        CONVERSATION_MESSAGE_RELATION
    } else {
        CONVERSATION_OUTPUT_RELATION
    };
    let locator = SourceRecordLocator::new(
        source.source_key.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::U64(
                    u64::try_from(item_index)
                        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                ),
            ])?,
            row_version: Some(TypedKey::bytes(row_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        Sha256::digest(complete_text.as_bytes()).into(),
    )?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source_key.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: Some(source.path.to_string_lossy().into_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.source_record_ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: complete_text.to_owned(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

// These eight values are the explicit certified identity and projection inputs;
// bundling them would only obscure the provider-local contract.
#[allow(clippy::too_many_arguments)]
pub(super) fn platform_document(
    source: &AstrBotSourceBackedSourceV0,
    physical_rowid: i64,
    logical_id: i64,
    row_digest: [u8; 32],
    session: &SessionFact,
    event: &EventFact,
    complete_text: &str,
) -> AstrBotSourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source_key, &session.provider_session_id)?;
    let native_item_key =
        NativeItemKey::native_id("astrbot.platform-message", TypedKey::I64(logical_id))?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source_key,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.source_key.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: PLATFORM_MESSAGE_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::I64(logical_id),
            ])?,
            row_version: Some(TypedKey::bytes(row_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        Sha256::digest(complete_text.as_bytes()).into(),
    )?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source_key.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: Some(source.path.to_string_lossy().into_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.source_record_ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: complete_text.to_owned(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

pub(super) fn stable_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> AstrBotSourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(SESSION_NAMESPACE, TypedKey::utf8(provider_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}
