use std::{fmt, sync::Arc};

use ctx_history_core::{CaptureProvider, ContentRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    locator::{
        CompleteContentBodyDigest, CompleteContentHashAuthority, CompleteContentSourceFamily,
        CompleteContentSourceLocator, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
    },
    registry::verified_content_route_matches,
    source_access::BrokeredSourceAccess,
};

/// One truncated canonical message selected by the Store/CLI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteMessageRequest {
    pub event_id: Uuid,
    pub provider: CaptureProvider,
    pub source_format: String,
    /// Opaque, read-only capability admitted from the Store-owned source route.
    /// Provider resolvers cannot reopen arbitrary paths.
    pub source_access: BrokeredSourceAccess,
    pub source_family: Option<CompleteContentSourceFamily>,
    pub content_profile: String,
    pub source_locator: Option<CompleteContentSourceLocator>,
    pub provider_session_id: Option<String>,
    pub source_record_ordinal: u64,
    pub source_record_subrecord_index: u32,
    pub expected_provider_event_hash: String,
    pub expected_hash_authority: CompleteContentHashAuthority,
    pub expected_native_record_id: Option<String>,
    pub expected_record_digest: Option<CompleteContentBodyDigest>,
    pub expected_content_ref: Option<ContentRef>,
    pub indexed_text: String,
    pub indexed_limit_chars: usize,
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
        if request
            .expected_content_ref
            .as_ref()
            .is_some_and(|expected| !expected.verifies(text.as_bytes()))
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::ContentVerificationFailed,
                request.event_id,
            ));
        }
        let body_sha256 = CompleteContentBodyDigest::from_text(&text);
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
        let Some(family) = first.source_family else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                first.event_id,
            ));
        };
        if requests.iter().any(|request| {
            request.source_family != Some(family)
                || !request.source_locator.as_ref().is_some_and(|locator| {
                    verified_content_route_matches(
                        &request.content_profile,
                        request.provider,
                        &request.source_format,
                        family,
                        VerifiedContentRole::MessageBody,
                        locator.kind(),
                    )
                })
        }) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                first.event_id,
            ));
        }
        if requests.iter().any(|request| {
            request.provider != first.provider
                || request.source_format != first.source_format
                || request.source_access != first.source_access
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
            family == resolver.family() && resolver.supports(first.provider, &first.source_format)
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
