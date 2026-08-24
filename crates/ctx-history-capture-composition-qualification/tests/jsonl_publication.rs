//! Cross-provider black-box qualification for JSONL capture publication.

use ctx_history_capture_composition::*;
use ctx_history_core::{CoreRecord, LiteralFactKind};

#[path = "jsonl_publication/claude_cursor.rs"]
mod claude_cursor;
#[path = "jsonl_publication/gemini_retrieval_exclusion.rs"]
mod gemini_retrieval_exclusion;
#[path = "jsonl_publication/jsonl_shared_publication.rs"]
mod jsonl_shared_publication;
#[path = "jsonl_publication/mux_publication.rs"]
mod mux_publication;
#[path = "jsonl_publication/openclaw_sqlite.rs"]
mod openclaw_sqlite;
#[path = "jsonl_publication/ordinary_projector_liveness.rs"]
mod ordinary_projector_liveness;

fn has_literal_fact(record: &CoreRecord, kind: LiteralFactKind, value: &str) -> bool {
    record
        .content
        .activity
        .iter()
        .flat_map(|activity| activity.facts.iter())
        .any(|fact| fact.kind == kind && fact.value == value)
}

fn test_provider_probes() -> StaticProviderProbeCatalog {
    use ctx_history_source_discovery::{CursorProbeFragment, CursorTranscriptProbeOutcome};

    fn cursor(_: &std::path::Path) -> CursorTranscriptProbeOutcome {
        CursorTranscriptProbeOutcome::NotFound
    }

    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor))
}

mod test_support_paths {
    use std::{fs, io};

    pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
        let temp_root = fs::canonicalize(std::env::temp_dir())?;
        tempfile::Builder::new()
            .prefix("ctx-history-capture-qualification-")
            .tempdir_in(temp_root)
    }
}

mod provider {
    pub(crate) mod source_backed {
        pub(crate) use ctx_history_capture_composition::*;

        pub(crate) fn assert_carried_route_failure(
            receipt: &SourceBackedRefreshReceipt,
            retained_generation: &str,
            class: SourceBackedSourceFailureClass,
        ) {
            assert_eq!(receipt.commit.generation_id, retained_generation);
            assert!(receipt.successful_route_ids.is_empty());
            assert_eq!(receipt.failed_routes.len(), 1);
            let failure = &receipt.failed_routes[0];
            assert_eq!(failure.class, class);
            assert!(failure.carried_forward);
            assert_eq!(
                receipt.carried_failed_route_ids,
                vec![failure.route_identity.clone()]
            );
        }

        pub(crate) mod family {
            pub(crate) mod jsonl {
                pub(crate) use ctx_history_provider_runtime::set_after_jsonl_semantic_preflight_hook;
            }
        }
    }
}
