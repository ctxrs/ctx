use super::*;
use crate::provider::source_backed::refresh_source_backed_generation;
use ctx_history_core::{ActivityTextCapture, CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use std::{fs, path::Path};

fn records(root: &Path, source: &ctx_history_core::SourceKey) -> Vec<CoreRecord> {
    let index = VerifiedIndex::open_pinned(root).unwrap();
    let page = index.core_source_event_page(source, None, 8).unwrap();
    let mut records: Vec<_> = page
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn registry(source_path: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_codex_explicit_session_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: source_path.to_path_buf(),
            exists: true,
            source_format: "codex_session_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    registry
}

#[test]
fn dual_item_ids_retain_tool_output_and_rewrite_removes_previous_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("rollout.jsonl");
    let index_root = temp.path().join("index");
    // Synthetic neutral input exercises the complete supported scanner and store.
    let header = r#"{"timestamp":"2026-08-01T00:00:00Z","type":"session_meta","payload":{"id":"019fb000-0000-7000-8000-0000000000a1","cwd":"/fixture"}}"#;
    let call = r#"{"timestamp":"2026-08-01T00:00:01Z","type":"response_item","payload":{"type":"function_call","id":"item-call","call_id":"shared-call","name":"exec_command","arguments":"{\"cmd\":\"echo sample\"}"}}"#;
    let output = r#"{"timestamp":"2026-08-01T00:00:02Z","type":"response_item","payload":{"type":"function_call_output","id":"item-output","call_id":"shared-call","output":"  complete result\nlast line\n"}}"#;
    let bytes = format!("{header}\n{call}\n{output}\n");
    fs::write(&source_path, &bytes).unwrap();
    let registry = registry(&source_path);
    let first =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(first.failed_routes.is_empty());
    assert_eq!(first.sources.len(), 1);
    let source = first.sources[0].observation().source();
    let initial = records(&index_root, source);
    assert_eq!(initial.len(), 2);
    assert_eq!(initial[0].event_type, "tool_call");
    assert_eq!(initial[1].event_type, "tool_output");
    for (record, item_id) in initial.iter().zip(["item-call", "item-output"]) {
        assert_eq!(
            record.parser_revision,
            "codex-nativepath-core-activity-v11-item-call-identity"
        );
        assert!(
            serde_json::to_string(record.native_event_id.as_ref().unwrap())
                .unwrap()
                .contains(item_id)
        );
        assert_eq!(
            record.content.activity.as_ref().unwrap().provider_call_id,
            Some(TypedKey::utf8("shared-call").unwrap())
        );
        assert!(record.event_copy.is_none());
    }
    assert_ne!(initial[0].event_id, initial[1].event_id);
    assert!(initial[0]
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .is_some());
    assert_eq!(
        initial[1].content.normalized_body.as_deref(),
        Some("  complete result\nlast line\n")
    );
    assert_eq!(
        initial[1]
            .content
            .activity
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .text,
        ActivityTextCapture::Present {
            value: "  complete result\nlast line\n".to_owned()
        }
    );
    let repeated =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(first.commit.generation_id, repeated.commit.generation_id);
    assert_eq!(initial, records(&index_root, source));
    assert_eq!(fs::read_to_string(&source_path).unwrap(), bytes);

    fs::write(
        &source_path,
        format!(
            "{header}\n{call}\n{}\n",
            output
                .replace("item-output", "replacement-output")
                .replace("complete result", "replacement result")
        ),
    )
    .unwrap();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(changed.failed_routes.is_empty());
    assert_ne!(changed.commit.generation_id, first.commit.generation_id);
    let current = records(&index_root, source);
    assert_eq!(current.len(), 2);
    assert_eq!(current[0].event_id, initial[0].event_id);
    assert_ne!(current[1].event_id, initial[1].event_id);
    assert!(VerifiedIndex::open_pinned(&index_root)
        .unwrap()
        .core_record_by_id(initial[1].event_id.as_uuid())
        .unwrap()
        .is_none());
    let repeated =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(changed.commit.generation_id, repeated.commit.generation_id);
}

#[test]
fn item_only_and_dual_id_calls_preserve_existing_pending_policy() {
    for (fields, pending) in [
        (serde_json::json!({"id":"shared-call"}), false),
        (
            serde_json::json!({"id":"item-call","call_id":"shared-call"}),
            false,
        ),
        (serde_json::json!({"call_id":"shared-call"}), true),
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("rollout.jsonl");
        let index_root = temp.path().join("index");
        let mut call = serde_json::json!({"type":"function_call", "name":"exec_command", "arguments":"{\"cmd\":\"ctx search example\"}"});
        call.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        let output = "Chunk ID: abc123\nWall time: 0.1 seconds\nProcess exited with code 0\nFinal output:\nsearch result";
        let values = [
            serde_json::json!({"type":"session_meta", "timestamp":"2026-08-01T00:00:00Z", "payload":{"id":"019fb000-0000-7000-8000-0000000000a1", "forked_from_id":"019fb000-0000-7000-8000-0000000000a0", "cwd":"/fixture"}}),
            serde_json::json!({"type":"response_item", "timestamp":"2026-08-01T00:00:01Z", "payload":call}),
            serde_json::json!({"type":"response_item", "timestamp":"2026-08-01T00:00:02Z", "payload":{"type":"function_call_output", "call_id":"shared-call", "output":output}}),
        ];
        fs::write(
            &source_path,
            values
                .iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let registry = registry(&source_path);
        let receipt =
            refresh_source_backed_generation(&index_root, &registry, WriterOptions::default())
                .unwrap();
        assert!(receipt.failed_routes.is_empty());
        let rows = records(&index_root, receipt.sources[0].observation().source());
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].event_type,
            if pending {
                "command_output"
            } else {
                "tool_output"
            }
        );
        assert_eq!(rows[1].event_copy.is_some(), pending);
        assert_eq!(rows[1].content.discovery_exclusion.is_some(), pending);
        assert_eq!(rows[1].content.normalized_body.as_deref(), Some(output));
        assert_eq!(
            rows[0].content.activity.as_ref().unwrap().provider_call_id,
            rows[1].content.activity.as_ref().unwrap().provider_call_id
        );
    }
}
