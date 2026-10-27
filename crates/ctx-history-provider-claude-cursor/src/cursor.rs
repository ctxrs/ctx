//! Provider-owned Cursor discovery, parsing, and complete Core projection.

mod layout;
#[cfg(test)]
mod layout_tests;
mod parser;
mod projection;
pub(crate) mod source_backed;

pub use layout::{
    discover_cursor_transcripts, probe_cursor_transcript_availability, CursorDiscoveryIssueKind,
    CursorTranscriptAvailability,
};
pub(crate) use source_backed::{
    cursor_jsonl_adapter, cursor_jsonl_adapter_with_source_root_lineage,
};

#[cfg(test)]
mod tests {
    #[test]
    fn self_contained_projection_has_no_read_time_body_gate() {
        let sources = [
            include_str!("cursor/parser.rs"),
            include_str!("cursor/projection.rs"),
            include_str!("cursor/source_backed.rs"),
        ]
        .join("\n");
        for removed in [
            concat!("Content", "Ref"),
            concat!("complete_content", "_ref"),
        ] {
            assert!(!sources.contains(removed), "found {removed}");
        }
        assert!(sources.contains("event.event_type == EventType::Message"));
    }
}

/// Native transcript projection for archival converters. Uses the same strict
/// admission and ambiguity checks as history import; does not carry timestamps
/// from earlier messages or manufacture missing native fields.
pub fn parse_transcript_record(
    bytes: &[u8],
) -> ctx_history_provider_runtime::Result<Option<Vec<CursorNativeEvent>>> {
    parser::project_cursor_jsonl_record(bytes, 0, 0, 0, bytes.len() as u64)
}
pub use projection::{CursorEventBody, CursorNativeEvent, CursorNativeOrder};

/// A prompt's own embedded clock, with no timestamp propagation to later events.
pub fn transcript_embedded_timestamp(
    event: &CursorNativeEvent,
) -> Option<chrono::DateTime<chrono::Utc>> {
    projection::embedded_cursor_timestamp(event)
}
