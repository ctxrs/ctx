use std::collections::HashSet;

use ctx_history_core::CaptureProvider;

use super::super::{
    provider_import_revision, provider_source_specs, DEFAULT_PROVIDER_IMPORT_REVISION,
    PROVIDER_IMPORT_REVISIONS,
};

#[test]
fn import_revision_registry_covers_default_provider_formats_without_duplicates() {
    let mut keys = HashSet::new();
    for entry in PROVIDER_IMPORT_REVISIONS {
        assert!(entry.revision > 0);
        assert!(
            keys.insert((entry.provider, entry.source_format)),
            "duplicate import revision for {}/{}",
            entry.provider.as_str(),
            entry.source_format
        );
    }

    for spec in provider_source_specs() {
        for location in spec.default_locations {
            assert!(
                PROVIDER_IMPORT_REVISIONS.iter().any(|entry| {
                    entry.provider == spec.provider && entry.source_format == location.source_format
                }),
                "missing import revision for {}/{}",
                spec.provider.as_str(),
                location.source_format
            );
        }
    }
}

#[test]
fn semantic_output_changes_bump_only_the_affected_material_formats() {
    assert_eq!(
        provider_import_revision(CaptureProvider::Codex, "codex_session_jsonl_tree"),
        2
    );
    assert_eq!(
        provider_import_revision(CaptureProvider::Codex, "codex_session_jsonl"),
        2
    );
    assert_eq!(
        provider_import_revision(CaptureProvider::Tabnine, "tabnine_cli_chat_recording_jsonl"),
        2
    );

    assert_eq!(
        provider_import_revision(CaptureProvider::Codex, "codex_history_jsonl"),
        DEFAULT_PROVIDER_IMPORT_REVISION
    );
    assert_eq!(
        provider_import_revision(CaptureProvider::Claude, "claude_projects_jsonl_tree"),
        DEFAULT_PROVIDER_IMPORT_REVISION
    );
    assert_eq!(
        provider_import_revision(CaptureProvider::Pi, "pi_session_jsonl"),
        DEFAULT_PROVIDER_IMPORT_REVISION
    );
}
