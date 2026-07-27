use std::{fmt, io};

use ctx_history_core::ContentRef;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::registry::{
    decode_hex, encode_hex, valid_content_profile, valid_locator_token, valid_opaque_locator,
    verified_content_contract_exists,
};

/// Existing provider record-size ceiling. Hydration never expands it.
pub const COMPLETE_CONTENT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Canonical indexed-message prefix retained before complete-content hydration.
pub const COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS: usize = crate::PROVIDER_MAX_TEXT_CHARS;
/// Bounded native locator size for narrowly typed, provider-owned coordinates.
pub const COMPLETE_CONTENT_MAX_LOCATOR_BYTES: usize = 4 * 1024;
/// Bounded event-metadata envelope for all verified content addresses.
pub const VERIFIED_CONTENT_LOCATORS_MAX_BYTES: usize = 8 * 1024;
/// v0.26 supports at most one address for each of its two content roles.
pub const VERIFIED_CONTENT_LOCATORS_MAX_ENTRIES: usize = 2;
pub(super) const COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES: usize = 256;
const COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES: usize = 1024;
pub(super) const VERIFIED_CONTENT_PROFILE_MAX_BYTES: usize = 128;
/// Local-only, path-free addresses for verified content retained in provider sources.
pub const VERIFIED_CONTENT_LOCATORS_METADATA_KEY: &str = "verified_content_locators_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompleteContentSourceFamily {
    Jsonl,
    Structured,
    Sqlite,
    #[cfg(test)]
    Fixture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedContentRole {
    MessageBody,
    ResultBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteContentHashAuthority {
    ProviderSupplied,
    NormalizedPayloadFallback,
}

/// Optional source state observed by the importing catalog.
///
/// A resolver may admit append-only growth after verifying the addressed
/// record. It must not treat a matching path or timestamp as sufficient proof.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub size_bytes: Option<u64>,
    pub modified_at_ms: Option<i64>,
    pub sha256: Option<String>,
}

/// Optional bounded coordinates retained by a provider adapter.
///
/// This is deliberately not an arbitrary JSON envelope and is not a request to
/// add a global locator table. New and legacy rows may have no locator and use
/// their ordinal/subrecord coordinates instead.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteContentSourceLocator {
    kind: String,
    value: Vec<u8>,
}

impl CompleteContentSourceLocator {
    pub fn new(kind: impl Into<String>, value: Vec<u8>) -> Option<Self> {
        let kind = kind.into();
        if kind.is_empty()
            || kind.len() > COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES
            || value.is_empty()
            || value.len() > COMPLETE_CONTENT_MAX_LOCATOR_BYTES
        {
            return None;
        }
        Some(Self { kind, value })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

impl fmt::Debug for CompleteContentSourceLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteContentSourceLocator")
            .field("kind", &self.kind)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

/// SHA-256 of one logical complete message body.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CompleteContentBodyDigest(String);

impl CompleteContentBodyDigest {
    pub fn from_text(text: &str) -> Self {
        Self::from_bytes(text.as_bytes())
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CompleteContentBodyDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).ok_or_else(|| D::Error::custom("expected lowercase SHA-256 hex"))
    }
}

impl fmt::Debug for CompleteContentBodyDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CompleteContentBodyDigest(<sha256>)")
    }
}

/// One canonical, local-only, path-free address for verified provider content.
///
/// `content_profile` names the exact provider normalization contract. Callers
/// must validate it against provider, source format, family, and role before
/// opening a provider source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedContentLocatorV1 {
    content_role: VerifiedContentRole,
    content_profile: String,
    content_ref: ContentRef,
    source_family: CompleteContentSourceFamily,
    address_kind: String,
    address_value: String,
    native_record_id: String,
    record_sha256: CompleteContentBodyDigest,
}

impl VerifiedContentLocatorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role: VerifiedContentRole,
        content_profile: impl Into<String>,
        content_ref: ContentRef,
        family: CompleteContentSourceFamily,
        kind: impl Into<String>,
        value: &[u8],
        native_record_id: impl Into<String>,
        record_sha256: CompleteContentBodyDigest,
    ) -> Option<Self> {
        let content_profile = content_profile.into();
        let kind = kind.into();
        let native_record_id = native_record_id.into();
        if !valid_content_profile(&content_profile)
            || content_ref.byte_len() > COMPLETE_CONTENT_MAX_BODY_BYTES as u64
            || !valid_locator_token(&kind)
            || native_record_id.is_empty()
            || native_record_id.len() > COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES
            || native_record_id.chars().any(char::is_control)
            || value.is_empty()
            || value.len() > COMPLETE_CONTENT_MAX_LOCATOR_BYTES
            || !valid_opaque_locator(family, &kind, value)
            || !verified_content_contract_exists(&content_profile, role, family, &kind)
        {
            return None;
        }
        let locator = Self {
            content_role: role,
            content_profile,
            content_ref,
            source_family: family,
            address_kind: kind,
            address_value: encode_hex(value),
            native_record_id,
            record_sha256,
        };
        (serde_json::to_vec(&locator).ok()?.len() <= COMPLETE_CONTENT_MAX_LOCATOR_BYTES)
            .then_some(locator)
    }

    pub fn role(&self) -> VerifiedContentRole {
        self.content_role
    }

    pub fn content_profile(&self) -> &str {
        &self.content_profile
    }

    pub fn content_ref(&self) -> &ContentRef {
        &self.content_ref
    }

    pub fn family(&self) -> CompleteContentSourceFamily {
        self.source_family
    }

    pub fn kind(&self) -> &str {
        &self.address_kind
    }

    pub fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    pub fn record_sha256(&self) -> &CompleteContentBodyDigest {
        &self.record_sha256
    }

    pub fn source_locator(&self) -> Option<CompleteContentSourceLocator> {
        CompleteContentSourceLocator::new(&self.address_kind, decode_hex(&self.address_value)?)
    }

    fn is_valid(&self) -> bool {
        valid_content_profile(&self.content_profile)
            && self.content_ref.byte_len() <= COMPLETE_CONTENT_MAX_BODY_BYTES as u64
            && valid_locator_token(&self.address_kind)
            && !self.native_record_id.is_empty()
            && self.native_record_id.len() <= COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES
            && !self.native_record_id.chars().any(char::is_control)
            && decode_hex(&self.address_value).is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= COMPLETE_CONTENT_MAX_LOCATOR_BYTES
                    && valid_opaque_locator(self.source_family, &self.address_kind, &value)
            })
            && verified_content_contract_exists(
                &self.content_profile,
                self.content_role,
                self.source_family,
                &self.address_kind,
            )
            && serde_json::to_vec(self)
                .is_ok_and(|encoded| encoded.len() <= COMPLETE_CONTENT_MAX_LOCATOR_BYTES)
    }
}

/// Strict bounded wrapper for all local verified-content addresses on an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedContentLocatorsV1 {
    version: u32,
    locators: Vec<VerifiedContentLocatorV1>,
}

impl VerifiedContentLocatorsV1 {
    pub fn singleton(locator: VerifiedContentLocatorV1) -> Option<Self> {
        let collection = Self {
            version: 1,
            locators: vec![locator],
        };
        (serde_json::to_vec(&collection).ok()?.len() <= VERIFIED_CONTENT_LOCATORS_MAX_BYTES)
            .then_some(collection)
    }

    pub fn from_metadata_value(value: &serde_json::Value) -> Option<Self> {
        let mut counter = BoundedJsonCounter::new(VERIFIED_CONTENT_LOCATORS_MAX_BYTES);
        serde_json::to_writer(&mut counter, value).ok()?;
        let collection = serde_json::from_value::<Self>(value.clone()).ok()?;
        if collection.version != 1
            || collection.locators.is_empty()
            || collection.locators.len() > VERIFIED_CONTENT_LOCATORS_MAX_ENTRIES
            || collection
                .locators
                .iter()
                .any(|locator| !locator.is_valid())
            || collection
                .locators
                .windows(2)
                .any(|pair| pair[0].content_role >= pair[1].content_role)
            || serde_json::to_vec(&collection).ok()?.len() > VERIFIED_CONTENT_LOCATORS_MAX_BYTES
        {
            return None;
        }
        Some(collection)
    }

    pub fn to_metadata_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn locator(&self, role: VerifiedContentRole) -> Option<&VerifiedContentLocatorV1> {
        self.locators
            .iter()
            .find(|locator| locator.content_role == role)
    }

    pub fn insert(&mut self, locator: VerifiedContentLocatorV1) -> bool {
        if self.locator(locator.content_role).is_some()
            || self.locators.len() >= VERIFIED_CONTENT_LOCATORS_MAX_ENTRIES
        {
            return false;
        }
        let role = locator.content_role;
        self.locators.push(locator);
        self.locators.sort_by_key(VerifiedContentLocatorV1::role);
        if serde_json::to_vec(self)
            .is_ok_and(|encoded| encoded.len() <= VERIFIED_CONTENT_LOCATORS_MAX_BYTES)
        {
            true
        } else {
            self.locators.retain(|locator| locator.content_role != role);
            false
        }
    }
}

struct BoundedJsonCounter {
    bytes: usize,
    limit: usize,
}

impl BoundedJsonCounter {
    const fn new(limit: usize) -> Self {
        Self { bytes: 0, limit }
    }
}

impl io::Write for BoundedJsonCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "verified-content locator metadata exceeds its bound",
            ));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn attach_verified_content_locator(
    metadata: &mut serde_json::Value,
    locator: VerifiedContentLocatorV1,
) -> Option<()> {
    let object = metadata.as_object_mut()?;
    let collection = match object.get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY) {
        Some(value) => {
            let mut collection = VerifiedContentLocatorsV1::from_metadata_value(value)?;
            if collection.locator(locator.role()).is_some() || !collection.insert(locator) {
                return None;
            }
            collection
        }
        None => VerifiedContentLocatorsV1::singleton(locator)?,
    };
    object.insert(
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
        collection.to_metadata_value(),
    );
    Some(())
}
