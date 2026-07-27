use super::*;
use crate::commands::import::native::import_one_source_without_search_refresh;
use crate::commands::import::SourcePreinventory;
use crate::provider_sources::explicit_path_source;
use ctx_history_capture::ProviderImportSummary;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::FileTimes;

fn write_event(path: &Path, id: &str, timestamp: &str, content: &str) {
    fs::write(
        path,
        json!({
            "id": id,
            "timestamp": timestamp,
            "source": "user",
            "llm_message": {"role": "user", "content": content},
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn manifest_inventory_persists_store_validated_control_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openhands");
    let conversation = root.join("v1_conversations/control-contract");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-message.json");
    write_event(
        &event_path,
        "control-contract-event",
        "2026-07-04T17:00:00Z",
        "control contract",
    );
    let source = explicit_path_source(CaptureProvider::OpenHands, root.clone());
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let inventory = inventory_source_import_files(&store, &source, false).unwrap();

    assert_eq!(inventory.files, 1);
    assert_eq!(inventory.bytes, fs::metadata(event_path).unwrap().len());
    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.pending, 1);
    assert_eq!(
        store
            .list_pending_source_import_files(
                CaptureProvider::OpenHands,
                provider_path_text(&root).unwrap(),
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn openhands_manifest_is_file_local_and_missing_history_remains_visible() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openhands");
    let conversation = root.join("v1_conversations/conversation-manifest");
    fs::create_dir_all(&conversation).unwrap();
    let first_path = conversation.join("0001-message.json");
    let second_path = conversation.join("0002-message.json");
    write_event(
        &first_path,
        "manifest-event-1",
        "2026-07-04T17:00:00Z",
        "first manifest event",
    );
    write_event(
        &second_path,
        "manifest-event-2",
        "2026-07-04T17:00:01Z",
        "second manifest event",
    );
    let source = explicit_path_source(CaptureProvider::OpenHands, root.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    let session_id = sessions[0].id;
    let original_session_metadata = sessions[0].sync.metadata["metadata"].clone();
    let original_events = store.events_for_session(session_id).unwrap();
    let original_second_payload = original_events
        .iter()
        .find(|event| event.payload["provider_event_hash"].as_str() == Some("manifest-event-2"))
        .map(|event| event.payload.clone())
        .unwrap();
    let original_ids = original_events
        .iter()
        .map(|event| event.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(original_ids.len(), 2);

    let unchanged = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(unchanged, ProviderImportSummary::default());

    let original_metadata = fs::metadata(&second_path).unwrap();
    let original_modified = original_metadata.modified().unwrap();
    let original_change_token = ctx_history_capture::observe_ordinary_file(&second_path)
        .unwrap()
        .token_hex();
    write_event(
        &second_path,
        "manifest-event-2",
        "2026-07-04T17:00:01Z",
        "edited manifest event",
    );
    assert_eq!(
        fs::metadata(&second_path).unwrap().len(),
        original_metadata.len()
    );
    fs::OpenOptions::new()
        .write(true)
        .open(&second_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified))
        .unwrap();
    assert_eq!(
        fs::metadata(&second_path).unwrap().modified().unwrap(),
        original_modified
    );
    assert_ne!(
        ctx_history_capture::observe_ordinary_file(&second_path)
            .unwrap()
            .token_hex(),
        original_change_token
    );
    let changed = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(changed.failed, 0, "{:?}", changed.failures);
    assert_eq!(changed.skipped_sessions, 1);
    assert_eq!(changed.skipped_events, 1);
    assert_eq!(
        store.get_session(session_id).unwrap().sync.metadata["metadata"],
        original_session_metadata
    );
    let after_change = store.events_for_session(session_id).unwrap();
    assert_eq!(
        after_change
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>(),
        original_ids
    );
    let stored_second_payload = after_change
        .iter()
        .find(|event| event.payload["provider_event_hash"].as_str() == Some("manifest-event-2"))
        .map(|event| event.payload.clone())
        .unwrap();
    assert_eq!(stored_second_payload, original_second_payload);
    let rendered_second = serde_json::to_string(&stored_second_payload).unwrap();
    assert!(rendered_second.contains("second manifest event"));
    assert!(!rendered_second.contains("edited manifest event"));

    fs::remove_file(&first_path).unwrap();
    let missing_once = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(missing_once, ProviderImportSummary::default());
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);

    let confirmed_missing = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(confirmed_missing, ProviderImportSummary::default());
    assert_eq!(store.source_import_file_counts().unwrap().stale, 1);
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
}

#[test]
fn openhands_manifest_indexes_certified_rejections_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openhands");
    let conversation = root.join("v1_conversations/conversation-rejections");
    fs::create_dir_all(&conversation).unwrap();
    write_event(
        &conversation.join("0001-valid.json"),
        "manifest-valid",
        "2026-07-04T17:00:00Z",
        "valid sibling",
    );
    fs::write(conversation.join("0002-malformed.json"), b"{not-json").unwrap();
    fs::File::create(conversation.join("0003-oversize.json"))
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let source = explicit_path_source(CaptureProvider::OpenHands, root);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 2, "{:?}", first.failures);
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("invalid OpenHands event JSON")));
    assert!(first
        .failures
        .iter()
        .any(|failure| failure.error.contains("exceeds the")));
    let indexed = store.source_import_file_counts().unwrap();
    assert_eq!(indexed.total, 3);
    assert_eq!(indexed.indexed, 3);
    assert_eq!(indexed.pending, 0);
    assert_eq!(indexed.failed, 0);

    let second = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    let third = import_one_source_without_search_refresh(
        &mut store,
        &source,
        None,
        false,
        &SourcePreinventory::None,
    )
    .unwrap();
    assert_eq!(second, ProviderImportSummary::default());
    assert_eq!(third, ProviderImportSummary::default());
    assert_eq!(store.source_import_file_counts().unwrap(), indexed);
}
