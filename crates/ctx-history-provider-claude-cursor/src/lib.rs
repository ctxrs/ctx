//! Claude Code and Cursor provider adapters for ctx agent history.
//!
//! This pack owns provider discovery, parsing, identity, checkpointing, and
//! projection. Capture supplies the concrete lifecycle binding and retains
//! registration, index lifecycle, and publication authority.

mod claude;
pub mod cursor;
mod raw_json;
use std::sync::Arc;

use ctx_history_jsonl::JsonlFamilyAdapter;
use ctx_history_provider_runtime::{ProviderJsonlRuntime, ProviderRuntimeBinding};

fn consume_neutral_preflight<E: ctx_history_jsonl::JsonlFamilyError>(
    reader: &mut ctx_history_jsonl::JsonlReader<E>,
) -> Result<(), E> {
    while reader
        .visit_page(&mut |_record| -> Result<(), E> { Ok(()) })?
        .is_some()
    {}
    Ok(())
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    pub mod cursor {
        use super::super::cursor::source_backed::source_key;
        use super::super::cursor::source_backed::{
            cursor_base_identity_probes, cursor_signature_records,
            reset_cursor_base_identity_probes, reset_cursor_projected_records,
            reset_cursor_signature_records, take_cursor_projected_records,
        };

        pub fn reset_projected_records(native_session_id: &str) {
            let source = source_key(native_session_id).expect("valid Cursor test session id");
            reset_cursor_projected_records(&source);
        }

        pub fn take_projected_records(native_session_id: &str) -> u64 {
            let source = source_key(native_session_id).expect("valid Cursor test session id");
            take_cursor_projected_records(&source)
        }

        pub fn reset_signature_records() {
            reset_cursor_signature_records();
        }

        pub fn signature_records() -> u64 {
            cursor_signature_records()
        }

        pub fn reset_base_identity_probes() {
            reset_cursor_base_identity_probes();
        }

        pub fn base_identity_probes() -> u64 {
            cursor_base_identity_probes()
        }
    }
}

pub use cursor::{discover_cursor_transcripts, CursorDiscoveryIssueKind};

const CLAUDE_PROJECTS_SOURCE_FORMAT: &str = "claude_projects_jsonl_tree";
const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl_tree";

pub fn claude_jsonl_adapter<B>() -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    claude::nativepath::source_backed::claude_jsonl_adapter::<B>()
}

pub fn claude_jsonl_adapter_for_named_home<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    claude::nativepath::source_backed::claude_jsonl_adapter_with_source_root_lineage::<B>(
        source_root_lineage,
    )
}

pub fn cursor_jsonl_adapter<B>() -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    cursor::cursor_jsonl_adapter::<B>()
}

#[cfg(test)]
mod neutral_preflight_tests {
    use super::*;
    use ctx_history_jsonl::{JsonlReader, JsonlSourceIdentity};
    use ctx_history_provider_runtime::source_io::OpenedProviderSourceFile;

    #[test]
    fn neutral_preflight_consumes_complete_framing_without_semantic_output() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("neutral-preflight.jsonl");
        let bytes = b"{\"message\":\"first\"}\nnot-json\n{\"message\":\"last\"}\n";
        std::fs::write(&path, bytes).unwrap();
        let source = Arc::new(OpenedProviderSourceFile::open(&path).unwrap());
        let identity = JsonlSourceIdentity::new(
            "neutral-test",
            "neutral-preflight-v1",
            "physical-only-v1",
            [1; 32],
            path,
        );
        let mut reader = JsonlReader::open(identity, source, None, None).unwrap();

        consume_neutral_preflight(&mut reader).unwrap();

        let checkpoint = reader.outcome().unwrap().checkpoint();
        assert!(checkpoint.terminal());
        assert_eq!(checkpoint.next_physical_ordinal(), 3);
        assert_eq!(checkpoint.complete_prefix_end(), bytes.len() as u64);
    }
}
