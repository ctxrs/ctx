use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    test_support_paths::tempdir,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn message(id: Option<&str>, text: &str) -> Value {
    let mut item = json!({
        "timestamp": "2026-07-28T12:00:00Z",
        "message": {
            "role": "assistant",
            "content": text,
        }
    });
    if let Some(id) = id {
        item.as_object_mut()
            .unwrap()
            .insert("id".to_owned(), Value::String(id.to_owned()));
    }
    item
}

fn tool_call(id: &str, secret_output: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-28T12:00:01Z",
        "message": {
            "role": "assistant",
            "content": "",
        },
        "toolCallStates": [{
            "toolCallId": format!("call-{id}"),
            "toolCall": {
                "id": format!("call-{id}"),
                "type": "function",
                "function": {
                    "name": "shell",
                    "arguments": "{\"command\":\"secret command\"}",
                }
            },
            "status": "done",
            "output": secret_output,
        }]
    })
}

fn session(session_id: &str, history: Vec<Value>) -> Value {
    json!({
        "sessionId": session_id,
        "title": format!("Session {session_id}"),
        "createdAt": "2026-07-28T12:00:00Z",
        "workspaceDirectory": "/workspace/continue",
        "history": history,
    })
}

fn write_session(root: &Path, name: &str, value: &Value) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(name);
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

fn route_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Continue,
        path: path.to_path_buf(),
        exists: true,
        source_format: CONTINUE_CLI_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn registry(path: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    ContinueSourceBackedReader::register(
        &mut registry,
        route_source(path),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn registry_with_adapter(
    path: &Path,
    adapter: ContinueSourceBackedReader,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        route_source(path),
        SourceBackedRouteSelection::Automatic,
        adapter,
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
fn cold_route_preserves_continue_projection_and_exact_hydration() {
    const SECRET: &str = "CONTINUE-SUCCESSFUL-OUTPUT-SECRET";

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let long_text = format!("{}continue-tail-term", "x".repeat(3_000));
    let source_path = write_session(
        &sessions,
        "primary.json",
        &session(
            "continue-primary",
            vec![
                message(Some("message-one"), &long_text),
                tool_call("tool-two", SECRET),
            ],
        ),
    );
    fs::write(
        sessions.join("sessions.json"),
        serde_json::to_vec(&json!([{
            "sessionId": "continue-primary",
            "title": "Indexed title",
        }]))
        .unwrap(),
    )
    .unwrap();
    let index_root = temp.path().join("index");
    let registry = registry(&sessions);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(receipt.sources.len(), 1);
    assert_eq!(
        receipt.sources[0].parser_revision(),
        CONTINUE_SOURCE_BACKED_PARSER_REVISION
    );
    assert_eq!(receipt.sources[0].counts().complete_records, 2);
    assert_eq!(receipt.sources[0].counts().retained_records, 2);
    assert_eq!(receipt.sources[0].counts().indexed_documents, 2);
    assert_eq!(
        receipt.sources[0].counts().certified_bytes,
        fs::metadata(&source_path).unwrap().len()
    );
    assert!(receipt.sources[0].frontier().is_some());

    let source = continue_source_key("continue-primary").unwrap();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| {
        event.parent_session_id.is_none()
            && event.root_session_id == event.session_id
            && event.provider_session_id.as_deref() == Some("continue-primary")
            && event.source_path.as_deref() == source_path.to_str()
            && event.workspace.as_deref() == Some("/workspace/continue")
    }));
    let NativeRecordCoordinate::Document {
        object_key,
        json_pointer,
    } = events[0].locator.coordinate()
    else {
        panic!("Continue locator lost its document coordinate");
    };
    assert_eq!(object_key, &TypedKey::utf8("continue-primary").unwrap());
    assert_eq!(json_pointer.as_deref(), Some("/history/0"));

    let request =
        EventHydrationRequest::new(events[0].event_id, events[0].locator.clone()).unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&request)
            .unwrap()
            .provider_bytes,
        long_text.as_bytes()
    );
    let tool = EventHydrationRequest::new(events[1].event_id, events[1].locator.clone()).unwrap();
    let hydrated = registry
        .resolver_registry()
        .hydrate_event(&tool)
        .unwrap()
        .provider_bytes;
    assert!(!String::from_utf8_lossy(&hydrated).contains(SECRET));
    assert!(String::from_utf8_lossy(&hydrated).contains("status: done"));
}

#[test]
fn route_replays_unchanged_replaces_one_leaf_deletes_and_retains_on_unavailable() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let first_path = write_session(
        &sessions,
        "first.json",
        &session(
            "continue-first",
            vec![message(Some("stable-first"), "before replacement")],
        ),
    );
    write_session(
        &sessions,
        "second.json",
        &session(
            "continue-second",
            vec![message(Some("stable-second"), "unchanged second")],
        ),
    );
    let index_root = temp.path().join("index");
    let parse_count = Arc::new(AtomicUsize::new(0));
    let adapter = ContinueSourceBackedReader::explicit(sessions.clone())
        .with_parse_count(Arc::clone(&parse_count));
    let registry = registry_with_adapter(&sessions, adapter);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);

    fs::write(
        &first_path,
        serde_json::to_vec(&session(
            "continue-first",
            vec![message(Some("stable-first"), "after replacement")],
        ))
        .unwrap(),
    )
    .unwrap();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    let first_source = continue_source_key("continue-first").unwrap();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&first_source, None, 4)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let changed_request =
        EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&changed_request)
            .unwrap()
            .provider_bytes,
        b"after replacement"
    );

    fs::remove_file(&first_path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    let retained_generation = deleted.commit.generation_id;

    fs::remove_file(sessions.join("second.json")).unwrap();
    fs::remove_dir(&sessions).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
}

#[test]
fn grouped_hydration_parses_exact_source_once_preserves_order_and_fails_atomically() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    write_session(
        &sessions,
        "primary.json",
        &session(
            "continue-primary",
            vec![
                message(Some("one"), "first"),
                message(Some("two"), "second"),
                message(Some("three"), "third"),
            ],
        ),
    );
    write_session(
        &sessions,
        "other.json",
        &session("continue-other", vec![message(Some("other"), "other")]),
    );
    let index_root = temp.path().join("index");
    let registry = registry(&sessions);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = continue_source_key("continue-primary").unwrap();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    let requests = [2_usize, 0, 1]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(events[index].event_id, events[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let mut parse_count = 0;
    let hydrated =
        hydrate_continue_group_with_observer(&sessions, &batch, || parse_count += 1).unwrap();
    assert_eq!(parse_count, 1);
    assert_eq!(
        hydrated
            .records()
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
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"third".as_slice(),
            b"first".as_slice(),
            b"second".as_slice()
        ]
    );

    let missing_locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::Document {
            object_key: TypedKey::utf8("continue-primary").unwrap(),
            json_pointer: Some("/history/99".to_owned()),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        events[0]
            .locator
            .certified_source_revision_digest()
            .copied(),
        *events[0].locator.record_digest(),
    )
    .unwrap();
    let missing = EventHydrationRequest::new(events[0].event_id, missing_locator).unwrap();
    let partly_valid = BatchHydrationRequest::new(vec![requests[0].clone(), missing]).unwrap();
    parse_count = 0;
    let error = hydrate_continue_group_with_observer(&sessions, &partly_valid, || parse_count += 1)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::MissingRecord);
    assert_eq!(parse_count, 1);
}

#[test]
fn final_inventory_revalidation_rejects_a_changed_leaf_before_commit() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let source_path = write_session(
        &sessions,
        "race.json",
        &session("continue-race", vec![message(Some("race"), "baseline")]),
    );
    let index_root = temp.path().join("index");
    let baseline_registry = registry(&sessions);
    let baseline =
        refresh_source_backed_generation(&index_root, &baseline_registry, writer_options())
            .unwrap();

    fs::write(
        &source_path,
        serde_json::to_vec(&session(
            "continue-race",
            vec![message(Some("race"), "candidate")],
        ))
        .unwrap(),
    )
    .unwrap();
    let replacement_path = source_path.clone();
    let after_scan = Arc::new(move || {
        fs::write(
            &replacement_path,
            serde_json::to_vec(&session(
                "continue-race",
                vec![message(Some("race"), "raced")],
            ))
            .unwrap(),
        )
        .unwrap();
    });
    let adapter =
        ContinueSourceBackedReader::explicit(sessions.clone()).with_after_scan(after_scan);
    let racing_registry = registry_with_adapter(&sessions, adapter);
    assert!(
        refresh_source_backed_generation(&index_root, &racing_registry, writer_options()).is_err()
    );
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        baseline.commit.generation_id
    );
}

#[test]
fn continue_document_route_has_one_lifecycle_and_no_spool_or_captured_driver() {
    let adapter = include_str!("source_backed.rs");
    let hydration = include_str!("source_backed/hydration.rs");
    let source = include_str!("source.rs");
    let inventory = include_str!("source/inventory.rs");
    let family = include_str!("../../../source_backed/family/document.rs");
    let registration = include_str!("../../../source_backed/registration/families/document.rs");
    for (name, production) in [
        ("adapter", adapter),
        ("hydration", hydration),
        ("source", source),
        ("inventory", inventory),
        ("family", family),
    ] {
        assert!(
            production.lines().count() < 1_000,
            "{name} production file exceeded the 1,000-line bound"
        );
    }
    assert_eq!(adapter.matches("parse_continue_source(").count(), 1);
    for forbidden in [
        "ContinuePathSpool",
        "ContinuePreparationStream",
        "CertifiedSource::certify",
        "Vec<LexicalDocument>",
        "Arc<OpenedProviderSourceFile>",
    ] {
        assert!(
            !adapter.contains(forbidden)
                && !hydration.contains(forbidden)
                && !source.contains(forbidden)
                && !inventory.contains(forbidden),
            "Continue production restored forbidden {forbidden}"
        );
    }
    let continue_registration = registration
        .split("pub(super) fn register_continue_route")
        .nth(1)
        .expect("Continue registration function exists");
    assert!(!continue_registration.contains("captured_route_driver"));
    assert!(continue_registration.contains("ContinueSourceBackedReader::register"));
}
