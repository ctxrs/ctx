//! Provider-owned Cursor source-backed discovery, parsing, and exact hydration.

mod layout;
mod parser;
mod projection;
mod source;
mod source_backed;

pub(crate) use layout::{discover_cursor_transcripts, CursorDiscoveryIssueKind};
pub(crate) use parser::cursor_complete_content_message_record;
pub(crate) use source::cursor_complete_content_source_from_admitted;
pub(crate) use source_backed::cursor_jsonl_adapter;

#[cfg(test)]
mod source_backed_tests;
