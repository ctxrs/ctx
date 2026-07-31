use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    AgentType, BatchHydrationRequest, CaptureProvider, ContentSourceResolver,
    EventHydrationRequest, HydrationFailureKind, NativeRecordCoordinate,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Junie,
        path: root.to_path_buf(),
        exists: true,
        source_format: "junie_session_events_jsonl_tree",
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

fn write_tree(root: &Path, session_id: &str, records: &[Value]) -> PathBuf {
    let session = root.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1_783_339_200_000_i64,
                "taskName": "Junie shared family fixture",
                "projectDir": "/workspace/junie",
            })
        ),
    )
    .unwrap();
    let path = session.join(RELATIVE_EVENTS_FILE);
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(&path, bytes).unwrap();
    path
}

fn records(prompt: &str) -> Vec<Value> {
    vec![
        json!({"kind": "UserPromptEvent", "prompt": prompt}),
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_200_001_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "a",
                "result": "first exact assistant block",
            }},
        }),
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_200_002_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "b",
                "result": "second exact assistant block",
            }},
        }),
    ]
}

#[test]
fn shared_family_junie_noop_one_pass_replacement_and_grouped_hydration_oracle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("junie");
    let transcript = write_tree(&root, "session-1", &records("junie exact"));
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 2);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| {
        event.agent_type == AgentType::Primary.as_str()
            && event.is_primary
            && event.workspace.as_deref() == Some("/workspace/junie")
            && event.cwd.as_deref() == Some("/workspace/junie")
            && event.locator.certified_source_revision_digest().is_some()
    }));
    assert!(matches!(
        events[0].locator.coordinate(),
        NativeRecordCoordinate::Jsonl { .. }
    ));
    assert!(matches!(
        events[1].locator.coordinate(),
        NativeRecordCoordinate::TreeRecord { .. }
    ));
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
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"first exact assistant block\n\nsecond exact assistant block".as_slice(),
            b"junie exact".as_slice(),
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
    let mut digest = Sha256::new();
    for (request, record) in requests.iter().zip(hydrated) {
        digest.update(request.event_id().digest());
        digest.update((record.provider_bytes.len() as u64).to_be_bytes());
        digest.update(record.provider_bytes);
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        "360858e3dd6113e216e24dfc2cc4a59ef3ae02750033affead49f0b3ebf909aa"
    );

    let before = fs::read_to_string(&transcript).unwrap();
    let rewritten = before.replace("junie exact", "junie other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&transcript, rewritten).unwrap();
    reset_jsonl_family_work();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "same-length rewrite must perform one full replacement pass"
    );
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(
        file,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "growth replacement"})
    )
    .unwrap();
    reset_jsonl_family_work();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        4,
        "growth must remain one full replacement pass"
    );
    assert_eq!(growth.sources[0].counts().indexed_documents, 3);
}

#[test]
fn shared_family_junie_retains_bodies_beyond_sixteen_kibibytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("junie");
    let prompt = format!("{} junie-prompt-tail", "prompt ".repeat(3_000));
    let response = format!("{} junie-response-tail", "response ".repeat(2_100));
    write_tree(
        &root,
        "session-long",
        &[
            json!({"kind": "UserPromptEvent", "prompt": prompt}),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_200_001_i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": "long-result",
                    "result": response,
                }},
            }),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);

    assert_eq!(events.len(), 2);
    assert_eq!(
        index
            .search_event_candidates("junie-prompt-tail", 10)
            .unwrap()[0]
            .event
            .event_id,
        events[0].event_id
    );
    assert_eq!(
        index
            .search_event_candidates("junie-response-tail", 10)
            .unwrap()[0]
            .event
            .event_id,
        events[1].event_id
    );
    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(hydrated[0].provider_bytes, prompt.as_bytes());
    assert_eq!(hydrated[1].provider_bytes, response.as_bytes());
    assert!(prompt.len() > 16 * 1024);
    assert!(response.len() > 16 * 1024);
}

#[test]
fn shared_family_junie_complete_deletion_and_missing_root_are_distinct() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("junie");
    write_tree(
        &root,
        "session-1",
        &[json!({"kind": "UserPromptEvent", "prompt": "present"})],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);

    fs::remove_dir_all(root.join("session-1")).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_ne!(deleted.commit.generation_id, cold.commit.generation_id);

    fs::remove_dir_all(&root).unwrap();
    assert!(
        refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err(),
        "a missing authority root is unavailable, not a deletion certificate"
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        deleted.commit.generation_id
    );
}

#[test]
fn shared_family_junie_over_limit_record_set_has_typed_hydration_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("junie");
    let records = (0..=super::super::projection::MAX_RECORD_SET_ENTRIES)
        .map(|index| {
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_200_000_i64 + index as i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": format!("{index:03}"),
                    "result": format!("bounded searchable part {index}"),
                }},
            })
        })
        .collect::<Vec<_>>();
    write_tree(&root, "session-1", &records);
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 10)
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert!(matches!(
        event.locator.coordinate(),
        NativeRecordCoordinate::ProviderNative { namespace, .. }
            if namespace == UNAVAILABLE_COORDINATE_NAMESPACE
    ));
    let failure = registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}
