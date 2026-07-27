//! Shared limits, hashing, and request-batch verification.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::complete_content::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    CompleteMessageRequest,
};

use super::source_access::{error, ContentErrorContext};

const STRUCTURED_MAX_FILES: usize = 4_096;
const STRUCTURED_MAX_DIRECTORY_DEPTH: usize = 12;
const STRUCTURED_MAX_JSON_ENTRIES: usize = 65_536;
const STRUCTURED_MAX_JSON_DEPTH: usize = 64;
const STRUCTURED_MAX_TOTAL_READ_BYTES: usize = 64 * 1024 * 1024;
pub(super) const STRUCTURED_MAX_COMPOUND_FILE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const STRUCTURED_MAX_NATIVE_ID_BYTES: usize = 1_024;
const STRUCTURED_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub(super) struct StructuredBounds {
    pub(super) max_files: usize,
    pub(super) max_depth: usize,
    pub(super) max_entries: usize,
    pub(super) max_json_depth: usize,
    pub(super) max_total_read_bytes: usize,
    pub(super) deadline: Duration,
}

impl Default for StructuredBounds {
    fn default() -> Self {
        Self {
            max_files: STRUCTURED_MAX_FILES,
            max_depth: STRUCTURED_MAX_DIRECTORY_DEPTH,
            max_entries: STRUCTURED_MAX_JSON_ENTRIES,
            max_json_depth: STRUCTURED_MAX_JSON_DEPTH,
            max_total_read_bytes: STRUCTURED_MAX_TOTAL_READ_BYTES,
            deadline: STRUCTURED_DEADLINE,
        }
    }
}
pub(super) fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn validate_request_batch(
    requests: &[CompleteMessageRequest],
) -> std::result::Result<(), CompleteContentError> {
    let first = &requests[0];
    let mut previous = None;
    for request in requests {
        let coordinate = (
            request.source_record_ordinal,
            request.source_record_subrecord_index,
        );
        if request.provider != first.provider
            || request.source_format != first.source_format
            || request.source_access != first.source_access
            || request.source_access.family() != CompleteContentSourceFamily::Structured
            || previous.is_some_and(|prior| prior >= coordinate)
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        previous = Some(coordinate);
    }
    Ok(())
}
pub(super) struct ResolutionBudget {
    pub(super) bounds: StructuredBounds,
    pub(super) deadline: Instant,
    files: usize,
    pub(super) entries: usize,
    pub(super) bytes: usize,
    pub(super) max_depth_seen: usize,
}

impl ResolutionBudget {
    pub(super) fn new(bounds: StructuredBounds, deadline: Instant) -> Self {
        Self {
            bounds,
            deadline,
            files: 0,
            entries: 0,
            bytes: 0,
            max_depth_seen: 0,
        }
    }

    pub(super) fn check(
        &self,
        request: &(impl ContentErrorContext + ?Sized),
    ) -> std::result::Result<(), CompleteContentError> {
        if Instant::now() > self.deadline {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
        Ok(())
    }

    pub(super) fn observe_file(
        &mut self,
        request: &(impl ContentErrorContext + ?Sized),
    ) -> std::result::Result<(), CompleteContentError> {
        self.check(request)?;
        self.files = self.files.saturating_add(1);
        if self.files > self.bounds.max_files {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        Ok(())
    }

    pub(super) fn observe_depth(
        &mut self,
        request: &(impl ContentErrorContext + ?Sized),
        depth: usize,
    ) -> std::result::Result<(), CompleteContentError> {
        self.max_depth_seen = self.max_depth_seen.max(depth);
        if depth > self.bounds.max_depth {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        self.check(request)
    }

    pub(super) fn observe_entries(
        &mut self,
        request: &(impl ContentErrorContext + ?Sized),
        count: usize,
    ) -> std::result::Result<(), CompleteContentError> {
        self.entries = self.entries.saturating_add(count);
        if self.entries > self.bounds.max_entries {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        self.check(request)
    }

    pub(super) fn observe_bytes(
        &mut self,
        request: &(impl ContentErrorContext + ?Sized),
        count: usize,
    ) -> std::result::Result<(), CompleteContentError> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > self.bounds.max_total_read_bytes {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        self.check(request)
    }
}
