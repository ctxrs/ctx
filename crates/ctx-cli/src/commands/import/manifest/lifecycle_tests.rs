use super::*;
use crate::commands::import::{native::import_one_source_inner, SourcePreinventory};
use crate::provider_sources::explicit_path_source;
use ctx_history_capture::ProviderImportWorkResult;
use rusqlite::Connection;
use serde_json::json;

fn write_junie_prompt(path: &Path, prompt: &str) {
    fs::write(
        path,
        format!(
            "{}\n",
            json!({
                "kind": "UserPromptEvent",
                "timestampMs": 1_783_339_200_000_i64,
                "prompt": prompt,
            })
        ),
    )
    .unwrap();
}

#[test]
fn junie_tree_bypasses_per_file_manifest_and_reconciles_the_root_once() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("junie-sessions");
    let first_id = "session-260726-100000-first";
    let second_id = "session-260726-100001-second";
    for session_id in [first_id, second_id] {
        fs::create_dir_all(root.join(session_id)).unwrap();
        write_junie_prompt(
            &root.join(session_id).join("events.jsonl"),
            &format!("prompt for {session_id}"),
        );
    }
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": first_id,
                "taskName": "first task",
                "projectDir": "/workspace/first",
            }),
            json!({
                "sessionId": second_id,
                "taskName": "second task",
                "projectDir": "/workspace/second",
            }),
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Junie, root.clone());
    assert_eq!(source.source_format, "junie_session_events_jsonl_tree");
    assert!(!source_uses_import_file_manifest(&source));
    let database = temp.path().join("work.sqlite");
    let mut store = Store::open(&database).unwrap();

    let first =
        import_one_source_inner(&mut store, &source, None, false, &SourcePreinventory::None)
            .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 2);
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
    let first_session = store
        .session_by_external_session(CaptureProvider::Junie, first_id)
        .unwrap()
        .unwrap();
    let first_event = store.events_for_session(first_session.id).unwrap()[0].id;
    let observer = Connection::open(&database).unwrap();
    let before_data_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let before_database = fs::read(&database).unwrap();
    let wal_path = database.with_extension("sqlite-wal");
    let before_wal = fs::read(&wal_path).ok();

    let unchanged =
        import_one_source_inner(&mut store, &source, None, false, &SourcePreinventory::None)
            .unwrap();
    assert_eq!(unchanged.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(unchanged.skipped_sessions, 2);
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_data_version
    );
    assert_eq!(fs::read(&database).unwrap(), before_database);
    assert_eq!(fs::read(&wal_path).ok(), before_wal);

    fs::remove_file(root.join(first_id).join("events.jsonl")).unwrap();
    let deleted =
        import_one_source_inner(&mut store, &source, None, false, &SourcePreinventory::None)
            .unwrap();
    assert_eq!(deleted.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(first_event)
        .is_err());

    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": second_id,
                "taskName": "second task updated",
                "projectDir": "/workspace/second",
            }),
        ),
    )
    .unwrap();
    let metadata_update =
        import_one_source_inner(&mut store, &source, None, false, &SourcePreinventory::None)
            .unwrap();
    assert_eq!(
        metadata_update.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(metadata_update.imported_events, 0);
    let second_session = store
        .session_by_external_session(CaptureProvider::Junie, second_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        second_session.sync.metadata["metadata"]["title"],
        "second task updated"
    );
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
}

#[test]
fn manifest_inventory_persists_store_validated_control_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("claude-projects");
    fs::create_dir_all(&root).unwrap();
    let event_path = root.join("session.jsonl");
    fs::write(&event_path, b"{}\n").unwrap();
    let source = explicit_path_source(CaptureProvider::Claude, root.clone());
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
                CaptureProvider::Claude,
                provider_path_text(&root).unwrap(),
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn provider_owned_root_manifest_route_table_is_exact() {
    assert_eq!(
        PROVIDER_OWNED_ROOT_MANIFEST_ROUTES,
        [
            (CaptureProvider::Pi, "pi_session_jsonl"),
            (CaptureProvider::Junie, "junie_session_events_jsonl_tree",),
            (
                CaptureProvider::Antigravity,
                "antigravity_cli_transcript_jsonl_tree",
            ),
            (CaptureProvider::OpenHands, "openhands_file_events"),
            (CaptureProvider::RovoDev, "rovodev_session_json_tree"),
            (CaptureProvider::Mux, "mux_session_jsonl_tree"),
            (CaptureProvider::Auggie, "auggie_session_json"),
        ]
    );

    let temp = tempfile::tempdir().unwrap();
    for (provider, expected_format) in PROVIDER_OWNED_ROOT_MANIFEST_ROUTES {
        let root = temp.path().join(provider.as_str());
        fs::create_dir(&root).unwrap();
        let source = explicit_path_source(provider, root);
        assert_eq!(source.source_format, expected_format);
        assert!(provider_owns_root_manifest(&source));
        assert!(!source_uses_import_file_manifest(&source));
    }
}
