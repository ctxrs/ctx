//! Bounded, read-only recovery of complete message bodies from provider sources.
//!
//! The Store and CLI own selection and policy. Provider-family implementations
//! own native source parsing. Complete-message batches are all-or-nothing;
//! result-content batches may omit individually unverifiable records after the
//! shared source itself has been opened and frozen successfully.

use std::{fmt, path::PathBuf, sync::Arc};

use ctx_history_core::{CaptureProvider, ContentRef};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub mod jsonl;
pub mod sqlite;
pub mod structured;

/// Existing provider record-size ceiling. Hydration never expands it.
pub const COMPLETE_CONTENT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Bounded native locator size for narrowly typed, provider-owned coordinates.
pub const COMPLETE_CONTENT_MAX_LOCATOR_BYTES: usize = 4 * 1024;
const COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES: usize = 256;
const COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES: usize = 1024;
pub const COMPLETE_CONTENT_LOCATOR_METADATA_KEY: &str = "complete_content_locator_v1";
/// Local-only source locator for a normalized command/tool result body.
pub const RESULT_CONTENT_LOCATOR_METADATA_KEY: &str = "result_content_locator_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompleteContentSourceFamily {
    Jsonl,
    Structured,
    Sqlite,
    Fixture,
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
        Self(format!("{:x}", Sha256::digest(text.as_bytes())))
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

/// Canonical, local-only event metadata persisted for an eligible truncated
/// message. This schema is versioned, deny-unknown, and bounded after encoding.
/// It is intentionally excluded from external projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedCompleteContentLocatorV1 {
    version: u32,
    family: CompleteContentSourceFamily,
    kind: String,
    value_hex: String,
    native_record_id: String,
    record_sha256: CompleteContentBodyDigest,
    body_sha256: CompleteContentBodyDigest,
}

impl PersistedCompleteContentLocatorV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        family: CompleteContentSourceFamily,
        kind: impl Into<String>,
        value: &[u8],
        native_record_id: impl Into<String>,
        record_sha256: CompleteContentBodyDigest,
        body_sha256: CompleteContentBodyDigest,
    ) -> Option<Self> {
        let kind = kind.into();
        let native_record_id = native_record_id.into();
        if !valid_locator_token(&kind)
            || native_record_id.is_empty()
            || native_record_id.len() > COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES
            || value.is_empty()
            || value.len() > COMPLETE_CONTENT_MAX_LOCATOR_BYTES
        {
            return None;
        }
        let locator = Self {
            version: 1,
            family,
            kind,
            value_hex: encode_hex(value),
            native_record_id,
            record_sha256,
            body_sha256,
        };
        (serde_json::to_vec(&locator).ok()?.len() <= COMPLETE_CONTENT_MAX_LOCATOR_BYTES)
            .then_some(locator)
    }

    pub fn from_metadata_value(value: &serde_json::Value) -> Option<Self> {
        let locator = serde_json::from_value::<Self>(value.clone()).ok()?;
        if locator.version != 1
            || !valid_locator_token(&locator.kind)
            || locator.native_record_id.is_empty()
            || locator.native_record_id.len() > COMPLETE_CONTENT_MAX_NATIVE_RECORD_ID_BYTES
            || serde_json::to_vec(&locator).ok()?.len() > COMPLETE_CONTENT_MAX_LOCATOR_BYTES
            || decode_hex(&locator.value_hex).is_none()
        {
            return None;
        }
        Some(locator)
    }

    pub fn to_metadata_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn family(&self) -> CompleteContentSourceFamily {
        self.family
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    pub fn record_sha256(&self) -> &CompleteContentBodyDigest {
        &self.record_sha256
    }

    pub fn body_sha256(&self) -> &CompleteContentBodyDigest {
        &self.body_sha256
    }

    pub fn source_locator(&self) -> Option<CompleteContentSourceLocator> {
        CompleteContentSourceLocator::new(&self.kind, decode_hex(&self.value_hex)?)
    }
}

fn valid_locator_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn encode_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}

/// One truncated canonical message selected by the Store/CLI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteMessageRequest {
    pub event_id: Uuid,
    pub provider: CaptureProvider,
    pub source_format: String,
    /// Exact current selected provider source. Request construction fails when
    /// no safe local path is available.
    pub raw_source_path: PathBuf,
    pub source_root: Option<PathBuf>,
    pub source_identity: Option<String>,
    pub source_family: Option<CompleteContentSourceFamily>,
    pub source_locator: Option<CompleteContentSourceLocator>,
    pub source_snapshot: SourceSnapshot,
    pub provider_session_id: Option<String>,
    pub source_record_ordinal: u64,
    pub source_record_subrecord_index: u32,
    pub expected_provider_event_hash: String,
    pub expected_hash_authority: CompleteContentHashAuthority,
    pub expected_native_record_id: Option<String>,
    pub expected_record_digest: Option<CompleteContentBodyDigest>,
    pub expected_body_digest: Option<CompleteContentBodyDigest>,
    pub indexed_text: String,
    pub indexed_limit_chars: usize,
}

/// Local source coordinates required to re-read one normalized result body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultContentRequest {
    pub event_id: Uuid,
    pub provider: CaptureProvider,
    pub source_format: String,
    pub raw_source_path: PathBuf,
    pub source_root: Option<PathBuf>,
    pub source_identity: Option<String>,
    pub source_locator: CompleteContentSourceLocator,
    pub source_snapshot: SourceSnapshot,
    pub source_record_ordinal: u64,
    pub source_record_subrecord_index: u32,
    pub expected_record_digest: CompleteContentBodyDigest,
    pub expected_content_ref: ContentRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResultContent {
    pub event_id: Uuid,
    pub content: String,
    pub content_ref: ContentRef,
    pub verification: SourceVerification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceVerification {
    pub source_identity_verified: bool,
    pub source_snapshot_verified: bool,
    pub record_identity_verified: bool,
}

impl SourceVerification {
    pub const VERIFIED: Self = Self {
        source_identity_verified: true,
        source_snapshot_verified: true,
        record_identity_verified: true,
    };

    pub fn is_verified(self) -> bool {
        self.source_identity_verified
            && self.source_snapshot_verified
            && self.record_identity_verified
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteMessage {
    pub event_id: Uuid,
    pub text: String,
    pub body_sha256: CompleteContentBodyDigest,
    pub verification: SourceVerification,
}

impl CompleteMessage {
    /// Applies provider-independent body bounds and indexed-prefix/digest
    /// verification after a family resolver has verified native identity.
    pub fn verified(
        request: &CompleteMessageRequest,
        text: String,
        verification: SourceVerification,
    ) -> Result<Self, CompleteContentError> {
        if text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentTooLarge,
                request.event_id,
            ));
        }
        if !verification.is_verified() {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        let indexed_prefix = text
            .chars()
            .take(request.indexed_limit_chars)
            .collect::<String>();
        if indexed_prefix != request.indexed_text
            || text.chars().count() <= request.indexed_limit_chars
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        let body_sha256 = CompleteContentBodyDigest::from_text(&text);
        if request
            .expected_body_digest
            .as_ref()
            .is_some_and(|expected| expected != &body_sha256)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        Ok(Self {
            event_id: request.event_id,
            text,
            body_sha256,
            verification,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteContentErrorKind {
    SourceMissing,
    SourceUnreadable,
    SourceChanged,
    HydrationUnsupported,
    SourceRecordMissing,
    ContentTooLarge,
    ContentVerificationFailed,
}

impl CompleteContentErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceUnreadable => "source_unreadable",
            Self::SourceChanged => "source_changed",
            Self::HydrationUnsupported => "hydration_unsupported",
            Self::SourceRecordMissing => "source_record_missing",
            Self::ContentTooLarge => "content_too_large",
            Self::ContentVerificationFailed => "content_verification_failed",
        }
    }

    pub const fn retryable(self) -> bool {
        matches!(self, Self::SourceUnreadable | Self::SourceChanged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteContentError {
    pub kind: CompleteContentErrorKind,
    pub event_id: Uuid,
    pub retryable: bool,
}

impl CompleteContentError {
    pub fn new(kind: CompleteContentErrorKind, event_id: Uuid) -> Self {
        Self {
            kind,
            event_id,
            retryable: kind.retryable(),
        }
    }
}

impl fmt::Display for CompleteContentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "complete content failed for ctx event {}: {}; inspect with `ctx locate event {}`",
            self.event_id,
            self.kind.as_str(),
            self.event_id
        )
    }
}

impl std::error::Error for CompleteContentError {}

/// Provider-family boundary. One call contains requests for exactly one source,
/// ordered by source record ordinal/subrecord, and succeeds atomically.
pub trait CompleteContentResolver: Send + Sync {
    fn family(&self) -> CompleteContentSourceFamily;

    fn supports(&self, provider: CaptureProvider, source_format: &str) -> bool;

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError>;
}

#[derive(Default)]
pub struct CompleteContentResolverRegistry {
    resolvers: Vec<Arc<dyn CompleteContentResolver>>,
}

impl CompleteContentResolverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<R>(&mut self, resolver: R)
    where
        R: CompleteContentResolver + 'static,
    {
        self.resolvers.push(Arc::new(resolver));
    }

    pub fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        if requests.iter().any(|request| {
            request.provider != first.provider
                || request.source_format != first.source_format
                || request.raw_source_path != first.raw_source_path
                || request.source_family != first.source_family
        }) || requests.windows(2).any(|requests| {
            (
                requests[0].source_record_ordinal,
                requests[0].source_record_subrecord_index,
            ) > (
                requests[1].source_record_ordinal,
                requests[1].source_record_subrecord_index,
            )
        }) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                first.event_id,
            ));
        }
        let Some(resolver) = self.resolvers.iter().find(|resolver| {
            first
                .source_family
                .map(|family| family == resolver.family())
                .unwrap_or(true)
                && resolver.supports(first.provider, &first.source_format)
        }) else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                first.event_id,
            ));
        };
        let messages = resolver.resolve(requests)?;
        if messages.len() != requests.len()
            || messages
                .iter()
                .zip(requests)
                .any(|(message, request)| message.event_id != request.event_id)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                first.event_id,
            ));
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests;
