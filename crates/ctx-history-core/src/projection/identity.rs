use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::errors::{
    encode_length_prefixed, validate_text, ProjectionContractError, ProjectionContractResult,
    MAX_LOGICAL_KIND_BYTES,
};
use super::native::{
    encode_native_item_key, encode_native_session_key, encode_subrecord_selector, encode_typed_key,
    NativeItemKey, NativeSessionKey, SubrecordSelector,
};
use super::source::{SourceAnchor, SourceKey};

pub const IDENTITY_VERSION: u16 = 1;
pub const STABLE_ENTITY_ID_CANONICAL_LEN: usize = 2 + 1 + 32 + 32 + 32 + 16;

const IDENTITY_DOMAIN: &[u8] = b"ctx.identity\0";
const ENTITY_SOURCE: u8 = 1;
const ENTITY_SESSION: u8 = 2;
const ENTITY_ITEM: u8 = 3;
pub(super) const STABLE_ENTITY_ID_KIND_OFFSET: usize = 2;
pub(super) const STABLE_ENTITY_ID_DIGEST_OFFSET: usize = STABLE_ENTITY_ID_KIND_OFFSET + 1;
pub(super) const STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET: usize = STABLE_ENTITY_ID_DIGEST_OFFSET + 32;
pub(super) const STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET: usize =
    STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET + 32;
pub(super) const STABLE_ENTITY_ID_UUID_OFFSET: usize =
    STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET + 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum StableEntityKind {
    Source = ENTITY_SOURCE,
    Session = ENTITY_SESSION,
    Event = ENTITY_ITEM,
}

/// Full identity equality uses the complete SHA-256 digest.
///
/// The UUIDv8 is a public compact representation. A registry must fail closed
/// if an existing UUID is ever observed with a different full digest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StableEntityId {
    pub(super) contract_version: u16,
    pub(super) entity_kind: StableEntityKind,
    pub(super) digest: [u8; 32],
    pub(super) source_digest: [u8; 32],
    pub(super) source_descriptor_digest: [u8; 32],
    pub(super) uuid: Uuid,
}

impl StableEntityId {
    pub const CANONICAL_LEN: usize = STABLE_ENTITY_ID_CANONICAL_LEN;

    pub fn contract_version(self) -> u16 {
        self.contract_version
    }

    pub fn entity_kind(self) -> StableEntityKind {
        self.entity_kind
    }

    pub fn digest(self) -> [u8; 32] {
        self.digest
    }

    pub fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    pub fn source_descriptor_digest(self) -> [u8; 32] {
        self.source_descriptor_digest
    }

    pub fn as_uuid(self) -> Uuid {
        self.uuid
    }

    /// Encodes every identity component in one canonical fixed-size layout.
    ///
    /// The layout is the big-endian contract version, entity kind, full
    /// digest, source digest, source descriptor digest, and compact UUID.
    pub fn encode_canonical(
        self,
    ) -> ProjectionContractResult<[u8; STABLE_ENTITY_ID_CANONICAL_LEN]> {
        self.validate_contract()?;
        let mut encoded = [0_u8; STABLE_ENTITY_ID_CANONICAL_LEN];
        encoded[..STABLE_ENTITY_ID_KIND_OFFSET]
            .copy_from_slice(&self.contract_version.to_be_bytes());
        encoded[STABLE_ENTITY_ID_KIND_OFFSET] = self.entity_kind as u8;
        encoded[STABLE_ENTITY_ID_DIGEST_OFFSET..STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET]
            .copy_from_slice(&self.digest);
        encoded[STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET
            ..STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET]
            .copy_from_slice(&self.source_digest);
        encoded[STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET..STABLE_ENTITY_ID_UUID_OFFSET]
            .copy_from_slice(&self.source_descriptor_digest);
        encoded[STABLE_ENTITY_ID_UUID_OFFSET..].copy_from_slice(self.uuid.as_bytes());
        Ok(encoded)
    }

    /// Decodes the canonical fixed-size layout and validates every identity
    /// invariant before returning a value.
    pub fn decode_canonical(encoded: &[u8]) -> ProjectionContractResult<Self> {
        if encoded.len() != STABLE_ENTITY_ID_CANONICAL_LEN {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        let contract_version = u16::from_be_bytes([encoded[0], encoded[1]]);
        let entity_kind = match encoded[STABLE_ENTITY_ID_KIND_OFFSET] {
            ENTITY_SOURCE => StableEntityKind::Source,
            ENTITY_SESSION => StableEntityKind::Session,
            ENTITY_ITEM => StableEntityKind::Event,
            _ => return Err(ProjectionContractError::InvalidDerivedIdentity),
        };
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(
            &encoded[STABLE_ENTITY_ID_DIGEST_OFFSET..STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET],
        );
        let mut source_digest = [0_u8; 32];
        source_digest.copy_from_slice(
            &encoded[STABLE_ENTITY_ID_SOURCE_DIGEST_OFFSET
                ..STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET],
        );
        let mut source_descriptor_digest = [0_u8; 32];
        source_descriptor_digest.copy_from_slice(
            &encoded
                [STABLE_ENTITY_ID_SOURCE_DESCRIPTOR_DIGEST_OFFSET..STABLE_ENTITY_ID_UUID_OFFSET],
        );
        let mut uuid = [0_u8; 16];
        uuid.copy_from_slice(&encoded[STABLE_ENTITY_ID_UUID_OFFSET..]);
        let identity = Self {
            contract_version,
            entity_kind,
            digest,
            source_digest,
            source_descriptor_digest,
            uuid: Uuid::from_bytes(uuid),
        };
        identity.validate_contract()?;
        Ok(identity)
    }

    pub fn validate_contract(self) -> ProjectionContractResult<()> {
        if self.contract_version != IDENTITY_VERSION {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        let mut uuid_bytes = [0_u8; 16];
        uuid_bytes.copy_from_slice(&self.digest[..16]);
        uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
        uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
        if Uuid::from_bytes(uuid_bytes) != self.uuid
            || (self.entity_kind == StableEntityKind::Source
                && (self.source_digest != self.digest || self.source_descriptor_digest != [0; 32]))
        {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        Ok(())
    }
}

impl PartialEq for StableEntityId {
    fn eq(&self, other: &Self) -> bool {
        self.contract_version == other.contract_version
            && self.entity_kind == other.entity_kind
            && self.digest == other.digest
    }
}

impl Eq for StableEntityId {}

impl Hash for StableEntityId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.contract_version.hash(state);
        self.entity_kind.hash(state);
        self.digest.hash(state);
    }
}

impl std::fmt::Display for StableEntityId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.uuid.fmt(formatter)
    }
}

pub struct SessionIdentityInput<'a> {
    pub source: &'a SourceKey,
    pub logical_session_kind: &'a str,
    pub native_session_key: &'a NativeSessionKey,
}

pub struct EventIdentityInput<'a> {
    pub source: &'a SourceKey,
    pub session_id: StableEntityId,
    pub logical_item_kind: &'a str,
    pub native_item_key: &'a NativeItemKey,
    pub subrecord_selector: Option<&'a SubrecordSelector>,
}

pub fn derive_session_id(
    input: SessionIdentityInput<'_>,
) -> ProjectionContractResult<StableEntityId> {
    input.source.validate_contract()?;
    validate_text(
        "logical_session_kind",
        input.logical_session_kind,
        MAX_LOGICAL_KIND_BYTES,
    )?;
    let mut fields = IdentityFields::new();
    fields.bytes(1, &input.source.identity.digest);
    fields.utf8(2, input.logical_session_kind);
    fields.native_session_key(3, input.native_session_key)?;
    derive_identity(StableEntityKind::Session, fields, Some(input.source))
}

pub fn derive_event_id(input: EventIdentityInput<'_>) -> ProjectionContractResult<StableEntityId> {
    input.source.validate_contract()?;
    validate_text(
        "logical_item_kind",
        input.logical_item_kind,
        MAX_LOGICAL_KIND_BYTES,
    )?;
    if input.session_id.entity_kind != StableEntityKind::Session {
        return Err(ProjectionContractError::EntityKindMismatch {
            expected: StableEntityKind::Session,
            actual: input.session_id.entity_kind,
        });
    }
    input.session_id.validate_contract()?;
    if input.session_id.source_digest != input.source.identity.digest {
        return Err(ProjectionContractError::SourceChanged);
    }
    if input.session_id.source_descriptor_digest != input.source.exact_descriptor_digest() {
        return Err(ProjectionContractError::SourceDescriptorChanged);
    }
    let mut fields = IdentityFields::new();
    fields.bytes(1, &input.source.identity.digest);
    fields.bytes(2, &input.session_id.digest);
    fields.utf8(3, input.logical_item_kind);
    fields.native_item_key(4, input.native_item_key)?;
    if let Some(subrecord) = input.subrecord_selector {
        fields.subrecord_selector(5, subrecord)?;
    }
    derive_identity(StableEntityKind::Event, fields, Some(input.source))
}

pub(super) fn derive_source_identity(
    provider: &str,
    anchor: &SourceAnchor,
) -> ProjectionContractResult<StableEntityId> {
    let mut fields = IdentityFields::new();
    fields.utf8(1, provider);
    fields.source_anchor(2, anchor)?;
    derive_identity(StableEntityKind::Source, fields, None)
}

fn derive_identity(
    entity_kind: StableEntityKind,
    fields: IdentityFields,
    source: Option<&SourceKey>,
) -> ProjectionContractResult<StableEntityId> {
    let field_count =
        u16::try_from(fields.values.len()).map_err(|_| ProjectionContractError::FieldTooLarge {
            field: "identity_field_count",
            actual: fields.values.len(),
            maximum: u16::MAX as usize,
        })?;
    let mut digest = Sha256::new();
    digest.update(IDENTITY_DOMAIN);
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update([entity_kind as u8]);
    digest.update(field_count.to_be_bytes());
    for (tag, value) in fields.values {
        digest.update(tag.to_be_bytes());
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    let digest: [u8; 32] = digest.finalize().into();
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let source_digest = source
        .map(|source| source.identity.digest)
        .unwrap_or(digest);
    let source_descriptor_digest = source
        .map(SourceKey::exact_descriptor_digest)
        .unwrap_or([0; 32]);
    Ok(StableEntityId {
        contract_version: IDENTITY_VERSION,
        entity_kind,
        digest,
        source_digest,
        source_descriptor_digest,
        uuid: Uuid::from_bytes(uuid_bytes),
    })
}

struct IdentityFields {
    values: Vec<(u16, Vec<u8>)>,
}

impl IdentityFields {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn push(&mut self, tag: u16, value: Vec<u8>) {
        debug_assert!(self.values.last().is_none_or(|(prior, _)| *prior < tag));
        self.values.push((tag, value));
    }

    fn bytes(&mut self, tag: u16, value: &[u8]) {
        self.push(tag, value.to_vec());
    }

    fn utf8(&mut self, tag: u16, value: &str) {
        self.push(tag, value.as_bytes().to_vec());
    }

    fn native_session_key(
        &mut self,
        tag: u16,
        key: &NativeSessionKey,
    ) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_session_key(&mut encoded, key)?;
        self.push(tag, encoded);
        Ok(())
    }

    fn source_anchor(&mut self, tag: u16, anchor: &SourceAnchor) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        match anchor {
            SourceAnchor::ProviderNative { namespace, key } => {
                encoded.push(1);
                encode_length_prefixed(&mut encoded, namespace.as_bytes());
                encode_typed_key(&mut encoded, key)?;
            }
            SourceAnchor::CatalogLineage(lineage) => {
                encoded.push(2);
                encoded.extend_from_slice(lineage);
            }
        }
        self.push(tag, encoded);
        Ok(())
    }

    fn native_item_key(&mut self, tag: u16, key: &NativeItemKey) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_item_key(&mut encoded, key)?;
        self.push(tag, encoded);
        Ok(())
    }

    fn subrecord_selector(
        &mut self,
        tag: u16,
        selector: &SubrecordSelector,
    ) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_subrecord_selector(&mut encoded, selector)?;
        self.push(tag, encoded);
        Ok(())
    }
}
