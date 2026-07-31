use ctx_history_core::{SourceRecordLocator, StableEntityId, StableEntityKind};
use tantivy::{
    schema::document::{
        DeserializeError, DocumentDeserialize, DocumentDeserializer, ValueDeserialize,
        ValueDeserializer, ValueVisitor,
    },
    DocAddress,
};

use crate::{hex, source_token, Fields, IndexError, Result, LEXICAL_SCHEMA_VERSION};

pub(crate) struct VerificationRecord {
    pub(crate) event_id: StableEntityId,
    pub(crate) session_id: StableEntityId,
    pub(crate) parent_session_id: Option<StableEntityId>,
    pub(crate) root_session_id: StableEntityId,
    pub(crate) source_owner: String,
}

// Verification uses a custom Tantivy document projection. The field ordinals
// are part of the exact schema checked at the generation boundary; keeping the
// projection here avoids retaining and then cloning every unrelated stored
// metadata field for every document.
const VERIFY_EVENT_ID: u32 = 0;
const VERIFY_EVENT_IDENTITY_DIGEST: u32 = 1;
const VERIFY_EVENT_IDENTITY: u32 = 2;
const VERIFY_SESSION_ID: u32 = 5;
const VERIFY_SESSION_IDENTITY_DIGEST: u32 = 6;
const VERIFY_SESSION_IDENTITY: u32 = 7;
const VERIFY_PARENT_SESSION_ID: u32 = 8;
const VERIFY_PARENT_SESSION_IDENTITY: u32 = 9;
const VERIFY_ROOT_SESSION_ID: u32 = 10;
const VERIFY_ROOT_SESSION_IDENTITY: u32 = 11;
const VERIFY_SOURCE_KEY: u32 = 12;
const VERIFY_NATIVE_LOCATOR: u32 = 13;
const VERIFY_PROVIDER: u32 = 14;
const VERIFY_SOURCE_FORMAT: u32 = 15;

enum VerificationStoredValue {
    Text(String),
    Bytes(Vec<u8>),
    Ignored,
}

#[derive(Default)]
struct VerificationStoredDocument {
    event_id: Option<VerificationStoredValue>,
    event_identity_digest: Option<VerificationStoredValue>,
    event_identity: Option<VerificationStoredValue>,
    session_id: Option<VerificationStoredValue>,
    session_identity_digest: Option<VerificationStoredValue>,
    session_identity: Option<VerificationStoredValue>,
    parent_session_id: Option<VerificationStoredValue>,
    parent_session_identity: Option<VerificationStoredValue>,
    root_session_id: Option<VerificationStoredValue>,
    root_session_identity: Option<VerificationStoredValue>,
    source_key: Option<VerificationStoredValue>,
    native_locator: Option<VerificationStoredValue>,
    provider: Option<VerificationStoredValue>,
    source_format: Option<VerificationStoredValue>,
}

impl VerificationStoredDocument {
    fn projected_slot(&mut self, field_id: u32) -> Option<&mut Option<VerificationStoredValue>> {
        match field_id {
            VERIFY_EVENT_ID => Some(&mut self.event_id),
            VERIFY_EVENT_IDENTITY_DIGEST => Some(&mut self.event_identity_digest),
            VERIFY_EVENT_IDENTITY => Some(&mut self.event_identity),
            VERIFY_SESSION_ID => Some(&mut self.session_id),
            VERIFY_SESSION_IDENTITY_DIGEST => Some(&mut self.session_identity_digest),
            VERIFY_SESSION_IDENTITY => Some(&mut self.session_identity),
            VERIFY_PARENT_SESSION_ID => Some(&mut self.parent_session_id),
            VERIFY_PARENT_SESSION_IDENTITY => Some(&mut self.parent_session_identity),
            VERIFY_ROOT_SESSION_ID => Some(&mut self.root_session_id),
            VERIFY_ROOT_SESSION_IDENTITY => Some(&mut self.root_session_identity),
            VERIFY_SOURCE_KEY => Some(&mut self.source_key),
            VERIFY_NATIVE_LOCATOR => Some(&mut self.native_locator),
            VERIFY_PROVIDER => Some(&mut self.provider),
            VERIFY_SOURCE_FORMAT => Some(&mut self.source_format),
            _ => None,
        }
    }
}

impl DocumentDeserialize for VerificationStoredDocument {
    fn deserialize<'de, D>(mut deserializer: D) -> std::result::Result<Self, DeserializeError>
    where
        D: DocumentDeserializer<'de>,
    {
        let mut document = Self::default();
        while let Some((field, value)) = deserializer.next_field::<VerificationStoredValue>()? {
            if let Some(slot) = document.projected_slot(field.field_id()) {
                if slot.is_none() {
                    *slot = Some(value);
                }
            }
        }
        Ok(document)
    }
}

struct VerificationValueVisitor;

impl ValueVisitor for VerificationValueVisitor {
    type Value = VerificationStoredValue;

    fn visit_null(&self) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_string(&self, value: String) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Text(value))
    }

    fn visit_u64(&self, _value: u64) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_i64(&self, _value: i64) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_f64(&self, _value: f64) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_bool(&self, _value: bool) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_datetime(
        &self,
        _value: tantivy::DateTime,
    ) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_ip_address(
        &self,
        _value: std::net::Ipv6Addr,
    ) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_facet(
        &self,
        _value: tantivy::schema::Facet,
    ) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }

    fn visit_bytes(&self, value: Vec<u8>) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Bytes(value))
    }

    fn visit_pre_tokenized_string(
        &self,
        _value: tantivy::tokenizer::PreTokenizedString,
    ) -> std::result::Result<Self::Value, DeserializeError> {
        Ok(VerificationStoredValue::Ignored)
    }
}

impl ValueDeserialize for VerificationStoredValue {
    fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Self, DeserializeError>
    where
        D: ValueDeserializer<'de>,
    {
        deserializer.deserialize_any(VerificationValueVisitor)
    }
}

pub(crate) fn validate_verification_projection(fields: Fields) -> Result<()> {
    let field_ids = [
        (fields.event_id, VERIFY_EVENT_ID),
        (fields.event_identity_digest, VERIFY_EVENT_IDENTITY_DIGEST),
        (fields.event_identity, VERIFY_EVENT_IDENTITY),
        (fields.session_id, VERIFY_SESSION_ID),
        (
            fields.session_identity_digest,
            VERIFY_SESSION_IDENTITY_DIGEST,
        ),
        (fields.session_identity, VERIFY_SESSION_IDENTITY),
        (fields.parent_session_id, VERIFY_PARENT_SESSION_ID),
        (
            fields.parent_session_identity,
            VERIFY_PARENT_SESSION_IDENTITY,
        ),
        (fields.root_session_id, VERIFY_ROOT_SESSION_ID),
        (fields.root_session_identity, VERIFY_ROOT_SESSION_IDENTITY),
        (fields.source_key, VERIFY_SOURCE_KEY),
        (fields.native_locator, VERIFY_NATIVE_LOCATOR),
        (fields.provider, VERIFY_PROVIDER),
        (fields.source_format, VERIFY_SOURCE_FORMAT),
    ];
    if field_ids
        .into_iter()
        .any(|(field, expected)| field.field_id() != expected)
    {
        return Err(IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION));
    }
    Ok(())
}

pub(crate) fn stored_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<VerificationRecord> {
    let document: VerificationStoredDocument = searcher.doc(address)?;
    let event_id = projected_identity(
        document.event_identity,
        document.event_id,
        document.event_identity_digest,
        StableEntityKind::Event,
        "event_identity",
    )?;
    let session_id = projected_identity(
        document.session_identity,
        document.session_id,
        document.session_identity_digest,
        StableEntityKind::Session,
        "session_identity",
    )?;
    let parent_session_id = optional_projected_session_identity(
        document.parent_session_identity,
        document.parent_session_id,
        "parent_session_identity",
    )?;
    let root_session_id = projected_session_identity(
        document.root_session_identity,
        document.root_session_id,
        "root_session_identity",
    )?;
    let locator_bytes = projected_bytes(document.native_locator, "native_locator")?;
    let locator: SourceRecordLocator = serde_json::from_slice(&locator_bytes)?;
    locator.validate_contract()?;
    let stored_source = projected_text(document.source_key, "source_key")?;
    let source_owner = source_token(locator.source());
    if stored_source != source_owner
        || event_id.source_digest() != locator.source().identity().digest()
        || session_id.source_digest() != locator.source().identity().digest()
        || event_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
        || session_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
    {
        return Err(IndexError::InvalidStoredDocumentField("native_locator"));
    }

    let provider = projected_text(document.provider, "provider")?;
    let source_format = projected_text(document.source_format, "source_format")?;
    if provider != locator.source().provider() || source_format != locator.source().source_format()
    {
        return Err(IndexError::InvalidStoredDocumentField("provider"));
    }

    Ok(VerificationRecord {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source_owner,
    })
}

fn projected_identity(
    identity: Option<VerificationStoredValue>,
    uuid: Option<VerificationStoredValue>,
    digest: Option<VerificationStoredValue>,
    expected_kind: StableEntityKind,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity = StableEntityId::decode_canonical(&projected_bytes(identity, field_name)?)?;
    let uuid = projected_text(uuid, field_name)?;
    let digest = projected_text(digest, field_name)?;
    if identity.entity_kind() != expected_kind
        || uuid != identity.as_uuid().to_string()
        || digest != hex(&identity.digest())
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn projected_session_identity(
    identity: Option<VerificationStoredValue>,
    uuid: Option<VerificationStoredValue>,
    field_name: &'static str,
) -> Result<StableEntityId> {
    let identity = StableEntityId::decode_canonical(&projected_bytes(identity, field_name)?)?;
    let uuid = projected_text(uuid, field_name)?;
    if identity.entity_kind() != StableEntityKind::Session || uuid != identity.as_uuid().to_string()
    {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(identity)
}

fn optional_projected_session_identity(
    identity: Option<VerificationStoredValue>,
    uuid: Option<VerificationStoredValue>,
    field_name: &'static str,
) -> Result<Option<StableEntityId>> {
    match (identity, uuid) {
        (None, None) => Ok(None),
        (Some(identity), Some(uuid)) => {
            projected_session_identity(Some(identity), Some(uuid), field_name).map(Some)
        }
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn projected_text(
    value: Option<VerificationStoredValue>,
    field_name: &'static str,
) -> Result<String> {
    match value {
        Some(VerificationStoredValue::Text(value)) if !value.is_empty() => Ok(value),
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}

fn projected_bytes(
    value: Option<VerificationStoredValue>,
    field_name: &'static str,
) -> Result<Vec<u8>> {
    match value {
        Some(VerificationStoredValue::Bytes(value)) => Ok(value),
        _ => Err(IndexError::InvalidStoredDocumentField(field_name)),
    }
}
