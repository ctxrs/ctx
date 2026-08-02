use ctx_history_core::{SourceKey, StableEntityId};
use tantivy::{schema::Value as _, DocAddress, TantivyDocument};

use super::records::{stored_core_verification_record, validate_core_record_encoded_bytes};
use crate::{source_token, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION};

const VERIFY_CORE_RECORD: u32 = 28;

pub(crate) struct VerificationRecord {
    pub(crate) source_owner: String,
    pub(crate) core_record_leaf: [u8; 32],
    pub(crate) has_parent_session: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum IdentityFieldRole {
    Event,
    Session,
    ParentSession,
    RootSession,
}

pub(crate) struct IdentityRecord {
    pub(crate) identity: StableEntityId,
    pub(crate) source_owner: Option<String>,
}

#[derive(serde::Deserialize)]
struct CoreIdentityProjection {
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source: SourceKey,
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
) -> Result<VerificationRecord> {
    let fields = crate::fields_from_schema(searcher.schema())?;
    let (event, core_record_leaf) = stored_core_verification_record(searcher, address, fields)?;
    Ok(VerificationRecord {
        source_owner: source_token(&event.source),
        core_record_leaf,
        has_parent_session: event.parent_session_id.is_some(),
    })
}

pub(crate) fn stored_identity_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    role: IdentityFieldRole,
) -> Result<IdentityRecord> {
    let fields = crate::fields_from_schema(searcher.schema())?;
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded = unique_stored_core(&document, fields.core_record)?;
    validate_core_record_encoded_bytes(searcher, address, encoded.len())?;
    let record: CoreIdentityProjection = serde_json::from_slice(encoded)?;
    record.source.validate_contract()?;
    record.event_id.validate_contract()?;
    record.session_id.validate_contract()?;
    record.root_session_id.validate_contract()?;
    if let Some(parent_session_id) = record.parent_session_id {
        parent_session_id.validate_contract()?;
    }
    let (identity, source_owner) = match role {
        IdentityFieldRole::Event => (record.event_id, None),
        IdentityFieldRole::Session => (record.session_id, Some(source_token(&record.source))),
        IdentityFieldRole::ParentSession => (
            record
                .parent_session_id
                .ok_or(IndexError::InvalidStoredDocumentField("parent_session_id"))?,
            None,
        ),
        IdentityFieldRole::RootSession => (record.root_session_id, None),
    };
    Ok(IdentityRecord {
        identity,
        source_owner,
    })
}

fn unique_stored_core(document: &TantivyDocument, field: tantivy::schema::Field) -> Result<&[u8]> {
    let mut values = document.get_all(field);
    let encoded = values
        .next()
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(encoded)
}
