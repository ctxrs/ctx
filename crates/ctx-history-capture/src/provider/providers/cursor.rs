//! Provider-owned Cursor source-backed discovery, parsing, and exact hydration.

mod checkpoint;
mod layout;
mod parser;
mod projection;
mod source;
mod source_backed;

pub(crate) use layout::{
    discover_cursor_transcripts, CursorDiscoveryIssueKind, CursorTranscriptPath,
};
pub(crate) use parser::cursor_complete_content_message_record;
pub(crate) use projection::CursorNativeSession;
pub(crate) use source::{
    cursor_complete_content_source_from_admitted, cursor_complete_content_source_revision,
    freeze_cursor_source, scan_cursor_source, CursorSourceObservation,
};
pub(crate) use source_backed::{
    extract_cursor_source_backed_cold, hydrate_cursor_source_backed_message,
    CursorSourceBackedPage, CursorSourceBackedRecord, CursorSourceBackedSink,
    CursorSourceBackedSourcePlan, CursorSourceBackedTerminal,
};

#[cfg(test)]
pub(crate) use source_backed::{
    CURSOR_SOURCE_BACKED_PAGE_MAX_BYTES, CURSOR_SOURCE_BACKED_PAGE_MAX_ROWS,
};

#[cfg(test)]
mod source_backed_tests;
