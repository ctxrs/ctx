use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, CertifiedProviderCursor,
};
use crate::test_support_paths::tempdir;
use crate::{
    CaptureWorkLimit, NormalizedProviderImportOptions, ProviderAdapterContext,
    ROVODEV_SOURCE_FORMAT,
};

use super::import_rovodev_sessions_batched;
use super::whole_json::ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS;

fn test_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "rovodev-batch-test-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn test_options() -> NormalizedProviderImportOptions {
    NormalizedProviderImportOptions {
        fast_event_inserts: true,
        capture_work_limit: CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        ..NormalizedProviderImportOptions::default()
    }
}

fn write_session(root: &Path, session_id: &str, context: Value) -> PathBuf {
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("session_context.json"),
        serde_json::to_vec(&context).unwrap(),
    )
    .unwrap();
    session_dir
}

#[test]
fn certified_replay_uses_identical_machine_and_source_identity() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(
        &root,
        "replay-session",
        json!({
            "session_id": "replay-session",
            "message_history": [{"role": "user", "content": "stable replay oracle"}]
        }),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first =
        import_rovodev_sessions_batched(&root, &mut store, context.clone(), options.clone())
            .unwrap();
    let replay = import_rovodev_sessions_batched(&root, &mut store, context, options).unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 1);
}

#[test]
fn changed_source_resets_with_identical_machine_and_source_identity() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_dir = write_session(
        &root,
        "changed-session",
        json!({
            "session_id": "changed-session",
            "message_history": [{"role": "user", "content": "before source change"}]
        }),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    import_rovodev_sessions_batched(&root, &mut store, context.clone(), options.clone()).unwrap();
    fs::write(
        session_dir.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": "changed-session",
            "message_history": [
                {"role": "user", "content": "after source change first"},
                {"role": "assistant", "content": "after source change second oracle"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let changed = import_rovodev_sessions_batched(&root, &mut store, context, options).unwrap();

    assert_eq!(changed.failed, 0, "{:?}", changed.failures);
    assert!(changed.imported_events > 0);
    assert!(store
        .search_event_hits("after source change second oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
}

#[test]
fn certified_rejection_replay_preserves_failed_diagnostics() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_dir = write_session(
        &root,
        "rejected-session",
        json!({
            "session_id": "rejected-session",
            "message_history": {"role": "user", "content": "not an array"}
        }),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first =
        import_rovodev_sessions_batched(&root, &mut store, context.clone(), options.clone())
            .unwrap();

    assert_eq!(first.failed, 1, "{:?}", first.failures);
    let context_path = fs::canonicalize(session_dir.join("session_context.json")).unwrap();
    let path_identity = provider_path_identity(&context_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        &path_identity,
    );
    let mut stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&stored.cursor).unwrap();
    assert_eq!(certified.rejected_records(), 1);
    stored.cursor = certified.with_rejected_records(2).encode().unwrap();
    store.upsert_sync_cursor(&stored).unwrap();

    let replay = import_rovodev_sessions_batched(&root, &mut store, context, options).unwrap();

    assert_eq!(replay.failed, 2);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert!(replay.failures[0]
        .error
        .contains("missing message_history array"));
}

#[test]
fn over_budget_whole_json_fails_closed_without_partial_projection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(
        &root,
        "over-budget-session",
        json!({
            "session_id": "over-budget-session",
            "message_history": [{
                "role": "user",
                "content": "must not be projected before the budget rejection"
            }],
            "adversarial": vec![
                Value::Null;
                ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS
            ]
        }),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_rovodev_sessions_batched(&root, &mut store, test_context(&root), test_options())
            .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("collection element budget"));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("must not be projected", 10)
        .unwrap()
        .is_empty());
}
