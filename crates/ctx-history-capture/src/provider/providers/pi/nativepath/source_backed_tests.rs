use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    LocatorRevisionPolicy,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::source_backed::{pi_header_probes, reset_pi_header_probes};
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
        provider: CaptureProvider::Pi,
        path: root.to_path_buf(),
        exists: true,
        source_format: "pi_session_jsonl",
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
fn shared_family_pi_noop_replacement_lineage_and_hydration_oracle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("session.jsonl");
    write_session(
        &transcript,
        "pi-child",
        Some("pi-parent"),
        &[
            message("message-1", "pi exact", 1),
            message("message-2", "pi response", 2),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    reset_pi_header_probes();
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 1);
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 3);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    reset_jsonl_family_work();
    reset_pi_header_probes();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 0);
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.role.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("user"), Some("user")]
    );
    assert!(events.iter().all(|event| {
        event.parent_session_id.is_some()
            && event.root_session_id == event.parent_session_id.unwrap()
            && event.agent_type == "subagent"
            && !event.is_primary
            && event.cwd.as_deref() == Some("/workspace/pi")
            && event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
            && event.locator.certified_source_revision_digest().is_none()
    }));
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    reset_jsonl_family_work();
    reset_pi_header_probes();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(pi_header_probes(), 0);
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![b"pi response".as_slice(), b"pi exact".as_slice()]
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
        "0a71688447ebb2963f3ab1dcebb9adf1eb59640ccf6445b1a3f633c6dd58371e"
    );

    let before = fs::read_to_string(&transcript).unwrap();
    let rewritten = before.replace("pi exact", "pi other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&transcript, rewritten).unwrap();
    reset_jsonl_family_work();
    reset_pi_header_probes();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 1);
    assert_eq!(
        jsonl_family_work().provider_projections,
        2,
        "same-length replacement visits each physical record once"
    );
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);
    let rewritten_events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(rewrite.sources[0].observation().source(), None, 10)
        .unwrap()
        .items;
    assert!(rewritten_events
        .iter()
        .any(|event| event.event_id == events[0].event_id));

    append_record(&transcript, &message("message-3", "pi growth", 3));
    reset_jsonl_family_work();
    reset_pi_header_probes();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 1);
    assert_eq!(
        jsonl_family_work().provider_projections,
        1,
        "certified Pi growth projects only the appended record"
    );
    assert_eq!(growth.sources[0].counts().indexed_documents, 3);
}

#[test]
fn shared_family_pi_indexes_complete_structured_body_beyond_16k() {
    const TAIL: &str = "pipostsixteenkilobytesentinel";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("session.jsonl");
    let structured_body = format!(
        r#"{{"arguments":{{"padding":"{}","tail":"{TAIL}"}},"tool":"write_file"}}"#,
        "x".repeat(17_000)
    );
    assert!(structured_body.find(TAIL).unwrap() > 16 * 1_024);
    write_session(
        &transcript,
        "pi-complete-body",
        None,
        &[message("message-complete-body", &structured_body, 1)],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let index = VerifiedIndex::open(&index_root).unwrap();
    let candidates = index.search_event_candidates(TAIL, 10).unwrap();
    assert_eq!(candidates.len(), 1);
    let source = receipt.sources[0].observation().source();
    let event = index
        .source_event_page(source, None, 10)
        .unwrap()
        .items
        .remove(0);
    assert_eq!(candidates[0].event.event_id, event.event_id);

    let request = EventHydrationRequest::new(event.event_id, event.locator).unwrap();
    let hydrated = registry
        .resolver_registry()
        .hydrate_event(&request)
        .unwrap();
    assert_eq!(hydrated.provider_bytes, structured_body.as_bytes());
    let structured: Value = serde_json::from_slice(&hydrated.provider_bytes).unwrap();
    assert_eq!(
        structured
            .pointer("/arguments/tail")
            .and_then(Value::as_str),
        Some(TAIL)
    );
}

#[test]
fn shared_family_pi_admits_a_later_header_and_counts_independent_rejections() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("mixed.jsonl");
    let records = [
        message("orphan", "must not be projected", 0).to_string(),
        header("pi-mixed", None).to_string(),
        message("message-1", "before malformed", 1).to_string(),
        "{\"type\":\"message\",\"message\":{\"content\":[".to_owned(),
        message("message-2", "after malformed", 2).to_string(),
    ];
    fs::write(&transcript, format!("{}\n", records.join("\n"))).unwrap();
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    reset_pi_header_probes();
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 2);
    assert_eq!(receipt.sources.len(), 1);
    let counts = receipt.sources[0].counts();
    assert_eq!(counts.complete_records, 5);
    assert_eq!(counts.retained_records, 2);
    assert_eq!(counts.rejected_records, 2);
    assert_eq!(counts.ignored_records, 1);
    assert_eq!(counts.indexed_documents, 2);

    let source = receipt.sources[0].observation().source();
    let events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 10)
        .unwrap()
        .items;
    assert_eq!(events.len(), 2);
    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap()
        .into_records();
    let bodies = hydrated
        .iter()
        .map(|record| record.provider_bytes.as_slice())
        .collect::<Vec<_>>();
    assert!(bodies.contains(&b"before malformed".as_slice()));
    assert!(bodies.contains(&b"after malformed".as_slice()));
    assert!(!bodies.contains(&b"must not be projected".as_slice()));

    reset_pi_header_probes();
    let second_index = temp.path().join("second-index");
    let cached_binding_cold =
        refresh_source_backed_generation(&second_index, &registry, writer_options()).unwrap();
    assert_eq!(pi_header_probes(), 0);
    assert_eq!(cached_binding_cold.sources[0].counts(), counts);
}

#[cfg(unix)]
#[test]
fn shared_family_pi_accepts_hardlinks_without_retaining_leaf_handles() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("first.jsonl");
    write_session(
        &transcript,
        "pi-hardlink",
        None,
        &[message("message-1", "hardlink body", 1)],
    );
    fs::hard_link(&transcript, root.join("second.jsonl")).unwrap();
    let registry = registry(&root);
    let result =
        refresh_source_backed_generation(temp.path().join("index"), &registry, writer_options())
            .unwrap();
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].counts().indexed_documents, 1);
}

#[test]
fn shared_family_pi_complete_deletion_and_missing_root_are_distinct() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("session.jsonl");
    write_session(
        &transcript,
        "pi-delete",
        None,
        &[message("message-1", "delete body", 1)],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);

    fs::remove_file(&transcript).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());

    fs::remove_dir_all(&root).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        deleted.commit.generation_id
    );
}

fn header(session_id: &str, parent_session_id: Option<&str>) -> Value {
    let mut value = json!({
        "type": "session",
        "id": session_id,
        "version": 3,
        "timestamp": "2026-07-28T12:00:00Z",
        "cwd": "/workspace/pi",
    });
    if let Some(parent_session_id) = parent_session_id {
        value["parentSession"] = json!(parent_session_id);
    }
    value
}

fn message(id: &str, content: &str, second: u64) -> Value {
    json!({
        "type": "message",
        "id": id,
        "parentId": null,
        "timestamp": format!("2026-07-28T12:00:{second:02}Z"),
        "message": {
            "role": "user",
            "content": content,
        },
    })
}

fn write_session(
    path: &Path,
    session_id: &str,
    parent_session_id: Option<&str>,
    messages: &[Value],
) {
    let mut records = vec![header(session_id, parent_session_id)];
    records.extend_from_slice(messages);
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_record(path: &PathBuf, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}
