use ctx_history_core::StableEntityId;
use tantivy::DocAddress;

use super::records::stored_core_verification_record;
use crate::{
    index_document::{
        core_content_bytes, EventRangeOrderKey, SemanticEventOrderKey, SessionEventOrderKey,
        SourceEventOrderKey, EVENT_RANGE_ORDER_KEY_LEN, SEMANTIC_EVENT_ORDER_KEY_LEN,
        SESSION_EVENT_ORDER_KEY_LEN, SOURCE_EVENT_ORDER_KEY_LEN,
    },
    source_token, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION,
};

const VERIFY_CORE_RECORD: u32 = 28;

pub(crate) struct VerificationRecord {
    pub(crate) core_record: ctx_history_core::CoreRecord,
    pub(crate) source_owner: String,
    pub(crate) core_record_leaf: [u8; 32],
    pub(crate) source_event_order: [u8; SOURCE_EVENT_ORDER_KEY_LEN],
    pub(crate) session_event_order: [u8; SESSION_EVENT_ORDER_KEY_LEN],
    pub(crate) semantic_event_order: [u8; SEMANTIC_EVENT_ORDER_KEY_LEN],
    pub(crate) event_range_order: [u8; EVENT_RANGE_ORDER_KEY_LEN],
    pub(crate) body: Option<String>,
    pub(crate) identities: CompactVerificationIdentities,
    pub(crate) stored_core_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactIdentity {
    pub(crate) digest: [u8; 32],
}

impl From<StableEntityId> for CompactIdentity {
    fn from(identity: StableEntityId) -> Self {
        let compact = Self {
            digest: identity.digest(),
        };
        debug_assert_eq!(compact.as_uuid(), identity.as_uuid());
        compact
    }
}

impl CompactIdentity {
    pub(crate) fn as_uuid(self) -> uuid::Uuid {
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&self.digest[..16]);
        bytes[6] = 0x80 | (bytes[6] & 0x0f);
        bytes[8] = 0x80 | (bytes[8] & 0x3f);
        uuid::Uuid::from_bytes(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactVerificationIdentities {
    pub(crate) event: CompactIdentity,
    pub(crate) session: CompactIdentity,
    pub(crate) parent_session: Option<CompactIdentity>,
    pub(crate) root_session: CompactIdentity,
    pub(crate) session_source_owner: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum IdentityFieldRole {
    Session,
    ParentSession,
    RootSession,
}

pub(crate) fn validate_verification_projection(fields: Fields) -> Result<()> {
    if fields.core_record.field_id() != VERIFY_CORE_RECORD {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub(crate) fn stored_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<VerificationRecord> {
    let (record, core_record_leaf, stored_core_bytes) =
        stored_core_verification_record(searcher, address, fields)?;
    let core = record.core_record;
    let source_owner = source_token(&core.source);
    let source_event_order =
        SourceEventOrderKey::for_core_record(&core, stored_core_bytes)?.into_bytes();
    let session_event_order = SessionEventOrderKey::for_core_record(&core)?.into_bytes();
    let semantic_event_order = SemanticEventOrderKey::for_event(core.event_id)?.into_bytes();
    let event_range_order = EventRangeOrderKey::for_core_record(
        &core,
        stored_core_bytes,
        core_content_bytes(&core.content)?,
    )?
    .into_bytes();
    let body = core.content.normalized_body.clone();
    let identities = CompactVerificationIdentities {
        event: core.event_id.into(),
        session: core.session_id.into(),
        parent_session: core.parent_session_id.map(CompactIdentity::from),
        root_session: core.root_session_id.into(),
        session_source_owner: core.source.identity().digest(),
    };
    Ok(VerificationRecord {
        core_record: core,
        source_owner,
        core_record_leaf,
        source_event_order,
        session_event_order,
        semantic_event_order,
        event_range_order,
        body,
        identities,
        stored_core_bytes,
    })
}
