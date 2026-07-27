//! Provider-owned Cursor NativePath discovery and parsing.
//!
//! This module intentionally stops at provider-private publication rows. The
//! Store publication facade and public import routing are frozen elsewhere and
//! consume this slice directly through NativePath.

mod checkpoint;
mod layout;
mod parser;
mod projection;
mod source;

pub(crate) use checkpoint::CursorCheckpoint;
pub(crate) use layout::{
    discover_cursor_transcripts, CursorDiscoveryIssueKind, CursorTranscriptPath,
};
pub(crate) use parser::{
    cursor_complete_content_message_record, CursorRecordRejection, CursorRejectionKind,
};
pub(crate) use projection::{
    CursorEventBody, CursorNativeEvent, CursorNativeSession, CursorPublicationPage,
    CursorPublicationSink,
};
pub(crate) use source::{
    cursor_complete_content_source_from_admitted, cursor_complete_content_source_revision,
    freeze_cursor_source, resolve_cursor_missing_sources, scan_cursor_source_into,
    CursorCompletedExactInventory, CursorFrozenSource, CursorKnownSource,
    CursorMissingSourceDisposition, CursorPriorObservation, CursorReadOutcome,
    CursorSourceObservation,
};

#[cfg(test)]
pub(crate) use checkpoint::CursorCheckpointDisposition;
#[cfg(test)]
pub(crate) use projection::{CURSOR_PUBLICATION_PAGE_MAX_BYTES, CURSOR_PUBLICATION_PAGE_MAX_ROWS};
#[cfg(test)]
pub(crate) use source::{scan_cursor_source, CursorSourceGeneration, CursorSourceMutation};

#[cfg(test)]
mod tests;
