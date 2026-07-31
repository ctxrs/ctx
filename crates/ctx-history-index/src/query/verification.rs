use ctx_history_core::StableEntityId;
use tantivy::DocAddress;

use super::stored_event_record;
use crate::{source_token, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION};

const VERIFY_QUERY_METADATA: u32 = 17;

pub(crate) struct VerificationRecord {
    pub(crate) event_id: StableEntityId,
    pub(crate) session_id: StableEntityId,
    pub(crate) parent_session_id: Option<StableEntityId>,
    pub(crate) root_session_id: StableEntityId,
    pub(crate) source_owner: String,
}

pub(crate) fn validate_verification_projection(fields: Fields) -> Result<()> {
    if fields.query_metadata.field_id() != VERIFY_QUERY_METADATA {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub(crate) fn stored_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<VerificationRecord> {
    let fields = crate::fields_from_schema(searcher.schema())?;
    let event = stored_event_record(searcher, address, fields)?;
    Ok(VerificationRecord {
        event_id: event.event_id,
        session_id: event.session_id,
        parent_session_id: event.parent_session_id,
        root_session_id: event.root_session_id,
        source_owner: source_token(&event.source),
    })
}
