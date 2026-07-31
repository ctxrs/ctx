use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use super::super::source_backed::{
    hydrate_gemini_source_backed_record, project_gemini_test_event, GeminiSourceBackedError,
};
use super::*;
use crate::{
    provider::source_backed::{
        family::jsonl::{jsonl_family_work, reset_jsonl_family_work, JsonlFamilyWork},
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};
use ctx_history_core::{
    AgentType, BatchHydrationRequest, CaptureProvider, ContentSourceResolver,
    EventHydrationRequest, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Gemini,
        path: root.to_path_buf(),
        exists: true,
        source_format: crate::GEMINI_CLI_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn shared_family_gemini_noop_replacement_and_grouped_hydration_oracle() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("shared-family-gemini", "main"),
            json!({
                "id": "gemini-user",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "gemini exact user"
            }),
            json!({
                "id": "gemini-assistant",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "gemini exact assistant"
            }),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 3);
    assert_eq!(cold.sources[0].counts().retained_records, 2);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    reset_gemini_parse_counters();
    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 3,
            leaf_opens: 2,
            provider_projections: 0,
        }
    );
    assert_eq!(
        gemini_parse_counters().0,
        3,
        "exact no-op reads one bounded identity header per capture and terminal inventory pass"
    );
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].provider_session_id.as_deref(),
        Some("shared-family-gemini")
    );
    assert_eq!(events[0].agent_type, AgentType::Primary.as_str());
    assert!(events[0].is_primary);
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();

    reset_jsonl_family_work();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"gemini exact assistant".as_slice(),
            b"gemini exact user".as_slice()
        ]
    );
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 0,
            leaf_opens: 1,
            provider_projections: 0,
        }
    );

    writeln!(
        OpenOptions::new().append(true).open(&path).unwrap(),
        "{}",
        json!({
            "id": "gemini-appended",
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "user",
            "content": "gemini replacement growth"
        })
    )
    .unwrap();
    reset_jsonl_family_work();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 3,
            leaf_opens: 2,
            provider_projections: 4,
        },
        "every changed Gemini leaf must receive one complete replacement scan"
    );
    assert_eq!(changed.sources[0].counts().indexed_documents, 3);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);

    let changed_index = VerifiedIndex::open(&index_root).unwrap();
    let changed_events = changed_index
        .source_event_page(changed.sources[0].observation().source(), None, 10)
        .unwrap()
        .items;
    for old in events {
        assert!(changed_events
            .iter()
            .any(|event| event.event_id == old.event_id && event.locator == old.locator));
    }
}

#[test]
fn shared_family_gemini_preserves_subagent_lineage_and_exact_locator() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = root.join("tmp/project/chats/root-thread/child-thread.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        jsonl(&[
            header("child-thread", "subagent"),
            json!({
                "id": "child-message",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": "child lineage sentinel"
            }),
        ]),
    )
    .unwrap();
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let document = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 10)
        .unwrap()
        .items
        .pop()
        .unwrap();

    let parent_session_id = document.parent_session_id.unwrap();
    assert_eq!(document.root_session_id, parent_session_id);
    assert_ne!(document.session_id, parent_session_id);
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some("child-thread")
    );
    assert_eq!(document.agent_type, AgentType::Subagent.as_str());
    assert!(!document.is_primary);
    assert_eq!(
        document.source_path.as_deref(),
        Some(path.to_str().unwrap())
    );
    assert_eq!(
        document.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    let NativeRecordCoordinate::Jsonl {
        physical_ordinal,
        native_session_key,
        native_event_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("expected a JSONL locator");
    };
    assert_eq!(*physical_ordinal, 1);
    assert_eq!(
        native_session_key,
        &Some(TypedKey::Utf8("child-thread".to_owned()))
    );
    assert_eq!(
        native_event_key,
        &Some(TypedKey::Utf8("child-message".to_owned()))
    );
}

#[test]
fn gemini_exact_jsonl_locator_reopens_original_record_after_append() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let exact_text = "Gemini snowman ☃, quote \"exact\", path C:\\tmp";
    let path = write_transcript(
        &root,
        &[
            header("source-backed-exact", "main"),
            json!({
                "id": "exact-message",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": exact_text
            }),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source_key = cold.sources[0].observation().source();
    let document = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source_key, None, 10)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let source = rediscover(&root, &path);

    let hydrated = hydrate_gemini_source_backed_record(&source, &document.locator).unwrap();
    assert_eq!(hydrated.provider_bytes, exact_text.as_bytes());
    let mut wrong_digest = *document.locator.record_digest();
    wrong_digest[0] ^= 1;
    let wrong_locator = SourceRecordLocator::new(
        document.locator.source().clone(),
        document.locator.coordinate().clone(),
        document.locator.revision_policy(),
        document.locator.certified_source_revision_digest().copied(),
        wrong_digest,
    )
    .unwrap();
    assert!(matches!(
        hydrate_gemini_source_backed_record(&source, &wrong_locator),
        Err(GeminiSourceBackedError::LocatorDigestMismatch)
    ));

    writeln!(
        OpenOptions::new().append(true).open(&path).unwrap(),
        "{}",
        json!({
            "id": "appended-message",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "gemini",
            "content": "later append"
        })
    )
    .unwrap();
    let appended_source = rediscover(&root, &path);
    let hydrated_after_append =
        hydrate_gemini_source_backed_record(&appended_source, &document.locator).unwrap();
    assert_eq!(hydrated_after_append.provider_bytes, exact_text.as_bytes());
}

#[test]
fn gemini_lexical_document_preserves_structured_tool_arguments_beyond_16k() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let structured_tail = format!("{}gemini-structured-tail", "argument-".repeat(2_100));
    let expected_args = json!({
        "path": "src/complete.rs",
        "content": structured_tail,
    });
    let path = write_transcript(
        &root,
        &[
            header("structured-source-backed", "main"),
            json!({
                "id": "structured-tool-call",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-structured",
                    "name": "write_file",
                    "args": expected_args,
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, mut events) = scan_collect(&source, None);
    assert_eq!(events.len(), 1);

    let document = project_gemini_test_event(&source, events.remove(0)).unwrap();
    let (tool_name, arguments) = document.body.split_once('\n').unwrap();
    assert_eq!(tool_name, "write_file");
    assert_eq!(
        serde_json::from_str::<Value>(arguments).unwrap(),
        expected_args
    );
    assert!(arguments.contains("gemini-structured-tail"));
    assert!(document.body.chars().count() > 16_384);
}

#[test]
fn gemini_route_uses_only_the_shared_jsonl_family_lifecycle() {
    let source = include_str!("../source_backed.rs");
    assert!(source.contains("jsonl_family_driver"));
    assert!(!source.contains("SourceBackedRouteDriver::new"));
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
}
