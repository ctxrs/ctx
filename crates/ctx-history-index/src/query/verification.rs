use ctx_history_core::StableEntityId;
use tantivy::{schema::Value as _, DocAddress, TantivyDocument};

use super::records::stored_core_verification_record;
use crate::{source_token, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION};

const VERIFY_QUERY_METADATA: u32 = 17;

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
    let (identity_field, identity_name, source_owner) = match role {
        IdentityFieldRole::Event => (fields.event_identity, "event_identity", None),
        IdentityFieldRole::Session => (
            fields.session_identity,
            "session_identity",
            Some(unique_stored_text(&document, fields.source_key, "source_key")?.to_owned()),
        ),
        IdentityFieldRole::ParentSession => (
            fields.parent_session_identity,
            "parent_session_identity",
            None,
        ),
        IdentityFieldRole::RootSession => {
            (fields.root_session_identity, "root_session_identity", None)
        }
    };
    let identity = StableEntityId::decode_canonical(unique_stored_bytes(
        &document,
        identity_field,
        identity_name,
    )?)?;
    Ok(IdentityRecord {
        identity,
        source_owner,
    })
}

fn unique_stored_bytes<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a [u8]> {
    let mut values = document.get_all(field);
    let value = values
        .next()
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

fn unique_stored_text<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a str> {
    let mut values = document.get_all(field);
    let value = values
        .next()
        .and_then(|value| value.as_str())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}
