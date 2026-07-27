//! Message recovery for bounded structured JSON and compound trees.
//!
//! The facade keeps the provider-facing resolver and locator API small. The
//! implementation is divided by contract: locator construction/decoding,
//! message recovery, source admission, and shared bounds.

mod contracts;
mod message;
pub(super) mod source_access;
pub(super) mod verification;

pub(crate) use contracts::decode_structured_locator;
use verification::StructuredBounds;

pub const STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND: &str = "structured-message-v1";

/// Bounded resolver for single-JSON, one-record-file, and compound JSON trees.
#[derive(Debug, Default)]
pub struct StructuredCompleteContentResolver {
    bounds: StructuredBounds,
}

impl StructuredCompleteContentResolver {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_bounds(bounds: StructuredBounds) -> Self {
        Self { bounds }
    }
}

#[cfg(test)]
use super::{
    verified_content_profile, verified_content_route_supported, CompleteContentBodyDigest,
    CompleteContentError, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentResolver, CompleteContentSourceFamily, CompleteMessage, CompleteMessageRequest,
    VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_ROUTES,
};
#[cfg(test)]
use crate::provider::providers::openhands::decode_openhands_event;
#[cfg(test)]
use ctx_history_core::{CaptureProvider, EventType};
#[cfg(test)]
use verification::digest_bytes;

#[cfg(test)]
#[path = "structured/tests.rs"]
mod tests;
