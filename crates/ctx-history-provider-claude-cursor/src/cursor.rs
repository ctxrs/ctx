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
pub(crate) use source_backed::cursor_jsonl_adapter;

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
